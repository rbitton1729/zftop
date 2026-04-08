// Linux source: parse /proc/meminfo into a `MemInfo` and assemble a
// `LinuxMemSource` that produces RAM bar snapshots in the htop-style
// App / ARC / Buf-Cache layout.

use anyhow::{Context, Result};
use ratatui::style::Color;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::{MemSnapshot, MemSource, RamSegment};

#[derive(Debug, Clone)]
pub struct MemInfo {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub s_reclaimable: u64,
}

impl MemInfo {
    pub fn from_path(path: &Path) -> Result<Self> {
        let content =
            fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self> {
        let map = parse_to_map(content);
        Ok(Self {
            total: get_kb(&map, "MemTotal")?,
            free: get_kb(&map, "MemFree")?,
            available: get_kb(&map, "MemAvailable")?,
            buffers: get_kb(&map, "Buffers")?,
            cached: get_kb(&map, "Cached")?,
            s_reclaimable: get_kb(&map, "SReclaimable").unwrap_or(0),
        })
    }

    /// Buffers + Cached + SReclaimable (matches `free` command's buff/cache).
    pub fn buf_cache(&self) -> u64 {
        self.buffers + self.cached + self.s_reclaimable
    }

    /// Memory used by applications (excluding buffers/cache/ARC).
    pub fn app_used(&self, arc_bytes: u64) -> u64 {
        let arc_kb = arc_bytes / 1024;
        self.total
            .saturating_sub(self.free)
            .saturating_sub(self.buf_cache())
            .saturating_sub(arc_kb)
    }
}

/// Parse /proc/meminfo lines like "MemTotal:  3931420 kB" into a map of name -> kB value.
fn parse_to_map(content: &str) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        map.insert(key.to_string(), value);
    }
    map
}

fn get_kb(map: &HashMap<String, u64>, key: &str) -> Result<u64> {
    map.get(key)
        .copied()
        .with_context(|| format!("missing field '{key}' in meminfo"))
}

/// `MemSource` impl that reads `/proc/meminfo` on each refresh.
pub struct LinuxMemSource {
    path: PathBuf,
    last: Option<MemInfo>,
}

impl LinuxMemSource {
    pub fn new(path: PathBuf) -> Self {
        let last = MemInfo::from_path(&path).ok();
        Self { path, last }
    }
}

impl MemSource for LinuxMemSource {
    fn refresh(&mut self) -> Result<()> {
        self.last = Some(MemInfo::from_path(&self.path)?);
        Ok(())
    }

    fn snapshot(&self, arc_bytes: u64) -> Option<MemSnapshot> {
        let m = self.last.as_ref()?;
        if m.total == 0 {
            return None;
        }
        let app_used_kb = m.app_used(arc_bytes);
        let buf_cache_kb = m.buf_cache();
        Some(MemSnapshot {
            total_bytes: m.total * 1024,
            segments: vec![
                RamSegment {
                    label: "App",
                    color: Color::Green,
                    bytes: app_used_kb * 1024,
                },
                RamSegment {
                    label: "ARC",
                    color: Color::Magenta,
                    bytes: arc_bytes,
                },
                RamSegment {
                    label: "Buf/Cache",
                    color: Color::Yellow,
                    bytes: buf_cache_kb * 1024,
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> MemInfo {
        let content = std::fs::read_to_string("fixtures/meminfo").unwrap();
        MemInfo::parse(&content).unwrap()
    }

    #[test]
    fn parse_fixture_total() {
        let m = fixture();
        assert_eq!(m.total, 32768000);
    }

    #[test]
    fn parse_fixture_free() {
        let m = fixture();
        assert_eq!(m.free, 4096000);
    }

    #[test]
    fn parse_fixture_available() {
        let m = fixture();
        assert_eq!(m.available, 18432000);
    }

    #[test]
    fn parse_fixture_buffers_cached() {
        let m = fixture();
        assert_eq!(m.buffers, 512000);
        assert_eq!(m.cached, 2048000);
    }

    #[test]
    fn parse_fixture_sreclaimable() {
        let m = fixture();
        assert_eq!(m.s_reclaimable, 1024000);
    }

    #[test]
    fn buf_cache_includes_sreclaimable() {
        let m = fixture();
        assert_eq!(m.buf_cache(), 512_000 + 2_048_000 + 1_024_000);
    }

    #[test]
    fn app_used_subtracts_arc() {
        let m = fixture();
        let arc_bytes: u64 = 12_345_678_912;
        let arc_kb = arc_bytes / 1024;
        let expected = 32_768_000 - 4_096_000 - 3_584_000 - arc_kb;
        assert_eq!(m.app_used(arc_bytes), expected);
    }

    #[test]
    fn linux_mem_source_snapshot_segments() {
        // Exercises LinuxMemSource::new and the MemSource::snapshot trait impl
        // end-to-end against the fixture, mirroring what main.rs does at runtime.
        let src = LinuxMemSource::new(PathBuf::from("fixtures/meminfo"));
        let arc_bytes: u64 = 8 * 1024 * 1024 * 1024; // 8 GiB
        let snap = src.snapshot(arc_bytes).unwrap();
        assert_eq!(snap.total_bytes, 32_768_000 * 1024);
        assert_eq!(snap.segments.len(), 3);
        assert_eq!(snap.segments[0].label, "App");
        assert_eq!(snap.segments[1].label, "ARC");
        assert_eq!(snap.segments[1].bytes, arc_bytes);
        assert_eq!(snap.segments[2].label, "Buf/Cache");
        // Buf/Cache = (buffers + cached + s_reclaimable) * 1024
        assert_eq!(snap.segments[2].bytes, (512_000 + 2_048_000 + 1_024_000) * 1024);
    }
}
