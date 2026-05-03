// App state and update logic.

use std::collections::BTreeSet;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::style::Color;

use crate::arcstats::ArcStats;
use crate::datasets::{DatasetNode, DatasetsSource};
use crate::meminfo::{MemSnapshot, MemSource, RamSegment};
use crate::pools::{PoolInfo, PoolsSource};

/// Top-level navigation tab. v0.3 ships four tabs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Pools,
    Datasets,
    Arc,
}

/// State of the Pools tab: either the list view with a selected row, or the
/// detail view drilldown for a specific pool by index into the snapshot.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PoolsView {
    List { selected: usize },
    Detail { pool_index: usize },
}

/// State of the Datasets tab: tree view with an expansion set + a
/// selection index over the visible (post-flatten) rows, or a per-
/// dataset detail drilldown identified by full ZFS name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatasetsView {
    Tree {
        /// Full ZFS names of dataset rows whose children are currently
        /// shown. Initialized at construction with every pool root so
        /// the landing screen matches the "pools auto-expanded one
        /// level" rule. Mutated by `←` / `→` keystrokes and by
        /// `refresh_datasets` (only to remove names that no longer
        /// exist in the snapshot — never to add new entries on
        /// refresh).
        expanded: BTreeSet<String>,
        /// Index into the *visible* (post-flatten) row list. Reclamped
        /// on every refresh and on collapse.
        selected: usize,
    },
    Detail {
        /// Full ZFS name of the dataset whose detail is being shown.
        /// Stored by name (not index) so concurrent refreshes that
        /// reorder or insert siblings don't snap the view to the
        /// wrong dataset.
        name: String,
        /// Cached `expanded` set so returning to Tree restores the
        /// user's expansion state byte-for-byte.
        expanded: BTreeSet<String>,
    },
}

impl Tab {
    /// Iteration order for the tab strip and for `cycle_tab`. The order
    /// here is the order the tabs appear left-to-right on screen and the
    /// order `Tab` / `Shift+Tab` cycle through them.
    pub const ALL: &'static [Tab] =
        &[Tab::Overview, Tab::Pools, Tab::Datasets, Tab::Arc];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Pools => "Pools",
            Tab::Datasets => "Datasets",
            Tab::Arc => "ARC",
        }
    }

    /// Hotkey character bound to this tab. Used by the tab strip renderer
    /// to show the key binding next to each tab label.
    pub fn hotkey(&self) -> char {
        match self {
            Tab::Overview => '1',
            Tab::Pools => '2',
            Tab::Datasets => '3',
            Tab::Arc => '4',
        }
    }
}

/// ARC sub-segment colours for the RAM bar. `size` is the primary ARC, drawn
/// in the familiar magenta; `overhead_size` (ABD scatter waste + compression
/// bookkeeping) sits adjacent in a darker purple so the extra footprint is
/// visible without being mistaken for a separate category.
const ARC_SIZE_COLOR: Color = Color::Indexed(171); // xterm256 #D75FFF
const ARC_OVERHEAD_COLOR: Color = Color::Magenta;

/// Build the ARC sub-segments the RAM bar should render for a given snapshot.
/// Both `App::new` and `App::refresh` funnel through this so the two call
/// sites can't drift apart.
fn arc_segments(stats: &ArcStats) -> Vec<RamSegment> {
    vec![
        RamSegment {
            label: "ARC",
            color: ARC_SIZE_COLOR,
            bytes: stats.size,
        },
        RamSegment {
            label: "ARC ovh",
            color: ARC_OVERHEAD_COLOR,
            bytes: stats.overhead_size,
        },
    ]
}

pub struct App {
    pub current: ArcStats,
    pub previous: Option<ArcStats>,
    /// Closure that reads a fresh `ArcStats` snapshot. Constructed in `main.rs`
    /// per OS — Linux wraps a procfs path, FreeBSD wraps a sysctl call.
    arc_reader: Box<dyn FnMut() -> Result<ArcStats>>,
    pub mem_source: Option<Box<dyn MemSource>>,
    pub mem_snapshot: Option<MemSnapshot>,
    pub should_quit: bool,
    /// Currently-selected top-level tab. Defaults to `Tab::Overview`.
    pub current_tab: Tab,
    /// Current pool data source. `None` when libzfs initialization failed at
    /// startup (captured error lives in `pools_init_error`).
    pools_source: Option<Box<dyn PoolsSource>>,
    /// Latest successful snapshot from the pools source. Empty on a freshly
    /// started app until the first refresh, or on hosts where `pools_source`
    /// is `None`.
    pub pools_snapshot: Vec<PoolInfo>,
    /// Error from the most recent `refresh()` call, or `None` if the last
    /// refresh succeeded. Stale snapshots are preserved — the UI still shows
    /// the last good snapshot when this is `Some`.
    pub pools_refresh_error: Option<String>,
    /// Error from `LibzfsPoolsSource::new()`. Set once at startup and never
    /// cleared. `None` when libzfs initialized cleanly (even on hosts with
    /// zero imported pools — that's an empty `pools_snapshot`, not an error).
    pub pools_init_error: Option<String>,
    /// Pools tab view state (list with selected row / detail drilldown).
    pub pools_view: PoolsView,
    // NEW datasets fields (mirror pools_*).
    datasets_source: Option<Box<dyn DatasetsSource>>,
    pub datasets_snapshot: Vec<DatasetNode>,
    pub datasets_refresh_error: Option<String>,
    pub datasets_init_error: Option<String>,
    pub datasets_view: DatasetsView,
}

pub struct BreakdownRow {
    pub label: &'static str,
    pub bytes: u64,
    pub pct: f64,
}

