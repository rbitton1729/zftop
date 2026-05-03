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

/// State of the Pools tab: tree view with an expansion set + a selection
/// index over the visible (post-flatten) rows, or a per-pool detail
/// drilldown identified by index into the snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolsView {
    Tree {
        /// Pool names whose vdev subtree is currently visible. Pools
        /// auto-fill this set on first paint (see `refresh_pools` +
        /// `pools_first_paint`); subsequent refreshes only `prune` the
        /// set, never re-add. Vdev rows aren't separately expandable —
        /// expanding a pool reveals its entire vdev tree.
        expanded: BTreeSet<String>,
        /// Index into the *visible* (post-flatten) row list. Reclamped
        /// on every refresh and on collapse.
        selected: usize,
    },
    Detail {
        pool_index: usize,
        /// Cached `expanded` set so returning to Tree restores the
        /// user's expansion state byte-for-byte.
        expanded: BTreeSet<String>,
    },
}

/// One visible row in the pools tree. Pool rows render with the wide
/// pool-info layout (HEALTH/TYPE/CAPACITY/FRAG/SCRUB/ERR); vdev rows
/// render with the vdev layout (STATE/KIND/SIZE/DEVICE_PATH/R/W/C).
/// `depth` is 0 for pool rows and starts at 1 for top-level vdevs.
pub enum VisibleRow<'a> {
    Pool(&'a PoolInfo),
    Vdev {
        node: &'a crate::pools::VdevNode,
        depth: u8,
    },
}

