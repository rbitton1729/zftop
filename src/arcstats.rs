// Parse /proc/spl/kstat/zfs/arcstats (or fixture) into typed data.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ArcStats {
    // Overall hit/miss/iohit
    pub hits: u64,
    pub iohits: u64,
    pub misses: u64,

    // Demand
    pub demand_data_hits: u64,
    pub demand_data_iohits: u64,
    pub demand_data_misses: u64,
    pub demand_metadata_hits: u64,
    pub demand_metadata_iohits: u64,
    pub demand_metadata_misses: u64,

    // Prefetch
    pub prefetch_data_hits: u64,
    pub prefetch_data_iohits: u64,
    pub prefetch_data_misses: u64,
    pub prefetch_metadata_hits: u64,
    pub prefetch_metadata_iohits: u64,
    pub prefetch_metadata_misses: u64,

    // ARC sizing
    pub size: u64,
    pub c: u64,
    pub c_min: u64,
    pub c_max: u64,

    // Breakdown — top level
    pub data_size: u64,
    pub metadata_size: u64,
    pub anon_size: u64,
    pub overhead_size: u64,
    pub hdr_size: u64,
    pub dbuf_size: u64,
    pub dnode_size: u64,
    pub bonus_size: u64,

    // Breakdown — per list with data/metadata split
    pub mru_size: u64,
    pub mru_data: u64,
    pub mru_metadata: u64,
    pub mfu_size: u64,
    pub mfu_data: u64,
    pub mfu_metadata: u64,

    // Compression
    pub compressed_size: u64,
    pub uncompressed_size: u64,

    // ZFS memory tracking
    pub memory_all_bytes: u64,
    pub memory_free_bytes: u64,
    pub memory_available_bytes: u64,
    pub arc_meta_used: u64,
}

impl ArcStats {
    pub fn from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self> {
        let map = parse_to_map(content)?;
        Ok(Self {
            hits: get(&map, "hits")?,
            iohits: get(&map, "iohits")?,
            misses: get(&map, "misses")?,
            demand_data_hits: get(&map, "demand_data_hits")?,
            demand_data_iohits: get(&map, "demand_data_iohits")?,
            demand_data_misses: get(&map, "demand_data_misses")?,
            demand_metadata_hits: get(&map, "demand_metadata_hits")?,
            demand_metadata_iohits: get(&map, "demand_metadata_iohits")?,
            demand_metadata_misses: get(&map, "demand_metadata_misses")?,
            prefetch_data_hits: get(&map, "prefetch_data_hits")?,
            prefetch_data_iohits: get(&map, "prefetch_data_iohits")?,
            prefetch_data_misses: get(&map, "prefetch_data_misses")?,
            prefetch_metadata_hits: get(&map, "prefetch_metadata_hits")?,
            prefetch_metadata_iohits: get(&map, "prefetch_metadata_iohits")?,
            prefetch_metadata_misses: get(&map, "prefetch_metadata_misses")?,
            size: get(&map, "size")?,
            c: get(&map, "c")?,
            c_min: get(&map, "c_min")?,
            c_max: get(&map, "c_max")?,
            data_size: get(&map, "data_size")?,
            metadata_size: get(&map, "metadata_size")?,
            anon_size: get(&map, "anon_size")?,
            overhead_size: get(&map, "overhead_size")?,
            hdr_size: get(&map, "hdr_size")?,
            dbuf_size: get(&map, "dbuf_size")?,
            dnode_size: get(&map, "dnode_size")?,
            bonus_size: get(&map, "bonus_size")?,
            mru_size: get(&map, "mru_size")?,
            mru_data: get(&map, "mru_data")?,
            mru_metadata: get(&map, "mru_metadata")?,
            mfu_size: get(&map, "mfu_size")?,
            mfu_data: get(&map, "mfu_data")?,
            mfu_metadata: get(&map, "mfu_metadata")?,
            compressed_size: get(&map, "compressed_size")?,
            uncompressed_size: get(&map, "uncompressed_size")?,
            memory_all_bytes: get(&map, "memory_all_bytes")?,
            memory_free_bytes: get(&map, "memory_free_bytes")?,
            memory_available_bytes: get(&map, "memory_available_bytes")?,
            arc_meta_used: get(&map, "arc_meta_used")?,
        })
    }
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

fn get(map: &HashMap<String, u64>, key: &str) -> Result<u64> {
    map.get(key)
        .copied()
        .with_context(|| format!("missing field '{key}' in arcstats"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ArcStats {
        let content = std::fs::read_to_string("fixtures/arcstats").unwrap();
        ArcStats::parse(&content).unwrap()
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
}