impl App {
    pub fn new(
        mut arc_reader: Box<dyn FnMut() -> Result<ArcStats>>,
        mut mem_source: Option<Box<dyn MemSource>>,
        pools_source: Option<Box<dyn PoolsSource>>,
        pools_init_error: Option<String>,
        datasets_source: Option<Box<dyn DatasetsSource>>,
        datasets_init_error: Option<String>,
    ) -> Result<Self> {
        let current = arc_reader()?;
        let arc_segs = arc_segments(&current);
        let mem_snapshot = mem_source.as_mut().and_then(|s| s.snapshot(&arc_segs));
        let mut app = Self {
            current,
            previous: None,
            arc_reader,
            mem_source,
            mem_snapshot,
            should_quit: false,
            current_tab: Tab::Overview,
            pools_source,
            pools_snapshot: Vec::new(),
            pools_refresh_error: None,
            pools_init_error,
            pools_view: PoolsView::List { selected: 0 },
            datasets_source,
            datasets_snapshot: Vec::new(),
            datasets_refresh_error: None,
            datasets_init_error,
            datasets_view: DatasetsView::Tree {
                expanded: BTreeSet::new(),
                selected: 0,
            },
        };
        // Tick the pools source once so the first render has data.
        app.refresh_pools();
        app.refresh_datasets();
        // Seed `expanded` with every pool root so the landing screen
        // shows pools expanded one level.
        if let DatasetsView::Tree { expanded, .. } = &mut app.datasets_view {
            for root in &app.datasets_snapshot {
                expanded.insert(root.name.clone());
            }
        }
        Ok(app)
    }

    /// Tick the pools source, populate `pools_snapshot` on success, preserve
    /// the stale snapshot on transient errors. No-op when `pools_source` is
    /// `None` (libzfs init failed at startup).
    fn refresh_pools(&mut self) {
        let Some(ps) = self.pools_source.as_mut() else {
            return;
        };
        match ps.refresh() {
            Ok(()) => {
                self.pools_snapshot = ps.pools();
                self.pools_refresh_error = None;
                self.clamp_pools_selection();
            }
            Err(e) => {
                self.pools_refresh_error = Some(e.to_string());
                // Keep stale snapshot — better than blanking on a transient.
            }
        }
    }

    /// Tick the datasets source. On success, populate the snapshot, prune
    /// expansion entries that no longer exist, reclamp the selection, and
    /// fall back from Detail to Tree if the inspected dataset vanished.
    /// On error, preserve the stale snapshot.
    fn refresh_datasets(&mut self) {
        let Some(ds) = self.datasets_source.as_mut() else {
            return;
        };
        match ds.refresh() {
            Ok(()) => {
                self.datasets_snapshot = ds.roots();
                self.datasets_refresh_error = None;
                self.prune_expanded_set();
                self.clamp_datasets_selection();
                self.fall_back_from_detail_if_dataset_vanished();
            }
            Err(e) => {
                self.datasets_refresh_error = Some(e.to_string());
            }
        }
    }

    /// Walk the new snapshot, collect every dataset name into a set, then
    /// retain only those entries in `expanded`. Names that vanish silently
    /// drop out; names that reappear later are not auto-restored.
    fn prune_expanded_set(&mut self) {
        let DatasetsView::Tree { expanded, .. } = &mut self.datasets_view else {
            return;
        };
        let mut all_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        fn walk(node: &DatasetNode, into: &mut std::collections::HashSet<String>) {
            into.insert(node.name.clone());
            for c in &node.children {
                walk(c, into);
            }
        }
        for root in &self.datasets_snapshot {
            walk(root, &mut all_names);
        }
        expanded.retain(|n| all_names.contains(n));
    }

    /// Reclamp `selected` to the visible row count after the snapshot
    /// changes. Computes visible count via `flatten_visible_dataset_rows`.
    fn clamp_datasets_selection(&mut self) {
        let visible_count = self.flatten_visible_dataset_rows().len();
        let DatasetsView::Tree { selected, .. } = &mut self.datasets_view else {
            return;
        };
        if visible_count == 0 {
            *selected = 0;
        } else if *selected >= visible_count {
            *selected = visible_count - 1;
        }
    }

    /// If the Detail view's named dataset is no longer present in the
    /// snapshot, fall back to Tree at row 0. Restores the cached expansion
    /// set.
    fn fall_back_from_detail_if_dataset_vanished(&mut self) {
        let DatasetsView::Detail { name, expanded } = &self.datasets_view else {
            return;
        };
        let mut exists = false;
        fn walk(node: &DatasetNode, target: &str, found: &mut bool) {
            if node.name == target {
                *found = true;
                return;
            }
            for c in &node.children {
                if *found {
                    return;
                }
                walk(c, target, found);
            }
        }
        for root in &self.datasets_snapshot {
            if exists {
                break;
            }
            walk(root, name, &mut exists);
        }
        if !exists {
            self.datasets_view = DatasetsView::Tree {
                expanded: expanded.clone(),
                selected: 0,
            };
        }
    }

    /// DFS over `datasets_snapshot` honoring `expanded`, returning
    /// (depth, &node) pairs in render order. Pure function over
    /// `(snapshot, expanded)`. Returns empty Vec when in Detail view.
    pub fn flatten_visible_dataset_rows(&self) -> Vec<(usize, &DatasetNode)> {
        let DatasetsView::Tree { expanded, .. } = &self.datasets_view else {
            return Vec::new();
        };
        let mut out = Vec::new();
        fn walk<'a>(
            node: &'a DatasetNode,
            depth: usize,
            expanded: &BTreeSet<String>,
            out: &mut Vec<(usize, &'a DatasetNode)>,
        ) {
            out.push((depth, node));
            if expanded.contains(&node.name) {
                for c in &node.children {
                    walk(c, depth + 1, expanded, out);
                }
            }
        }
        for root in &self.datasets_snapshot {
            walk(root, 0, expanded, &mut out);
        }
        out
    }

