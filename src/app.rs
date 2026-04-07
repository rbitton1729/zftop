// App state and update logic.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

use crate::arcstats::ArcStats;
use crate::meminfo::MemInfo;

const DEFAULT_MEMINFO: &str = "/proc/meminfo";

pub struct App {
    pub current: ArcStats,
    pub previous: Option<ArcStats>,
    pub meminfo: Option<MemInfo>,
    pub meminfo_source: PathBuf,
    pub source: PathBuf,
    pub should_quit: bool,
}

pub struct BreakdownRow {
    pub label: &'static str,
    pub bytes: u64,
    pub pct: f64,
}

impl App {
    pub fn new(source: PathBuf, meminfo_source: Option<PathBuf>) -> Result<Self> {
        let current = ArcStats::from_path(&source)?;
        let meminfo_path = meminfo_source.unwrap_or_else(|| PathBuf::from(DEFAULT_MEMINFO));
        let meminfo = MemInfo::from_path(&meminfo_path).ok();
        Ok(Self {
            current,
            previous: None,
            meminfo,
            meminfo_source: meminfo_path,
            source,
            should_quit: false,
        })
    }

    pub fn refresh(&mut self) -> Result<()> {
        let next = ArcStats::from_path(&self.source)?;
        self.previous = Some(std::mem::replace(&mut self.current, next));
        self.meminfo = MemInfo::from_path(&self.meminfo_source).ok();
        Ok(())
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('r') => {
                let _ = self.refresh();
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

    /// RAM breakdown segments (in KiB) for the stacked bar.
    /// Returns: (app_used, buffers_cache, arc, free) — all in KiB.
    pub fn ram_segments(&self) -> Option<(u64, u64, u64, u64)> {
        let m = self.meminfo.as_ref()?;
        let arc_kb = self.current.size / 1024;
        let app_used = m.app_used(self.current.size);
        let buf_cache = m.buffers + m.cached;
        let free = m.free;
        Some((app_used, buf_cache, arc_kb, free))
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

    #[test]
    fn overall_hit_ratio() {
        let app = App {
            current: sample_stats(),
            previous: None,
            meminfo: None,
            meminfo_source: PathBuf::new(),
            source: PathBuf::new(),
            should_quit: false,
        };
        assert!((app.hit_ratio_overall() - 90.0).abs() < 0.01);
    }

    #[test]
    fn demand_hit_ratio() {
        let app = App {
            current: sample_stats(),
            previous: None,
            meminfo: None,
            meminfo_source: PathBuf::new(),
            source: PathBuf::new(),
            should_quit: false,
        };
        // (5000+3000) / (5000+3000+500+300) = 8000/8800 ≈ 90.909%
        assert!((app.hit_ratio_demand() - 90.909).abs() < 0.01);
    }

    #[test]
    fn prefetch_hit_ratio() {
        let app = App {
            current: sample_stats(),
            previous: None,
            meminfo: None,
            meminfo_source: PathBuf::new(),
            source: PathBuf::new(),
            should_quit: false,
        };
        // (800+200) / (800+200+150+50) = 1000/1200 ≈ 83.333%
        assert!((app.hit_ratio_prefetch() - 83.333).abs() < 0.01);
    }

    #[test]
    fn throughput_none_without_previous() {
        let app = App {
            current: sample_stats(),
            previous: None,
            meminfo: None,
            meminfo_source: PathBuf::new(),
            source: PathBuf::new(),
            should_quit: false,
        };
        assert!(app.throughput_hits().is_none());
        assert!(app.throughput_misses().is_none());
    }

    #[test]
    fn throughput_delta() {
        let mut prev = sample_stats();
        prev.hits = 8000;
        prev.misses = 900;
        let app = App {
            current: sample_stats(),
            previous: Some(prev),
            meminfo: None,
            meminfo_source: PathBuf::new(),
            source: PathBuf::new(),
            should_quit: false,
        };
        assert_eq!(app.throughput_hits(), Some(1000));
        assert_eq!(app.throughput_misses(), Some(100));
    }

    #[test]
    fn arc_usage() {
        let app = App {
            current: sample_stats(),
            previous: None,
            meminfo: None,
            meminfo_source: PathBuf::new(),
            source: PathBuf::new(),
            should_quit: false,
        };
        assert!((app.arc_usage_pct() - 0.625).abs() < 0.001);
    }

    #[test]
    fn breakdown_has_expected_categories() {
        let app = App {
            current: sample_stats(),
            previous: None,
            meminfo: None,
            meminfo_source: PathBuf::new(),
            source: PathBuf::new(),
            should_quit: false,
        };
        let rows = app.arc_breakdown();
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        assert!(labels.contains(&"MFU data"));
        assert!(labels.contains(&"MRU data"));
        assert!(labels.contains(&"Anon"));
        assert!(labels.contains(&"Headers"));
        // All percentages should be relative to total size
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
}
