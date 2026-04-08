// Linux source: parse /proc/spl/kstat/zfs/arcstats text format.
//
// Format is three columns per line — `name type value` — with two header
// lines we skip. We build a `name -> u64` map and feed it to the shared
// populator via a closure.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{populate, ArcStats};

pub fn from_procfs_path(path: &Path) -> Result<ArcStats> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse(&content)
}

pub fn parse(content: &str) -> Result<ArcStats> {
    let map = parse_to_map(content)?;
    populate(|name| {
        map.get(name)
            .copied()
            .ok_or_else(|| anyhow!("missing field '{name}' in arcstats"))
    })
}

fn parse_to_map(content: &str) -> Result<HashMap<String, u64>> {
    let mut map = HashMap::new();
    for line in content.lines().skip(2) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue;
        }
        let name = parts[0];
        let value: u64 = parts[2]
            .parse()
            .with_context(|| format!("failed to parse value for '{name}'"))?;
        map.insert(name.to_string(), value);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ArcStats {
        let content = std::fs::read_to_string("fixtures/arcstats").unwrap();
        parse(&content).unwrap()
    }

    #[test]
    fn parse_fixture_hits() {
        let stats = fixture();
        assert_eq!(stats.hits, 11891096);
        assert_eq!(stats.iohits, 23938);
        assert_eq!(stats.misses, 101339);
    }

    #[test]
    fn parse_fixture_demand() {
        let stats = fixture();
        assert_eq!(stats.demand_data_hits, 4894419);
        assert_eq!(stats.demand_data_iohits, 582);
        assert_eq!(stats.demand_data_misses, 30760);
        assert_eq!(stats.demand_metadata_hits, 6900081);
        assert_eq!(stats.demand_metadata_iohits, 1006);
        assert_eq!(stats.demand_metadata_misses, 37485);
    }

    #[test]
    fn parse_fixture_prefetch() {
        let stats = fixture();
        assert_eq!(stats.prefetch_data_hits, 39679);
        assert_eq!(stats.prefetch_data_iohits, 141);
        assert_eq!(stats.prefetch_data_misses, 19977);
        assert_eq!(stats.prefetch_metadata_hits, 56917);
        assert_eq!(stats.prefetch_metadata_iohits, 22209);
        assert_eq!(stats.prefetch_metadata_misses, 13117);
    }

    #[test]
    fn parse_fixture_sizing() {
        let stats = fixture();
        assert_eq!(stats.size, 8540576328);
        assert_eq!(stats.c, 8589934592);
        assert_eq!(stats.c_min, 912230400);
        assert_eq!(stats.c_max, 8589934592);
    }

    #[test]
    fn parse_fixture_breakdown() {
        let stats = fixture();
        assert_eq!(stats.data_size, 6777644544);
        assert_eq!(stats.metadata_size, 1123503104);
        assert_eq!(stats.mru_size, 2139001856);
        assert_eq!(stats.mru_data, 1425743872);
        assert_eq!(stats.mru_metadata, 713257984);
        assert_eq!(stats.mfu_size, 5754272768);
        assert_eq!(stats.mfu_data, 5350335488);
        assert_eq!(stats.mfu_metadata, 403937280);
        assert_eq!(stats.anon_size, 6574080);
    }

    #[test]
    fn parse_fixture_compression() {
        let stats = fixture();
        assert_eq!(stats.compressed_size, 7217783296);
        assert_eq!(stats.uncompressed_size, 12716617216);
    }

    #[test]
    fn parse_fixture_memory() {
        let stats = fixture();
        assert_eq!(stats.memory_all_bytes, 29191372800);
        assert_eq!(stats.memory_free_bytes, 8198266880);
        assert_eq!(stats.memory_available_bytes, 7122022400);
        assert_eq!(stats.arc_meta_used, 1730780232);
    }

    #[test]
    fn from_procfs_path_round_trips() {
        // Exercises the file-reading entry point used by main.rs on Linux.
        let stats = from_procfs_path(Path::new("fixtures/arcstats")).unwrap();
        assert_eq!(stats.hits, 11891096);
    }
}