    /// Keep `pools_view` valid when the snapshot shape shifts under it.
    /// - List selection is clamped to `len - 1`.
    /// - Detail with an index past the new `len` falls back to List.
    fn clamp_pools_selection(&mut self) {
        match &mut self.pools_view {
            PoolsView::List { selected } => {
                if self.pools_snapshot.is_empty() {
                    *selected = 0;
                } else if *selected >= self.pools_snapshot.len() {
                    *selected = self.pools_snapshot.len() - 1;
                }
            }
            PoolsView::Detail { pool_index } => {
                if *pool_index >= self.pools_snapshot.len() {
                    self.pools_view = PoolsView::List {
                        selected: self.pools_snapshot.len().saturating_sub(1),
                    };
                }
            }
        }
    }

    /// Move `current_tab` by `delta` positions through `Tab::ALL`, wrapping
    /// in both directions. `+1` is next tab (used by `Tab` key), `-1` is
    /// previous tab (used by `Shift+Tab` / `BackTab`).
    pub fn cycle_tab(&mut self, delta: i32) {
        let all = Tab::ALL;
        let len = all.len() as i32;
        let current_idx = all
            .iter()
            .position(|t| *t == self.current_tab)
            .unwrap_or(0) as i32;
        let next_idx = ((current_idx + delta) % len + len) % len;
        self.switch_tab(all[next_idx as usize]);
    }

    /// Switch to a different top-level tab. Leaving the Pools tab while
    /// drilled into a specific pool collapses the drilldown back to the
    /// list view (keeping the selection on the same pool), so returning
    /// to Pools later lands on the list — not on a stale detail view. A
    /// no-op switch (e.g. pressing `2` while already on Pools) preserves
    /// whatever sub-view the user is currently in.
    fn switch_tab(&mut self, target: Tab) {
        if target == self.current_tab {
            return;
        }
        if self.current_tab == Tab::Pools {
            if let PoolsView::Detail { pool_index } = self.pools_view {
                self.pools_view = PoolsView::List {
                    selected: pool_index,
                };
            }
        }
        self.current_tab = target;
    }

    pub fn refresh(&mut self) -> Result<()> {
        let next = (self.arc_reader)()?;
        self.previous = Some(std::mem::replace(&mut self.current, next));
        if let Some(mem) = self.mem_source.as_mut() {
            // Memory refresh failures are non-fatal — the bar just won't update.
            let _ = mem.refresh();
        }
        let arc_segs = arc_segments(&self.current);
        self.mem_snapshot = self.mem_source.as_ref().and_then(|s| s.snapshot(&arc_segs));
        self.refresh_pools();
        self.refresh_datasets();
        Ok(())
    }

    /// Count pools whose health is anything other than Online. Used by the
    /// Overview alarm summary to highlight "something is wrong" at a glance.
    pub fn pools_degraded_count(&self) -> usize {
        self.pools_snapshot
            .iter()
            .filter(|p| p.health != crate::pools::PoolHealth::Online)
            .count()
    }

    /// Sum of `size_bytes` across every pool in the snapshot.
    pub fn pools_total_capacity(&self) -> u64 {
        self.pools_snapshot.iter().map(|p| p.size_bytes).sum()
    }

    /// Sum of `allocated_bytes` across every pool in the snapshot.
    pub fn pools_total_allocated(&self) -> u64 {
        self.pools_snapshot.iter().map(|p| p.allocated_bytes).sum()
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Global bindings — handled on every tab.
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('r') => {
                let _ = self.refresh();
                return;
            }
            KeyCode::Char('1') => {
                self.switch_tab(Tab::Overview);
                return;
            }
            KeyCode::Char('2') => {
                self.switch_tab(Tab::Pools);
                return;
            }
            KeyCode::Char('3') => {
                self.switch_tab(Tab::Datasets);
                return;
            }
            KeyCode::Char('4') => {
                self.switch_tab(Tab::Arc);
                return;
            }
            KeyCode::Tab => {
                self.cycle_tab(1);
                return;
            }
            KeyCode::BackTab => {
                self.cycle_tab(-1);
                return;
            }
            _ => {}
        }

