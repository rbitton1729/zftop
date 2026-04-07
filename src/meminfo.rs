// Parse /proc/meminfo for system memory overview.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct MemInfo {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
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
        })
    }

    /// Memory used by applications (excluding buffers/cache/ARC).
    pub fn app_used(&self, arc_bytes: u64) -> u64 {
        let arc_kb = arc_bytes / 1024;
        self.total
            .saturating_sub(self.free)
            .saturating_sub(self.buffers)
            .saturating_sub(self.cached)
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
    fn app_used_subtracts_arc() {
        let m = fixture();
        // total - free - buffers - cached - arc_kb
        // 32768000 - 4096000 - 512000 - 2048000 - (12345678912/1024)
        let arc_bytes: u64 = 12_345_678_912;
        let arc_kb = arc_bytes / 1024;
        let expected = 32_768_000 - 4_096_000 - 512_000 - 2_048_000 - arc_kb;
        assert_eq!(m.app_used(arc_bytes), expected);
    }
}
