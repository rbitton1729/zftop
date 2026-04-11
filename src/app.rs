// App state and update logic.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

use crate::arcstats::ArcStats;
use crate::meminfo::{MemSnapshot, MemSource, RamSegment};

/// Top-level navigation tab. v0.2b ships all three variants but only the ARC
/// tab has real content; Overview and Pools render placeholders until v0.2c.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Arc,
    Pools,
}

impl Tab {
    /// Iteration order for the tab strip and for `cycle_tab`. The order here
    /// is the order the tabs appear left-to-right on screen and the order
    /// `Tab` / `Shift+Tab` cycle through them.
    pub const ALL: &'static [Tab] = &[Tab::Overview, Tab::Arc, Tab::Pools];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Arc => "ARC",
            Tab::Pools => "Pools",
        }
    }

    /// Hotkey character bound to this tab. Used by the tab strip renderer
    /// to show the key binding next to each tab label.
    pub fn hotkey(&self) -> char {
        match self {
            Tab::Overview => '1',
            Tab::Arc => '2',
            Tab::Pools => '3',
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
    /// Currently-selected top-level tab. Defaults to `Tab::Arc` in v0.2b
    /// (preserves v0.1 launch experience). Plan v0.2c flips the default to
    /// `Tab::Overview` once Overview has real content.
    pub current_tab: Tab,
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
    ) -> Result<Self> {
        let current = arc_reader()?;
        let arc_segs = arc_segments(&current);
        let mem_snapshot = mem_source.as_mut().and_then(|s| s.snapshot(&arc_segs));
        Ok(Self {
            current,
            previous: None,
            arc_reader,
            mem_source,
            mem_snapshot,
            should_quit: false,
            current_tab: Tab::Arc,
        })
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
        self.current_tab = all[next_idx as usize];
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
        Ok(())
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
                self.current_tab = Tab::Overview;
                return;
            }
            KeyCode::Char('2') => {
                self.current_tab = Tab::Arc;
                return;
            }
            KeyCode::Char('3') => {
                self.current_tab = Tab::Pools;
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

        // Per-tab bindings — plan v0.2c will dispatch pools-list selection,
        // drilldown, escape-to-list, etc. here. Nothing to dispatch yet.
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
            current_tab: Tab::Arc,
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
    fn tab_all_ordered_overview_arc_pools() {
        assert_eq!(Tab::ALL, &[Tab::Overview, Tab::Arc, Tab::Pools]);
    }

    #[test]
    fn tab_titles_stable() {
        assert_eq!(Tab::Overview.title(), "Overview");
        assert_eq!(Tab::Arc.title(), "ARC");
        assert_eq!(Tab::Pools.title(), "Pools");
    }

    #[test]
    fn tab_hotkeys_are_1_2_3_in_order() {
        assert_eq!(Tab::Overview.hotkey(), '1');
        assert_eq!(Tab::Arc.hotkey(), '2');
        assert_eq!(Tab::Pools.hotkey(), '3');
    }

    #[test]
    fn cycle_tab_forward_wraps() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.cycle_tab(1);
        assert_eq!(app.current_tab, Tab::Arc);
        app.cycle_tab(1);
        assert_eq!(app.current_tab, Tab::Pools);
        app.cycle_tab(1); // wraps
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn cycle_tab_back_wraps() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.cycle_tab(-1); // wraps
        assert_eq!(app.current_tab, Tab::Pools);
        app.cycle_tab(-1);
        assert_eq!(app.current_tab, Tab::Arc);
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
    fn hotkey_2_switches_to_arc() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.current_tab, Tab::Arc);
    }

    #[test]
    fn hotkey_3_switches_to_pools() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Arc;
        app.on_key(key(KeyCode::Char('3')));
        assert_eq!(app.current_tab, Tab::Pools);
    }

    #[test]
    fn tab_key_cycles_forward() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Arc);
    }

    #[test]
    fn back_tab_cycles_backward() {
        let mut app = app_with(sample_stats(), None);
        app.current_tab = Tab::Overview;
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Pools);
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

        let app = App::new(arc_reader, mem_source).expect("App::new should succeed");
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
}