        // Per-tab bindings.
        if self.current_tab == Tab::Pools {
            self.on_key_pools(key);
        }
    }

    /// Handle a mouse event. Scroll wheel events on the Pools list move
     /// the selection; elsewhere they're ignored. Click/drag/move events
     /// are ignored entirely — zftop is keyboard-driven.
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll(1),
            MouseEventKind::ScrollUp => self.scroll(-1),
            _ => {}
        }
    }

    fn scroll(&mut self, delta: i32) {
        if self.current_tab == Tab::Pools {
            if let PoolsView::List { selected } = &mut self.pools_view {
                if self.pools_snapshot.is_empty() {
                    return;
                }
                let last = self.pools_snapshot.len() - 1;
                let new = (*selected as i32 + delta).clamp(0, last as i32) as usize;
                *selected = new;
            }
        }
    }

    fn on_key_pools(&mut self, key: KeyEvent) {
        match (self.pools_view, key.code) {
            // List navigation
            (PoolsView::List { .. }, KeyCode::Down | KeyCode::Char('j')) => {
                if let PoolsView::List { selected } = &mut self.pools_view {
                    if *selected + 1 < self.pools_snapshot.len() {
                        *selected += 1;
                    }
                }
            }
            (PoolsView::List { .. }, KeyCode::Up | KeyCode::Char('k')) => {
                if let PoolsView::List { selected } = &mut self.pools_view {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
            }
            (PoolsView::List { .. }, KeyCode::Home) => {
                if let PoolsView::List { selected } = &mut self.pools_view {
                    *selected = 0;
                }
            }
            (PoolsView::List { .. }, KeyCode::End) => {
                if let PoolsView::List { selected } = &mut self.pools_view {
                    *selected = self.pools_snapshot.len().saturating_sub(1);
                }
            }
            (PoolsView::List { selected }, KeyCode::Enter) => {
                if !self.pools_snapshot.is_empty() {
                    self.pools_view = PoolsView::Detail { pool_index: selected };
                }
            }
            // Detail → back to list
            (PoolsView::Detail { pool_index }, KeyCode::Esc | KeyCode::Backspace) => {
                self.pools_view = PoolsView::List { selected: pool_index };
            }
            _ => {}
        }
    }

    pub fn hit_ratio_overall(&self) -> f64 {
        ratio(self.current.hits, self.current.misses)
    }

    pub fn hit_ratio_demand(&self) -> f64 {
        let hits = self.current.demand_data_hits + self.current.demand_metadata_hits;
        let misses = self.current.demand_data_misses + self.current.demand_metadata_misses;
        ratio(hits, misses)
    }

    pub fn hit_ratio_prefetch(&self) -> f64 {
        let hits = self.current.prefetch_data_hits + self.current.prefetch_metadata_hits;
        let misses = self.current.prefetch_data_misses + self.current.prefetch_metadata_misses;
        ratio(hits, misses)
    }

    pub fn throughput_hits(&self) -> Option<u64> {
        self.previous
            .as_ref()
            .map(|prev| self.current.hits.saturating_sub(prev.hits))
    }

    pub fn throughput_misses(&self) -> Option<u64> {
        self.previous
            .as_ref()
            .map(|prev| self.current.misses.saturating_sub(prev.misses))
    }

    pub fn throughput_iohits(&self) -> Option<u64> {
        self.previous
            .as_ref()
            .map(|prev| self.current.iohits.saturating_sub(prev.iohits))
    }

    pub fn arc_breakdown(&self) -> Vec<BreakdownRow> {
        let s = &self.current;
        let total = s.size;

        let rows = [
            ("MFU data", s.mfu_data),
            ("MFU meta", s.mfu_metadata),
            ("MRU data", s.mru_data),
            ("MRU meta", s.mru_metadata),
            ("Anon", s.anon_size),
            ("Headers", s.hdr_size),
            ("Dbuf", s.dbuf_size),
            ("Dnode", s.dnode_size),
            ("Bonus", s.bonus_size),
        ];

        rows.into_iter()
            .map(|(label, bytes)| BreakdownRow {
                label,
                bytes,
                pct: if total > 0 {
                    bytes as f64 / total as f64 * 100.0
                } else {
                    0.0
                },
            })
            .collect()
    }

    pub fn arc_usage_pct(&self) -> f64 {
        if self.current.c_max > 0 {
            self.current.size as f64 / self.current.c_max as f64
        } else {
            0.0
        }
    }

    /// ARC compression ratio: uncompressed / compressed. >1.0 means compression is helping.
    pub fn arc_compression_ratio(&self) -> Option<f64> {
        let s = &self.current;
        if s.compressed_size > 0 {
            Some(s.uncompressed_size as f64 / s.compressed_size as f64)
        } else {
            None
        }
    }

    /// Returns the cached system-RAM snapshot for the UI.
    pub fn ram_segments(&self) -> Option<(u64, &[RamSegment])> {
        self.mem_snapshot
            .as_ref()
            .map(|s| (s.total_bytes, s.segments.as_slice()))
    }
}