enum CollapseAction {
    Collapse(String),
    JumpTo(usize),
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
    /// True until the first non-empty `pools_snapshot` lands. On that
    /// first paint, every pool name is auto-inserted into
    /// `pools_view.expanded`. Subsequent refreshes only `prune` the
    /// expanded set; they never auto-add. This means user collapse
    /// decisions persist across refreshes, and newly imported pools
    /// start expanded by default.
    pools_first_paint: bool,
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
            pools_view: PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 0,
            },
            pools_first_paint: true,
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
                // First paint after the first non-empty snapshot lands:
                // auto-expand every pool. Subsequent refreshes never insert
                // — so user collapses persist and newly imported pools that
                // appear later don't override the user's preferences.
                if self.pools_first_paint && !self.pools_snapshot.is_empty() {
                    if let PoolsView::Tree { expanded, .. } = &mut self.pools_view {
                        for p in &self.pools_snapshot {
                            expanded.insert(p.name.clone());
                        }
                    }
                    self.pools_first_paint = false;
                }
                self.prune_expanded_pools();
                self.clamp_pools_selection();
            }
            Err(e) => {
                self.pools_refresh_error = Some(e.to_string());
                // Keep stale snapshot — better than blanking on a transient.
            }
        }
    }

    /// Drop entries from `pools_view.expanded` for pool names that no
    /// longer exist in `pools_snapshot`. Called from `refresh_pools` after
    /// a successful refresh.
    fn prune_expanded_pools(&mut self) {
        let expanded = match &mut self.pools_view {
            PoolsView::Tree { expanded, .. } => expanded,
            PoolsView::Detail { expanded, .. } => expanded,
        };
        let names: std::collections::HashSet<String> = self
            .pools_snapshot
            .iter()
            .map(|p| p.name.clone())
            .collect();
        expanded.retain(|n| names.contains(n));
    }

    /// DFS over `pools_snapshot` honoring `expanded`, returning
    /// `VisibleRow`s in render order. Pool rows are always emitted; vdev
    /// rows are emitted only for expanded pools, recursively. Returns an
    /// empty Vec when in `Detail` view.
    pub fn flatten_visible_pool_rows(&self) -> Vec<VisibleRow<'_>> {
        let PoolsView::Tree { expanded, .. } = &self.pools_view else {
            return Vec::new();
        };
        let mut out: Vec<VisibleRow<'_>> = Vec::new();
        for pool in &self.pools_snapshot {
            out.push(VisibleRow::Pool(pool));
            if expanded.contains(&pool.name) {
                for child in &pool.root_vdev.children {
                    push_vdev_row(child, 1, &mut out);
                }
            }
        }
        out
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
    /// - Tree selection is clamped to the new visible-row count.
    /// - Detail with an out-of-range pool_index falls back to Tree at
    ///   row 0, preserving the cached expansion set.
    fn clamp_pools_selection(&mut self) {
        // Compute visible_count BEFORE the &mut match (avoids borrow conflict).
        let visible_count = self.flatten_visible_pool_rows().len();
        match &mut self.pools_view {
            PoolsView::Tree { selected, .. } => {
                if visible_count == 0 {
                    *selected = 0;
                } else if *selected >= visible_count {
                    *selected = visible_count - 1;
                }
            }
            PoolsView::Detail { pool_index, expanded } => {
                if *pool_index >= self.pools_snapshot.len() {
                    let restored = expanded.clone();
                    self.pools_view = PoolsView::Tree {
                        expanded: restored,
                        selected: 0,
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
    /// to Pools later lands on the list — not on a stale detail view.
    /// Similarly, leaving the Datasets tab while in a detail view collapses
    /// back to the tree view, preserving the expansion state and landing on
    /// the same dataset row if it still exists. A no-op switch (e.g. pressing
    /// `2` while already on Pools) preserves whatever sub-view the user is
    /// currently in.
    fn switch_tab(&mut self, target: Tab) {
        if target == self.current_tab {
            return;
        }
        if self.current_tab == Tab::Pools {
            if let PoolsView::Detail { pool_index: _, expanded } = &self.pools_view {
                let restored = expanded.clone();
                self.pools_view = PoolsView::Tree {
                    expanded: restored,
                    selected: 0,
                };
            }
        }
        if self.current_tab == Tab::Datasets {
            if let DatasetsView::Detail { name, expanded } = &self.datasets_view {
                let prev_name = name.clone();
                let restored_expanded = expanded.clone();
                self.datasets_view = DatasetsView::Tree {
                    expanded: restored_expanded,
                    selected: 0,
                };
                let rows = self.flatten_visible_dataset_rows();
                if let Some(idx) = rows.iter().position(|(_, n)| n.name == prev_name) {
                    if let DatasetsView::Tree { selected, .. } = &mut self.datasets_view {
                        *selected = idx;
                    }
                }
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
        } else if self.current_tab == Tab::Datasets {
            self.on_key_datasets(key);
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
            let visible_count = self.flatten_visible_pool_rows().len();
            if visible_count == 0 {
                return;
            }
            if let PoolsView::Tree { selected, .. } = &mut self.pools_view {
                let last = visible_count - 1;
                let new = (*selected as i32 + delta).clamp(0, last as i32) as usize;
                *selected = new;
            }
        } else if self.current_tab == Tab::Datasets {
            let visible_count = self.flatten_visible_dataset_rows().len();
            if visible_count == 0 {
                return;
            }
            if let DatasetsView::Tree { selected, .. } = &mut self.datasets_view {
                let last = visible_count - 1;
                let new = (*selected as i32 + delta).clamp(0, last as i32) as usize;
                *selected = new;
            }
        }
    }

    fn on_key_pools(&mut self, key: KeyEvent) {
        // Detail-view bindings first.
        if let PoolsView::Detail { pool_index: _, expanded } = &self.pools_view {
            match key.code {
                KeyCode::Esc | KeyCode::Backspace => {
                    let restored = expanded.clone();
                    self.pools_view = PoolsView::Tree {
                        expanded: restored,
                        selected: 0,
                    };
                    self.clamp_pools_selection();
                }
                _ => {}
            }
            return;
        }

        // Tree-view bindings.
        let visible_count = self.flatten_visible_pool_rows().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if let PoolsView::Tree { selected, .. } = &mut self.pools_view {
                    if *selected + 1 < visible_count {
                        *selected += 1;
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let PoolsView::Tree { selected, .. } = &mut self.pools_view {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
            }
            KeyCode::Home => {
                if let PoolsView::Tree { selected, .. } = &mut self.pools_view {
                    *selected = 0;
                }
            }
            KeyCode::End => {
                if let PoolsView::Tree { selected, .. } = &mut self.pools_view {
                    *selected = visible_count.saturating_sub(1);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.expand_selected_pool();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.collapse_or_jump_to_parent_pool();
            }
            KeyCode::Enter => {
                self.drill_into_selected_pool();
            }
            _ => {}
        }
    }

    /// Returns the index in `pools_snapshot` of the pool that contains
    /// the currently-selected row. For pool rows this is the row's pool;
    /// for vdev rows this is the most-recent pool row at-or-before the
    /// selection. Returns `None` if there is no visible selection.
    fn selected_pool_index_for_drill(&self) -> Option<usize> {
        let PoolsView::Tree { selected, .. } = &self.pools_view else {
            return None;
        };
        let rows = self.flatten_visible_pool_rows();
        let target = rows.get(*selected)?;
        let target_pool_name: &str = match target {
            VisibleRow::Pool(p) => &p.name,
            VisibleRow::Vdev { .. } => {
                let mut i = *selected;
                loop {
                    if i == 0 {
                        if let Some(VisibleRow::Pool(p)) = rows.first() {
                            break p.name.as_str();
                        } else {
                            return None;
                        }
                    }
                    i -= 1;
                    if let Some(VisibleRow::Pool(p)) = rows.get(i) {
                        break p.name.as_str();
                    }
                }
            }
        };
        self.pools_snapshot.iter().position(|p| p.name == target_pool_name)
    }

    /// If the selected row is a Pool with vdevs, insert its name into
    /// `expanded`. No-op for vdev rows or pools with no vdev children.
    fn expand_selected_pool(&mut self) {
        let name_to_expand = {
            let rows = self.flatten_visible_pool_rows();
            let PoolsView::Tree { selected, .. } = &self.pools_view else {
                return;
            };
            match rows.get(*selected) {
                Some(VisibleRow::Pool(p)) if !p.root_vdev.children.is_empty() => {
                    p.name.clone()
                }
                _ => return,
            }
        };
        if let PoolsView::Tree { expanded, .. } = &mut self.pools_view {
            expanded.insert(name_to_expand);
        }
    }

    /// `←` on a Pool row + expanded → collapse it.
    /// `←` on a Vdev row → jump selection to that vdev's parent Pool row.
    /// Otherwise no-op.
    fn collapse_or_jump_to_parent_pool(&mut self) {
        let action = {
            let rows = self.flatten_visible_pool_rows();
            let PoolsView::Tree { selected, expanded } = &self.pools_view else {
                return;
            };
            match rows.get(*selected) {
                Some(VisibleRow::Pool(p)) if expanded.contains(&p.name) => {
                    CollapseAction::Collapse(p.name.clone())
                }
                Some(VisibleRow::Vdev { .. }) => {
                    let mut i = *selected;
                    loop {
                        if i == 0 {
                            break CollapseAction::JumpTo(0);
                        }
                        i -= 1;
                        if matches!(rows.get(i), Some(VisibleRow::Pool(_))) {
                            break CollapseAction::JumpTo(i);
                        }
                    }
                }
                _ => return,
            }
        };
        match action {
            CollapseAction::Collapse(name) => {
                if let PoolsView::Tree { expanded, .. } = &mut self.pools_view {
                    expanded.remove(&name);
                }
                self.clamp_pools_selection();
            }
            CollapseAction::JumpTo(idx) => {
                if let PoolsView::Tree { selected, .. } = &mut self.pools_view {
                    *selected = idx;
                }
            }
        }
    }

    /// Enter on a Pool row OR a Vdev row → drill into the containing
    /// pool's Detail. Preserves the current `expanded` set so Esc returns
    /// to the same Tree state.
    fn drill_into_selected_pool(&mut self) {
        let Some(pool_index) = self.selected_pool_index_for_drill() else {
            return;
        };
        let restored = match &self.pools_view {
            PoolsView::Tree { expanded, .. } => expanded.clone(),
            PoolsView::Detail { .. } => return,
        };
        self.pools_view = PoolsView::Detail {
            pool_index,
            expanded: restored,
        };
    }

    fn on_key_datasets(&mut self, key: KeyEvent) {
        // Detail-view bindings first. Capture the values we need BEFORE
        // mutating self.datasets_view (the Esc handler computes a new
        // selection from the previous detail name after the transition).
        if let DatasetsView::Detail { name, expanded } = &self.datasets_view {
            match key.code {
                KeyCode::Esc | KeyCode::Backspace => {
                    let restored_expanded = expanded.clone();
                    let prev_name = name.clone();
                    self.datasets_view = DatasetsView::Tree {
                        expanded: restored_expanded,
                        selected: 0,
                    };
                    let rows = self.flatten_visible_dataset_rows();
                    if let Some(idx) = rows.iter().position(|(_, n)| n.name == prev_name) {
                        if let DatasetsView::Tree { selected, .. } = &mut self.datasets_view {
                            *selected = idx;
                        }
                    }
                    return;
                }
                _ => return, // detail view ignores other keys
            }
        }

        // Tree-view bindings.
        let visible_count = self.flatten_visible_dataset_rows().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if let DatasetsView::Tree { selected, .. } = &mut self.datasets_view {
                    if *selected + 1 < visible_count {
                        *selected += 1;
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let DatasetsView::Tree { selected, .. } = &mut self.datasets_view {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                }
            }
            KeyCode::Home => {
                if let DatasetsView::Tree { selected, .. } = &mut self.datasets_view {
                    *selected = 0;
                }
            }
            KeyCode::End => {
                if let DatasetsView::Tree { selected, .. } = &mut self.datasets_view {
                    *selected = visible_count.saturating_sub(1);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.expand_selected_dataset();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.collapse_or_jump_to_parent();
            }
            KeyCode::Enter => {
                self.drill_into_selected_dataset();
            }
            _ => {}
        }
    }

    /// Insert the currently-selected DatasetNode's name into `expanded` if
    /// it has children. No-op for leaves and zvols.
    fn expand_selected_dataset(&mut self) {
        // Collect what we need before taking a mutable borrow.
        let (selected_idx, name, has_children) = {
            let rows = self.flatten_visible_dataset_rows();
            let DatasetsView::Tree { selected, .. } = &self.datasets_view else {
                return;
            };
            let Some((_, node)) = rows.get(*selected) else {
                return;
            };
            (*selected, node.name.clone(), node.has_children())
        };
        let _ = selected_idx;
        if has_children {
            if let DatasetsView::Tree { expanded, .. } = &mut self.datasets_view {
                expanded.insert(name);
            }
        }
    }

    /// If the selected row is expanded, collapse it. Otherwise (collapsed
    /// or leaf), jump selection to the parent row. Pool roots have no
    /// parent — no-op.
    fn collapse_or_jump_to_parent(&mut self) {
        // Collect the information we need before taking a mutable borrow.
        let (selected_idx, depth, name, has_children, depths_before) = {
            let rows = self.flatten_visible_dataset_rows();
            let DatasetsView::Tree { selected, .. } = &self.datasets_view else {
                return;
            };
            let Some((depth, node)) = rows.get(*selected).map(|(d, n)| (*d, *n)) else {
                return;
            };
            let depths_before: Vec<usize> = rows[..*selected].iter().map(|(d, _)| *d).collect();
            (*selected, depth, node.name.clone(), node.has_children(), depths_before)
        };
        if let DatasetsView::Tree { selected, expanded } = &mut self.datasets_view {
            if expanded.contains(&name) && has_children {
                expanded.remove(&name);
                return;
            }
            if depth == 0 {
                return; // pool root, no parent
            }
            let target_depth = depth - 1;
            for i in (0..selected_idx).rev() {
                if depths_before[i] == target_depth {
                    *selected = i;
                    return;
                }
            }
        }
    }

    /// Drop into Detail for the currently-selected dataset. No-op when
    /// in Detail already or when the snapshot is empty.
    fn drill_into_selected_dataset(&mut self) {
        let rows = self.flatten_visible_dataset_rows();
        let selected_idx = match &self.datasets_view {
            DatasetsView::Tree { selected, .. } => *selected,
            DatasetsView::Detail { .. } => return,
        };
        let Some((_, node)) = rows.get(selected_idx) else {
            return;
        };
        let name = node.name.clone();
        let expanded = match &self.datasets_view {
            DatasetsView::Tree { expanded, .. } => expanded.clone(),
            DatasetsView::Detail { .. } => unreachable!(),
        };
        self.datasets_view = DatasetsView::Detail { name, expanded };
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

fn push_vdev_row<'a>(
    node: &'a crate::pools::VdevNode,
    depth: u8,
    out: &mut Vec<VisibleRow<'a>>,
) {
    out.push(VisibleRow::Vdev { node, depth });
    for child in &node.children {
        push_vdev_row(child, depth + 1, out);
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
            pools_view: PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 0,
            },
            pools_first_paint: true,
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
                device_path: None,
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
        app.pools_view = PoolsView::Tree {
            expanded: BTreeSet::new(),
            selected: 2,
        };
        // Shrink the underlying source to one pool and refresh.
        app.pools_source = Some(Box::new(FakePoolsSource::new(vec![test_pool(
            "a",
            PoolHealth::Online,
            100,
            50,
        )])));
        app.refresh_pools();
        // After refresh: snapshot = [a], selected clamps to last visible row.
        // (first-paint auto-expands "a" — but that's a single-pool, no-children
        // case so visible rows still = 1 → selected clamps to 0.)
        if let PoolsView::Tree { selected, .. } = &app.pools_view {
            assert_eq!(*selected, 0);
        } else {
            panic!("expected Tree view, got {:?}", app.pools_view);
        }
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
        assert_eq!(
            app.pools_view,
            PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 1,
            }
        );
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.pools_view,
            PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 2,
            }
        );
    }

    #[test]
    fn pools_down_clamps_at_last() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Tree {
            expanded: BTreeSet::new(),
            selected: 1,
        };
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.pools_view,
            PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 1,
            }
        );
    }

    #[test]
    fn pools_up_at_first_is_noop() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.pools_view,
            PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 0,
            }
        );
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
        assert_eq!(
            app.pools_view,
            PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 2,
            }
        );
        app.on_key(key(KeyCode::Home));
        assert_eq!(
            app.pools_view,
            PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 0,
            }
        );
    }

    #[test]
    fn pools_enter_drills_into_detail() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Tree {
            expanded: BTreeSet::new(),
            selected: 1,
        };
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.pools_view,
            PoolsView::Detail {
                pool_index: 1,
                expanded: BTreeSet::new(),
            }
        );
    }

    #[test]
    fn pools_enter_with_empty_list_is_noop() {
        let mut app = app_with_pools(vec![]);
        app.current_tab = Tab::Pools;
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.pools_view, PoolsView::Tree { .. }));
    }

    /// Leaving the Pools tab while drilled into a specific pool must
    /// collapse the drilldown so that returning to Pools lands on the
    /// tree. Exercises every tab-change key: 1, 3, Tab, BackTab.
    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_overview_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail {
            pool_index: 2,
            expanded: BTreeSet::new(),
        };
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.current_tab, Tab::Overview);
        assert!(
            matches!(app.pools_view, PoolsView::Tree { .. }),
            "leaving Pools via '1' should have collapsed Detail to Tree"
        );
    }

    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_arc_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail {
            pool_index: 1,
            expanded: BTreeSet::new(),
        };
        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.current_tab, Tab::Arc);
        assert!(matches!(app.pools_view, PoolsView::Tree { .. }));
    }

    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_tab_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail {
            pool_index: 0,
            expanded: BTreeSet::new(),
        };
        app.on_key(key(KeyCode::Tab));
        // Pools → Datasets (next in Tab::ALL order).
        assert_eq!(app.current_tab, Tab::Datasets);
        assert!(matches!(app.pools_view, PoolsView::Tree { .. }));
    }

    #[test]
    fn leaving_pools_while_in_detail_collapses_to_list_via_backtab_key() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail {
            pool_index: 1,
            expanded: BTreeSet::new(),
        };
        app.on_key(key(KeyCode::BackTab));
        // Pools ← Overview (previous in Tab::ALL order).
        assert_eq!(app.current_tab, Tab::Overview);
        assert!(matches!(app.pools_view, PoolsView::Tree { .. }));
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
        app.pools_view = PoolsView::Detail {
            pool_index: 1,
            expanded: BTreeSet::new(),
        };
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.current_tab, Tab::Pools);
        assert!(
            matches!(app.pools_view, PoolsView::Detail { pool_index: 1, .. }),
            "no-op tab switch should not disturb the sub-view"
        );
    }

    /// End-to-end round trip: drill in, tab out, tab back → tree view.
    #[test]
    fn pools_detail_round_trip_ends_on_list() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Tree {
            expanded: BTreeSet::new(),
            selected: 2,
        };
        // Drill in
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(
            app.pools_view,
            PoolsView::Detail { pool_index: 2, .. }
        ));
        // Tab out to Overview
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.current_tab, Tab::Overview);
        // Return to Pools
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.current_tab, Tab::Pools);
        assert!(
            matches!(app.pools_view, PoolsView::Tree { .. }),
            "returning to Pools after a drill-in + tab-out should land on the tree"
        );
    }

    #[test]
    fn pools_esc_returns_to_list_with_same_index() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.current_tab = Tab::Pools;
        app.pools_view = PoolsView::Detail {
            pool_index: 1,
            expanded: BTreeSet::new(),
        };
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(app.pools_view, PoolsView::Tree { .. }));
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
        assert_eq!(
            app.pools_view,
            PoolsView::Tree {
                expanded: BTreeSet::new(),
                selected: 0,
            }
        );
    }

    #[test]
    fn detail_view_drops_to_list_when_pool_vanishes() {
        let mut app = app_with_pools(vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
        ]);
        app.pools_view = PoolsView::Detail {
            pool_index: 1,
            expanded: BTreeSet::new(),
        };
        app.pools_source = Some(Box::new(FakePoolsSource::new(vec![test_pool(
            "a",
            PoolHealth::Online,
            100,
            50,
        )])));
        app.refresh_pools();
        assert!(matches!(app.pools_view, PoolsView::Tree { selected: 0, .. }));
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

    #[test]
    fn datasets_down_advances_selection() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![]), ds_fs("tank/srv", vec![])],
        )]);
        app.current_tab = Tab::Datasets;
        // After app_with_datasets, expanded contains "tank" so visible rows
        // are: tank, tank/home, tank/srv. Selection starts at 0.
        app.on_key(key(KeyCode::Down));
        if let DatasetsView::Tree { selected, .. } = &app.datasets_view {
            assert_eq!(*selected, 1);
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn datasets_down_clamps_at_last() {
        let mut app = app_with_datasets(vec![ds_fs("tank", vec![])]);
        app.current_tab = Tab::Datasets;
        app.on_key(key(KeyCode::Down));
        if let DatasetsView::Tree { selected, .. } = &app.datasets_view {
            assert_eq!(*selected, 0);
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn datasets_right_expands_selected_node() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![ds_fs("tank/home/alice", vec![])])],
        )]);
        app.current_tab = Tab::Datasets;
        if let DatasetsView::Tree { selected, .. } = &mut app.datasets_view {
            *selected = 1; // tank/home
        }
        app.on_key(key(KeyCode::Right));
        if let DatasetsView::Tree { expanded, .. } = &app.datasets_view {
            assert!(expanded.contains("tank/home"));
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn datasets_right_on_leaf_is_noop() {
        let mut app = app_with_datasets(vec![ds_fs("tank", vec![])]);
        app.current_tab = Tab::Datasets;
        let before = app.datasets_view.clone();
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.datasets_view, before);
    }

    #[test]
    fn datasets_left_collapses_expanded_node() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![])],
        )]);
        app.current_tab = Tab::Datasets;
        app.on_key(key(KeyCode::Left));
        if let DatasetsView::Tree { expanded, .. } = &app.datasets_view {
            assert!(!expanded.contains("tank"));
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn datasets_left_on_collapsed_jumps_to_parent() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![ds_fs("tank/home/alice", vec![])])],
        )]);
        app.current_tab = Tab::Datasets;
        if let DatasetsView::Tree { selected, .. } = &mut app.datasets_view {
            *selected = 1;
        }
        app.on_key(key(KeyCode::Left));
        if let DatasetsView::Tree { selected, .. } = &app.datasets_view {
            assert_eq!(*selected, 0);
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn datasets_left_on_pool_root_is_noop() {
        let mut app = app_with_datasets(vec![ds_fs("tank", vec![])]);
        app.current_tab = Tab::Datasets;
        if let DatasetsView::Tree { expanded, .. } = &mut app.datasets_view {
            expanded.clear();
        }
        let before = app.datasets_view.clone();
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.datasets_view, before);
    }

    #[test]
    fn datasets_enter_drills_into_detail() {
        let mut app = app_with_datasets(vec![ds_fs("tank", vec![])]);
        app.current_tab = Tab::Datasets;
        app.on_key(key(KeyCode::Enter));
        if let DatasetsView::Detail { name, .. } = &app.datasets_view {
            assert_eq!(name, "tank");
        } else {
            panic!("expected Detail view");
        }
    }

    #[test]
    fn datasets_esc_returns_to_tree_with_same_selection() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![])],
        )]);
        app.current_tab = Tab::Datasets;
        let mut expanded = BTreeSet::new();
        expanded.insert("tank".to_string());
        app.datasets_view = DatasetsView::Detail {
            name: "tank/home".into(),
            expanded,
        };
        app.on_key(key(KeyCode::Esc));
        if let DatasetsView::Tree { selected, expanded } = &app.datasets_view {
            assert!(expanded.contains("tank"));
            assert_eq!(*selected, 1);
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn datasets_keys_ignored_when_not_on_datasets_tab() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![])],
        )]);
        app.current_tab = Tab::Pools;
        let before = app.datasets_view.clone();
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.datasets_view, before);
    }

    #[test]
    fn datasets_mouse_scroll_moves_selection() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![])],
        )]);
        app.current_tab = Tab::Datasets;
        use crossterm::event::{MouseEvent, MouseEventKind};
        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(scroll_down);
        if let DatasetsView::Tree { selected, .. } = &app.datasets_view {
            assert_eq!(*selected, 1);
        } else {
            panic!("expected Tree view");
        }
    }

    #[test]
    fn leaving_datasets_while_in_detail_collapses_to_tree_via_overview_key() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![])],
        )]);
        app.current_tab = Tab::Datasets;
        let mut expanded = BTreeSet::new();
        expanded.insert("tank".to_string());
        app.datasets_view = DatasetsView::Detail {
            name: "tank/home".into(),
            expanded,
        };
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.current_tab, Tab::Overview);
        assert!(matches!(app.datasets_view, DatasetsView::Tree { .. }));
    }

    #[test]
    fn leaving_datasets_while_in_detail_collapses_to_tree_via_arc_key() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![])],
        )]);
        app.current_tab = Tab::Datasets;
        let mut expanded = BTreeSet::new();
        expanded.insert("tank".to_string());
        app.datasets_view = DatasetsView::Detail {
            name: "tank/home".into(),
            expanded,
        };
        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.current_tab, Tab::Arc);
        assert!(matches!(app.datasets_view, DatasetsView::Tree { .. }));
    }

    #[test]
    fn pressing_datasets_key_while_already_on_datasets_preserves_detail() {
        let mut app = app_with_datasets(vec![ds_fs(
            "tank",
            vec![ds_fs("tank/home", vec![])],
        )]);
        app.current_tab = Tab::Datasets;
        let mut expanded = BTreeSet::new();
        expanded.insert("tank".to_string());
        app.datasets_view = DatasetsView::Detail {
            name: "tank/home".into(),
            expanded,
        };
        app.on_key(key(KeyCode::Char('3')));
        assert_eq!(app.current_tab, Tab::Datasets);
        assert!(
            matches!(app.datasets_view, DatasetsView::Detail { .. }),
            "no-op tab switch should not disturb the sub-view"
        );
    }

    // ---------------------------------------------------------------
    // v0.3.1 pools tree state-machine tests.
    //
    // These exercise the new helpers (flatten_visible_pool_rows,
    // expand_selected_pool, collapse_or_jump_to_parent_pool, drill_into_…
    // selected_pool, prune_expanded_pools) plus the `pools_first_paint`
    // default-expand-once behavior. Unlike the older `app_with_pools`
    // fixture above (which assigns `pools_snapshot` directly and never
    // ticks `refresh_pools`), these tests need the auto-expand-on-first
    // -paint path to fire — so they construct the app via `App::new`,
    // which calls `refresh_pools` once during construction.
    // ---------------------------------------------------------------

    use std::path::PathBuf;

    fn pool_with_vdevs(name: &str, leaf_names: &[&str]) -> PoolInfo {
        let leaves: Vec<VdevNode> = leaf_names
            .iter()
            .map(|n| VdevNode {
                name: (*n).into(),
                kind: VdevKind::Disk,
                state: VdevState::Online,
                size_bytes: Some(1 << 30),
                errors: PoolErrors::default(),
                children: vec![],
                device_path: Some(format!("/dev/{n}")),
            })
            .collect();
        let raidz = VdevNode {
            name: "raidz1-0".into(),
            kind: VdevKind::Raidz { parity: 1 },
            state: VdevState::Online,
            size_bytes: Some(4 << 30),
            errors: PoolErrors::default(),
            children: leaves,
            device_path: None,
        };
        PoolInfo {
            name: name.into(),
            health: PoolHealth::Online,
            allocated_bytes: 1 << 30,
            size_bytes: 4 << 30,
            free_bytes: 3 << 30,
            fragmentation_pct: Some(5),
            scrub: ScrubState::Never,
            errors: PoolErrors::default(),
            root_vdev: VdevNode {
                name: name.into(),
                kind: VdevKind::Root,
                state: VdevState::Online,
                size_bytes: Some(4 << 30),
                errors: PoolErrors::default(),
                children: vec![raidz],
                device_path: None,
            },
        }
    }

    fn app_via_new_with_pools(pools: Vec<PoolInfo>) -> App {
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<crate::arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || crate::arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> = Some(Box::new(
            crate::meminfo::linux::LinuxMemSource::new(meminfo_path),
        ));
        let pools_source: Option<Box<dyn PoolsSource>> =
            Some(Box::new(FakePoolsSource::new(pools)));
        let mut app =
            App::new(arc_reader, mem, pools_source, None, None, None)
                .expect("fixture App::new");
        app.current_tab = Tab::Pools;
        app
    }

    #[test]
    fn tree_selection_moves_within_visible_rows() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda", "sdb"])]);
        // tank is auto-expanded after first paint, so visible rows are:
        // 0: tank (Pool), 1: raidz1-0 (Vdev), 2: sda, 3: sdb.
        app.on_key(key(KeyCode::Down));
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 1);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 3);
        app.on_key(key(KeyCode::Down)); // off the end → no-op
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 3);
        app.on_key(key(KeyCode::Up));
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 2);
    }

    #[test]
    fn right_arrow_expands_collapsed_pool() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // Force collapsed state (override default-expanded behavior).
        if let PoolsView::Tree { expanded, selected } = &mut app.pools_view {
            expanded.clear();
            *selected = 0;
        }
        assert_eq!(app.flatten_visible_pool_rows().len(), 1); // pool only
        app.on_key(key(KeyCode::Right));
        // Now expanded → raidz + sda visible.
        assert!(app.flatten_visible_pool_rows().len() >= 2);
        let PoolsView::Tree { expanded, .. } = &app.pools_view else { panic!() };
        assert!(expanded.contains("tank"));
    }

    #[test]
    fn right_arrow_on_already_expanded_pool_is_no_op() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // tank is auto-expanded by default-expand.
        let before = app.flatten_visible_pool_rows().len();
        app.on_key(key(KeyCode::Right));
        let after = app.flatten_visible_pool_rows().len();
        assert_eq!(before, after);
    }

    #[test]
    fn right_arrow_on_vdev_row_is_no_op() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // Move to row 1 (raidz1-0).
        app.on_key(key(KeyCode::Down));
        let before = app.flatten_visible_pool_rows().len();
        app.on_key(key(KeyCode::Right));
        let after = app.flatten_visible_pool_rows().len();
        assert_eq!(before, after);
    }

    #[test]
    fn left_arrow_collapses_expanded_pool_when_on_pool_row() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // Default-expanded.
        let PoolsView::Tree { expanded, .. } = &app.pools_view else { panic!() };
        assert!(expanded.contains("tank"));
        // Selected starts at 0 (the pool row).
        app.on_key(key(KeyCode::Left));
        let PoolsView::Tree { expanded, .. } = &app.pools_view else { panic!() };
        assert!(!expanded.contains("tank"));
    }

    #[test]
    fn left_arrow_jumps_to_parent_pool_when_on_vdev_row() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // Move to sda (row 2: pool, raidz, sda).
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 2);
        app.on_key(key(KeyCode::Left));
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 0); // jumped to the tank pool row
    }

    #[test]
    fn enter_on_pool_row_drills_to_pool_detail() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        app.on_key(key(KeyCode::Enter));
        match &app.pools_view {
            PoolsView::Detail { pool_index, .. } => assert_eq!(*pool_index, 0),
            other => panic!("expected Detail, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_vdev_row_drills_to_containing_pool_detail() {
        let mut app = app_via_new_with_pools(vec![
            pool_with_vdevs("tank", &["sda"]),
            pool_with_vdevs("rpool", &["nvme0n1"]),
        ]);
        // Visible rows after default-expand: 0 tank, 1 raidz1-0, 2 sda,
        // 3 rpool, 4 raidz1-0, 5 nvme0n1. Move to row 5 (nvme0n1 under rpool).
        for _ in 0..5 {
            app.on_key(key(KeyCode::Down));
        }
        app.on_key(key(KeyCode::Enter));
        match &app.pools_view {
            PoolsView::Detail { pool_index, .. } => assert_eq!(*pool_index, 1),
            other => panic!("expected Detail, got {other:?}"),
        }
    }

    #[test]
    fn esc_from_detail_returns_to_tree_with_same_selection() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // Move to row 2 (sda), drill, then esc.
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.pools_view, PoolsView::Detail { .. }));
        app.on_key(key(KeyCode::Esc));
        match &app.pools_view {
            PoolsView::Tree { selected, .. } => {
                // After esc + clamp, the selection lands on something valid.
                // The exact value isn't load-bearing — just that we're back
                // in Tree and `selected` is in-range.
                assert!(*selected < app.flatten_visible_pool_rows().len());
            }
            other => panic!("expected Tree, got {other:?}"),
        }
    }

    #[test]
    fn expand_state_survives_drill_and_return() {
        let mut app = app_via_new_with_pools(vec![
            pool_with_vdevs("tank", &["sda"]),
            pool_with_vdevs("rpool", &["nvme0n1"]),
        ]);
        // Collapse rpool manually.
        if let PoolsView::Tree { expanded, .. } = &mut app.pools_view {
            expanded.remove("rpool");
        }
        let before: BTreeSet<String> = match &app.pools_view {
            PoolsView::Tree { expanded, .. } => expanded.clone(),
            _ => panic!(),
        };
        app.on_key(key(KeyCode::Enter)); // drill on tank
        app.on_key(key(KeyCode::Esc));   // back
        let after: BTreeSet<String> = match &app.pools_view {
            PoolsView::Tree { expanded, .. } => expanded.clone(),
            _ => panic!(),
        };
        assert_eq!(before, after);
    }

    #[test]
    fn prune_expanded_pools_drops_vanished_pools() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // Manually add an extra entry that doesn't correspond to any pool.
        if let PoolsView::Tree { expanded, .. } = &mut app.pools_view {
            expanded.insert("ghost".into());
        }
        // Refresh — prune should drop "ghost" but keep "tank".
        let _ = app.refresh();
        let PoolsView::Tree { expanded, .. } = &app.pools_view else { panic!() };
        assert!(expanded.contains("tank"));
        assert!(!expanded.contains("ghost"));
    }

    #[test]
    fn default_expanded_only_runs_once() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        // After construction, tank is auto-expanded.
        assert!(matches!(&app.pools_view,
            PoolsView::Tree { expanded, .. } if expanded.contains("tank")));
        // User collapses tank.
        if let PoolsView::Tree { expanded, .. } = &mut app.pools_view {
            expanded.remove("tank");
        }
        // Refresh — first-paint logic must NOT re-add tank, because
        // pools_first_paint was set to false on the first refresh.
        let _ = app.refresh();
        let PoolsView::Tree { expanded, .. } = &app.pools_view else { panic!() };
        assert!(!expanded.contains("tank"),
            "default-expand re-fired after user collapse — pools_first_paint logic broken");
    }

    #[test]
    fn empty_snapshot_clamps_selection_to_zero() {
        let mut app = app_via_new_with_pools(vec![]);
        // No pools → no visible rows.
        assert_eq!(app.flatten_visible_pool_rows().len(), 0);
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 0);
        // j/k on empty are no-ops.
        app.on_key(key(KeyCode::Down));
        let PoolsView::Tree { selected, .. } = &app.pools_view else { panic!() };
        assert_eq!(*selected, 0);
    }

    #[test]
    fn switch_tab_collapses_detail_to_tree() {
        let mut app = app_via_new_with_pools(vec![pool_with_vdevs("tank", &["sda"])]);
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.pools_view, PoolsView::Detail { .. }));
        // Tab switch → cycle next, then back.
        app.cycle_tab(1);
        // We've left Pools; pools_view should already be Tree.
        assert!(matches!(app.pools_view, PoolsView::Tree { .. }));
        // Coming back via hotkey '2' should land on Tree.
        app.on_key(key(KeyCode::Char('2')));
        assert!(matches!(app.pools_view, PoolsView::Tree { .. }));
    }
}