fn ratio(hits: u64, misses: u64) -> f64 {
    let total = hits + misses;
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64 * 100.0
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;

    let b = bytes as f64;
    if b >= TIB {
        format!("{:.1} TiB", b / TIB)
    } else if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stats() -> ArcStats {
        ArcStats {
            hits: 9000,
            iohits: 100,
            misses: 1000,
            demand_data_hits: 5000,
            demand_data_iohits: 50,
            demand_data_misses: 500,
            demand_metadata_hits: 3000,
            demand_metadata_iohits: 30,
            demand_metadata_misses: 300,
            prefetch_data_hits: 800,
            prefetch_data_iohits: 15,
            prefetch_data_misses: 150,
            prefetch_metadata_hits: 200,
            prefetch_metadata_iohits: 5,
            prefetch_metadata_misses: 50,
            size: 10 * 1024 * 1024 * 1024,     // 10 GiB
            c: 16 * 1024 * 1024 * 1024,        // 16 GiB
            c_min: 1024 * 1024 * 1024,          // 1 GiB
            c_max: 16 * 1024 * 1024 * 1024,     // 16 GiB
            data_size: 6 * 1024 * 1024 * 1024,
            metadata_size: 1024 * 1024 * 1024,
            anon_size: 512 * 1024 * 1024,
            overhead_size: 256 * 1024 * 1024,
            hdr_size: 64 * 1024 * 1024,
            dbuf_size: 96 * 1024 * 1024,
            dnode_size: 128 * 1024 * 1024,
            bonus_size: 64 * 1024 * 1024,
            mru_size: 3 * 1024 * 1024 * 1024,
            mru_data: 2 * 1024 * 1024 * 1024,
            mru_metadata: 1024 * 1024 * 1024,
            mfu_size: 4 * 1024 * 1024 * 1024,
            mfu_data: 3 * 1024 * 1024 * 1024,
            mfu_metadata: 1024 * 1024 * 1024,
            compressed_size: 5 * 1024 * 1024 * 1024,
            uncompressed_size: 8 * 1024 * 1024 * 1024,
            memory_all_bytes: 32 * 1024 * 1024 * 1024,
            memory_free_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 12 * 1024 * 1024 * 1024,
            arc_meta_used: 2 * 1024 * 1024 * 1024,
        }
    }

    /// Build an `App` with no live sources — used by derived-metric tests
    /// that don't exercise refresh().
    fn app_with(current: ArcStats, previous: Option<ArcStats>) -> App {
        App {
            current,
            previous,
            arc_reader: Box::new(|| panic!("arc_reader not used in this test")),
            mem_source: None,
            mem_snapshot: None,
            should_quit: false,
            current_tab: Tab::Overview,
            pools_source: None,
            pools_snapshot: Vec::new(),
            pools_refresh_error: None,
            pools_init_error: None,
            pools_view: PoolsView::List { selected: 0 },
            datasets_source: None,
            datasets_snapshot: Vec::new(),
            datasets_refresh_error: None,
            datasets_init_error: None,
            datasets_view: DatasetsView::Tree {
                expanded: BTreeSet::new(),
                selected: 0,
            },
        }
    }

    /// Test stub: echoes the `arc_segments` slice it receives back as the
    /// snapshot's segments verbatim, so tests can assert exactly what App
    /// passed into `MemSource::snapshot()` — labels, colours and byte counts.
    struct EchoMemSource;

    impl MemSource for EchoMemSource {
        fn refresh(&mut self) -> Result<()> {
            Ok(())
        }

        fn snapshot(&self, arc_segments: &[RamSegment]) -> Option<MemSnapshot> {
            Some(MemSnapshot {
                total_bytes: 100 * 1024 * 1024 * 1024, // 100 GiB, arbitrary
                segments: arc_segments.to_vec(),
            })
        }
    }

    #[test]
    fn overall_hit_ratio() {
        let app = app_with(sample_stats(), None);
        assert!((app.hit_ratio_overall() - 90.0).abs() < 0.01);
    }

    #[test]
    fn demand_hit_ratio() {
        let app = app_with(sample_stats(), None);
        // (5000+3000) / (5000+3000+500+300) = 8000/8800 ≈ 90.909%
        assert!((app.hit_ratio_demand() - 90.909).abs() < 0.01);
    }

    #[test]
    fn prefetch_hit_ratio() {
        let app = app_with(sample_stats(), None);
        // (800+200) / (800+200+150+50) = 1000/1200 ≈ 83.333%
        assert!((app.hit_ratio_prefetch() - 83.333).abs() < 0.01);
    }

    #[test]
    fn throughput_none_without_previous() {
        let app = app_with(sample_stats(), None);
        assert!(app.throughput_hits().is_none());
        assert!(app.throughput_misses().is_none());
    }

    #[test]
    fn throughput_delta() {
        let mut prev = sample_stats();
        prev.hits = 8000;
        prev.misses = 900;
        let app = app_with(sample_stats(), Some(prev));
        assert_eq!(app.throughput_hits(), Some(1000));
        assert_eq!(app.throughput_misses(), Some(100));
    }

    #[test]
    fn arc_usage() {
        let app = app_with(sample_stats(), None);
        assert!((app.arc_usage_pct() - 0.625).abs() < 0.001);
    }

    #[test]
    fn breakdown_has_expected_categories() {
        let app = app_with(sample_stats(), None);
        let rows = app.arc_breakdown();
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        assert!(labels.contains(&"MFU data"));
        assert!(labels.contains(&"MRU data"));
        assert!(labels.contains(&"Anon"));
        assert!(labels.contains(&"Headers"));
        for row in &rows {
            assert!(row.pct >= 0.0 && row.pct <= 100.0);
        }
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1048576), "1.0 MiB");
        assert_eq!(format_bytes(1073741824), "1.0 GiB");
        assert_eq!(format_bytes(1099511627776), "1.0 TiB");
    }

    #[test]
    fn tab_all_ordered_overview_pools_datasets_arc() {
        assert_eq!(Tab::ALL, &[Tab::Overview, Tab::Pools, Tab::Datasets, Tab::Arc]);
    }

    #[test]
    fn tab_titles_stable() {
        assert_eq!(Tab::Overview.title(), "Overview");
        assert_eq!(Tab::Pools.title(), "Pools");
        assert_eq!(Tab::Datasets.title(), "Datasets");
        assert_eq!(Tab::Arc.title(), "ARC");
    }

    #[test]
    fn tab_hotkeys_match_position() {
        assert_eq!(Tab::Overview.hotkey(), '1');
        assert_eq!(Tab::Pools.hotkey(), '2');
        assert_eq!(Tab::Datasets.hotkey(), '3');
        assert_eq!(Tab::Arc.hotkey(), '4');
    }

    #[test]
    fn cycle_tab_forward_wraps() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.cycle_tab(1);
        assert_eq!(app.current_tab, Tab::Pools);
        app.cycle_tab(1);
        assert_eq!(app.current_tab, Tab::Datasets);
        app.cycle_tab(1);
        assert_eq!(app.current_tab, Tab::Arc);
        app.cycle_tab(1); // wraps
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn cycle_tab_back_wraps() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.cycle_tab(-1); // wraps
        assert_eq!(app.current_tab, Tab::Arc);
        app.cycle_tab(-1);
        assert_eq!(app.current_tab, Tab::Datasets);
        app.cycle_tab(-1);
        assert_eq!(app.current_tab, Tab::Pools);
        app.cycle_tab(-1);
        assert_eq!(app.current_tab, Tab::Overview);
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn hotkey_1_switches_to_overview() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Arc;
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn hotkey_2_switches_to_pools() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.current_tab, Tab::Pools);
    }

    #[test]
    fn hotkey_3_switches_to_datasets() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::Char('3')));
        assert_eq!(app.current_tab, Tab::Datasets);
    }

    #[test]
    fn hotkey_4_switches_to_arc() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.current_tab, Tab::Arc);
    }

    #[test]
    fn tab_key_cycles_forward() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Pools);
    }

    #[test]
    fn back_tab_cycles_backward() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Arc);
    }

    #[test]
    fn q_still_quits() {
        let mut app = app_with(sample_stats(), None);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_still_quits() {
        let mut app = app_with(sample_stats(), None);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    use crate::pools::fake::FakePoolsSource;
    use crate::pools::{
        ErrorCounts as PoolErrors, PoolHealth, PoolInfo, ScrubState, VdevKind, VdevNode,
        VdevState,
    };

    fn test_pool(name: &str, health: PoolHealth, size: u64, alloc: u64) -> PoolInfo {
        PoolInfo {
            name: name.into(),
            health,
            allocated_bytes: alloc,
            size_bytes: size,
            free_bytes: size.saturating_sub(alloc),
            fragmentation_pct: Some(10),
            scrub: ScrubState::Never,
            errors: PoolErrors::default(),
            root_vdev: VdevNode {
                name: name.into(),
                kind: VdevKind::Root,
                state: VdevState::Online,
                size_bytes: Some(size),
                errors: PoolErrors::default(),
                children: vec![],
            },
        }
    }

    fn app_with_pools(pools: Vec<PoolInfo>) -> App {
        let mut app = app_with(sample_stats(), None);
        app.pools_source = Some(Box::new(FakePoolsSource::new(pools.clone())));
        app.pools_snapshot = pools;
        app
    }

    #[test]
    fn refresh_pools_populates_snapshot_from_source() {
        let pools = vec![test_pool("tank", PoolHealth::Online, 1_000, 500)];
        let mut app = app_with(sample_stats(), None);
        app.pools_source = Some(Box::new(FakePoolsSource::new(pools.clone())));
        app.refresh_pools();
        assert_eq!(app.pools_snapshot.len(), 1);
        assert_eq!(app.pools_snapshot[0].name, "tank");
        assert!(app.pools_refresh_error.is_none());
    }

    #[test]
    fn refresh_pools_error_preserves_stale_snapshot() {
        let initial = vec![test_pool("tank", PoolHealth::Online, 1_000, 500)];
        let mut app = app_with_pools(initial);
        // Swap the source for one that errors on the next refresh.
        app.pools_source = Some(Box::new(
            FakePoolsSource::new(vec![]).fail_next_refresh("transient libzfs fail"),
        ));
        app.refresh_pools();
        assert!(app.pools_refresh_error.is_some());
        assert_eq!(app.pools_snapshot.len(), 1, "snapshot should be preserved");
    }

    #[test]
    fn pools_degraded_count_sums_non_online() {
        let app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Degraded, 100, 50),
            test_pool("c", PoolHealth::Faulted, 100, 50),
        ]);
        assert_eq!(app.pools_degraded_count(), 2);
    }

    #[test]
    fn pools_totals_sum_correctly() {
        let app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 1000, 400),
            test_pool("b", PoolHealth::Online, 2000, 800),
        ]);
        assert_eq!(app.pools_total_capacity(), 3000);
        assert_eq!(app.pools_total_allocated(), 1200);
    }

    #[test]
    fn selection_clamps_when_pools_shrink() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ]);
        app.pools_view = PoolsView::List { selected: 2 };
        // Shrink the underlying source to one pool and refresh.
        app.pools_source = Some(Box::new(FakePoolsSource::new(vec![test_pool(
            "a",
            PoolHealth::Online,
            100,
            50,
        )])));
        app.refresh_pools();
        assert_eq!(app.pools_view, PoolsView::List { selected: 0 });
    }

    #[test]
    fn pools_down_advances_selection() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.pools_view, PoolsView::List { selected: 1 });
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.pools_view, PoolsView::List { selected: 2 });
    }

    #[test]
    fn pools_down_clamps_at_last() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::List { selected: 1 };
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.pools_view, PoolsView::List { selected: 1 });
    }

    #[test]
    fn pools_up_at_first_is_noop() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.pools_view, PoolsView::List { selected: 0 });
    }

    #[test]
    fn pools_home_end_jump() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.on_key(key(KeyCode::End));
        assert_eq!(app.pools_view, PoolsView::List { selected: 2 });
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.pools_view, PoolsView::List { selected: 0 });
    }

    #[test]
    fn pools_enter_drills_into_detail() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::List { selected: 1 };
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.pools_view, PoolsView::Detail { pool_index: 1 });
    }

    #[test]
    fn pools_enter_with_empty_list_is_noop() {
        let mut app = app_with_pools(vec![]);
        app.current_tab = Tab::Pools;
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.pools_view, PoolsView::List { .. }));
    }

    /// Leaving the Pools tab while drilled into a specific pool must
    /// collapse the drilldown so that returning to Pools lands on the
    /// list. The selection stays on the pool the user was inspecting.
    /// Exercises every tab-change key: 1, 3, Tab, BackTab.
    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_overview_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail { pool_index: 2 };
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.current_tab, Tab::Overview);
        assert_eq!(
            app.pools_view,
            PoolsView::List { selected: 2 },
            "leaving Pools via '1' should have collapsed Detail to List"
        );
    }

    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_arc_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail { pool_index: 1 };
        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.current_tab, Tab::Arc);
        assert_eq!(app.pools_view, PoolsView::List { selected: 1 });
    }

    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_tab_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail { pool_index: 0 };
        app.on_key(key(KeyCode::Tab));
        // Pools → Datasets (next in Tab::ALL order).
        assert_eq!(app.current_tab, Tab::Datasets);
        assert_eq!(app.pools_view, PoolsView::List { selected: 0 });
    }

    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_backtab_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail { pool_index: 1 };
        app.on_key(key(KeyCode::BackTab));
        // Pools ← Overview (previous in Tab::ALL order).
        assert_eq!(app.current_tab, Tab::Overview);
        assert_eq!(app.pools_view, PoolsView::List { selected: 1 });
    }

    /// Pressing `2` while already on the Pools tab must NOT collapse an
    /// in-progress drilldown — that's a no-op switch. Only switching
    /// *away* and back should reset the sub-view.
    #[test]
    fn pressing_pools_key_while_already_on_pools_preserves_detail() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail { pool_index: 1 };
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.current_tab, Tab::Pools);
        assert_eq!(
            app.pools_view,
            PoolsView::Detail { pool_index: 1 },
            "no-op tab switch should not disturb the sub-view"
        );
    }

    /// End-to-end round trip: drill in, tab out, tab back → list view.
    #[test]
    fn pools_detail_round_trip_ends_on_list() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::List { selected: 2 };
        // Drill in
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.pools_view, PoolsView::Detail { pool_index: 2 });
        // Tab out to Overview
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.current_tab, Tab::Overview);
        // Return to Pools
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.current_tab, Tab::Pools);
        assert_eq!(
            app.pools_view,
            PoolsView::List { selected: 2 },
            "returning to Pools after a drill-in + tab-out should land on the list"
        );
    }

    #[test]
    fn pools_esc_returns_to_list_with_same_index() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail { pool_index: 1 };
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.pools_view, PoolsView::List { selected: 1 });
    }

    #[test]
    fn pools_keys_ignored_when_not_on_pools_tab() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::Down));
        // Selection unchanged because we're not on the Pools tab.
        assert_eq!(app.pools_view, PoolsView::List { selected: 0 });
    }

    #[test]
    fn detail_view_drops_to_list_when_pool_vanishes() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.pools_view = PoolsView::Detail { pool_index: 1 };
        app.pools_source = Some(Box::new(FakePoolsSource::new(vec![test_pool(
            "a",
            PoolHealth::Online,
            100,
            50,
        )])));
        app.refresh_pools();
        assert!(matches!(app.pools_view, PoolsView::List { selected: 0 }));
    }

    use crate::datasets::fake::FakeDatasetsSource;
    use crate::datasets::{DatasetKind, DatasetNode, DatasetProperties};

    fn ds(name: &str, kind: DatasetKind, children: Vec<DatasetNode>) -> DatasetNode {
        DatasetNode {
            name: name.into(),
            kind,
            used_bytes: 100,
            refer_bytes: 100,
            available_bytes: 1000,
            compression_ratio: 1.0,
            properties: DatasetProperties::default(),
            children,
        }
    }

    fn ds_fs(name: &str, children: Vec<DatasetNode>) -> DatasetNode {
        ds(name, DatasetKind::Filesystem, children)
    }

    fn app_with_datasets(roots: Vec<DatasetNode>) -> App {
        let mut app = app_with(sample_stats(), None);
        app.datasets_source = Some(Box::new(FakeDatasetsSource::new(roots.clone())));
        app.datasets_snapshot = roots.clone();
        // Seed expanded with the root names like App::new does.
        if let DatasetsView::Tree { expanded, .. } = &mut app.datasets_view {
            for r in &roots {
                expanded.insert(r.name.clone());
            }
        }
        app
    }

    #[test]
    fn refresh_datasets_populates_snapshot_from_source() {
        let roots = vec![ds_fs("tank", vec![])];
        let mut app = app_with(sample_stats(), None);
        app.datasets_source = Some(Box::new(FakeDatasetsSource::new(roots.clone())));
        app.refresh_datasets();
        assert_eq!(app.datasets_snapshot.len(), 1);
        assert_eq!(app.datasets_snapshot[0].name, "tank");
        assert!(app.datasets_refresh_error.is_none());
    }

    #[test]
    fn refresh_datasets_error_preserves_stale_snapshot() {
        let initial = vec![ds_fs("tank", vec![])];
        let mut app = app_with_datasets(initial);
        app.datasets_source = Some(Box::new(
            FakeDatasetsSource::new(vec![]).fail_next_refresh("transient libzfs fail"),
        ));
        app.refresh_datasets();
        assert!(app.datasets_refresh_error.is_some());
        assert_eq!(app.datasets_snapshot.len(), 1, "snapshot should be preserved");
    }

    #[test]
    fn prune_expanded_set_removes_vanished_names() {
        let initial = vec![ds_fs("tank", vec![ds_fs("tank/home", vec![])])];
        let mut app = app_with_datasets(initial);
        if let DatasetsView::Tree { expanded, .. } = &mut app.datasets_view {
            expanded.insert("tank/home".to_string());
        }
        // Swap in a snapshot that no longer has tank/home.
        app.datasets_source =
            Some(Box::new(FakeDatasetsSource::new(vec![ds_fs("tank", vec![])])));
        app.refresh_datasets();
        if let DatasetsView::Tree { expanded, .. } = &app.datasets_view {
            assert!(expanded.contains("tank"), "tank should still be expanded");
            assert!(
                !expanded.contains("tank/home"),
                "tank/home should be pruned"
            );
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn detail_view_falls_back_to_tree_when_dataset_vanishes() {
        let initial = vec![ds_fs("tank", vec![ds_fs("tank/home", vec![])])];
        let mut app = app_with_datasets(initial);
        let mut expanded_clone = BTreeSet::new();
        expanded_clone.insert("tank".to_string());
        app.datasets_view = DatasetsView::Detail {
            name: "tank/home".into(),
            expanded: expanded_clone,
        };
        // tank/home gets destroyed.
        app.datasets_source =
            Some(Box::new(FakeDatasetsSource::new(vec![ds_fs("tank", vec![])])));
        app.refresh_datasets();
        assert!(matches!(app.datasets_view, DatasetsView::Tree { .. }));
    }

    #[test]
    fn detail_view_survives_when_dataset_still_exists() {
        let initial = vec![ds_fs("tank", vec![ds_fs("tank/home", vec![])])];
        let mut app = app_with_datasets(initial);
        let mut expanded_clone = BTreeSet::new();
        expanded_clone.insert("tank".to_string());
        app.datasets_view = DatasetsView::Detail {
            name: "tank/home".into(),
            expanded: expanded_clone,
        };
        // Snapshot unchanged; refresh shouldn't disturb the detail view.
        app.refresh_datasets();
        assert!(matches!(app.datasets_view, DatasetsView::Detail { .. }));
    }

    #[test]
    fn app_passes_two_arc_segments_size_and_overhead() {
        // The RAM bar should get TWO adjacent ARC sub-segments: primary `size`
        // in the familiar magenta, and `overhead_size` (ABD scatter waste,
        // compression bookkeeping — real RAM not counted in `size`) in a
        // darker purple so the extra footprint is visible but visually tied
        // to ARC. Both segments must arrive through MemSource::snapshot so
        // meminfo stays agnostic about what counts as ARC.
        let stats = sample_stats();

        let arc_reader: Box<dyn FnMut() -> Result<ArcStats>> =
            Box::new(move || Ok(sample_stats()));
        let mem_source: Option<Box<dyn MemSource>> = Some(Box::new(EchoMemSource));

        let app = App::new(arc_reader, mem_source, None, None, None, None)
            .expect("App::new should succeed");
        let snap = app.mem_snapshot.expect("snapshot should be present");

        assert_eq!(
            snap.segments.len(),
            2,
            "App should pass two ARC sub-segments (size + overhead_size)"
        );

        assert_eq!(snap.segments[0].label, "ARC");
        assert_eq!(snap.segments[0].bytes, stats.size);
        assert_eq!(snap.segments[0].color, ARC_SIZE_COLOR);

        assert_eq!(snap.segments[1].label, "ARC ovh");
        assert_eq!(snap.segments[1].bytes, stats.overhead_size);
        assert_eq!(snap.segments[1].color, ARC_OVERHEAD_COLOR);

        // Darker-purple guard: overhead must NOT reuse the primary ARC colour,
        // or the split would be invisible to the user.
        assert_ne!(
            snap.segments[0].color, snap.segments[1].color,
            "ARC and ARC ovh must use visually distinct colours"
        );
    }

    #[test]
    fn flatten_visible_returns_only_pool_roots_when_nothing_expanded() {
        let mut app = app_with_datasets(vec![
            ds_fs("tank", vec![ds_fs("tank/home", vec![])]),
            ds_fs("scratch", vec![]),
        ]);
        // Override the seed-from-app_with_datasets — start fully collapsed.
        app.datasets_view = DatasetsView::Tree {
            expanded: BTreeSet::new(),
            selected: 0,
        };
        let rows = app.flatten_visible_dataset_rows();
        let names: Vec<&str> = rows.iter().map(|(_, n)| n.name.as_str()).collect();
        assert_eq!(names, vec!["tank", "scratch"]);
    }

    #[test]
    fn flatten_visible_descends_only_into_expanded() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![
                ds_fs("tank/home", vec![ds_fs("tank/home/alice", vec![])]),
                ds_fs("tank/srv", vec![]),
            ],
        )]);
        let mut expanded = BTreeSet::new();
        expanded.insert("tank".to_string()); // expand pool root only
        app.datasets_view = DatasetsView::Tree {
            expanded,
            selected: 0,
        };
        let rows = app.flatten_visible_dataset_rows();
        let names: Vec<&str> = rows.iter().map(|(_, n)| n.name.as_str()).collect();
        assert_eq!(names, vec!["tank", "tank/home", "tank/srv"]);
        // tank/home/alice hidden because tank/home not expanded.
    }

    #[test]
    fn flatten_visible_descends_into_nested_expanded() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![ds_fs("tank/home/alice", vec![])])],
        )]);
        let mut expanded = BTreeSet::new();
        expanded.insert("tank".to_string());
        expanded.insert("tank/home".to_string());
        app.datasets_view = DatasetsView::Tree {
            expanded,
            selected: 0,
        };
        let rows = app.flatten_visible_dataset_rows();
        let names: Vec<&str> = rows.iter().map(|(_, n)| n.name.as_str()).collect();
        assert_eq!(names, vec!["tank", "tank/home", "tank/home/alice"]);
    }

    #[test]
    fn flatten_visible_depth_tags_match_tree_depth() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![ds_fs("tank/home/alice", vec![])])],
        )]);
        let mut expanded = BTreeSet::new();
        expanded.insert("tank".to_string());
        expanded.insert("tank/home".to_string());
        app.datasets_view = DatasetsView::Tree {
            expanded,
            selected: 0,
        };
        let depths: Vec<usize> = app
            .flatten_visible_dataset_rows()
            .iter()
            .map(|(d, _)| *d)
            .collect();
        assert_eq!(depths, vec![0, 1, 2]);
    }

    #[test]
    fn flatten_visible_returns_empty_in_detail_view() {
        let app = app_with_datasets(vec![ds_fs("tank", vec![])]);
        let app = {
            let mut a = app;
            a.datasets_view = DatasetsView::Detail {
                name: "tank".into(),
                expanded: BTreeSet::new(),
            };
            a
        };
        assert_eq!(app.flatten_visible_dataset_rows().len(), 0);
    }
}
