// FreeBSD source: read kstat.zfs.misc.arcstats.* via the sysctl(3) interface.
//
// At runtime (FreeBSD only) we use the `sysctl` crate. For tests on the dev
// host (Linux), `parse_sysctl_text` is cross-platform and consumes captured
// `sysctl(8)` output from `fixtures/bsd/arcstats.freebsd.txt`. Both paths funnel
// through `super::populate` so the field list lives in exactly one place.

use anyhow::Result;
#[cfg(target_os = "freebsd")]
use anyhow::Context;
#[cfg(test)]
use anyhow::anyhow;
#[cfg(test)]
use std::collections::HashMap;

use super::{populate, ArcStats};

const KSTAT_PREFIX: &str = "kstat.zfs.misc.arcstats.";

/// Read the current ARC stats by issuing one `sysctl` lookup per field.
///
/// Uses `Ctl::value_string()` and parses the result as a `u64` rather than
/// `value_as::<u64>()` so we're robust against width variation across OpenZFS
/// versions (some keys are S64, some Uint, some opaque).
#[cfg(target_os = "freebsd")]
pub fn from_sysctl() -> Result<ArcStats> {
    use sysctl::Sysctl;
    populate(|name| {
        let key = format!("{KSTAT_PREFIX}{name}");
        let ctl = sysctl::Ctl::new(&key)
            .with_context(|| format!("failed to open sysctl {key}"))?;
        let s = ctl
            .value_string()
            .with_context(|| format!("failed to read sysctl {key}"))?;
        s.trim()
            .parse::<u64>()
            .with_context(|| format!("sysctl {key} returned non-numeric value: {s:?}"))
    })
}

/// Parse text in the format produced by `sysctl kstat.zfs.misc.arcstats`:
///
///     kstat.zfs.misc.arcstats.hits: 8019094
///     kstat.zfs.misc.arcstats.iohits: 12844
///     ...
///
/// Used by tests; the runtime FreeBSD path uses `from_sysctl` instead.
#[cfg(test)]
pub fn parse_sysctl_text(content: &str) -> Result<ArcStats> {
    let map = parse_to_map(content);
    populate(|name| {
        map.get(name)
            .copied()
            .ok_or_else(|| anyhow!("missing field '{name}' in sysctl arcstats text"))
    })
}

#[cfg(test)]
fn parse_to_map(content: &str) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(short) = key.strip_prefix(KSTAT_PREFIX) else {
            continue;
        };
        let Ok(parsed) = value.trim().parse::<u64>() else {
            continue;
        };
        map.insert(short.to_string(), parsed);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ArcStats {
        let content = std::fs::read_to_string("fixtures/bsd/arcstats.freebsd.txt").unwrap();
        parse_sysctl_text(&content).unwrap()
    }

    #[test]
    fn parse_fixture_hits() {
        let stats = fixture();
        assert_eq!(stats.hits, 8019094);
        assert_eq!(stats.iohits, 12844);
        assert_eq!(stats.misses, 80548);
    }

    #[test]
    fn parse_fixture_demand() {
        let stats = fixture();
        assert_eq!(stats.demand_data_hits, 6040389);
        assert_eq!(stats.demand_data_iohits, 20);
        assert_eq!(stats.demand_data_misses, 61775);
        assert_eq!(stats.demand_metadata_hits, 1970835);
        assert_eq!(stats.demand_metadata_iohits, 476);
        assert_eq!(stats.demand_metadata_misses, 4475);
    }

    #[test]
    fn parse_fixture_prefetch() {
        let stats = fixture();
        assert_eq!(stats.prefetch_data_hits, 3407);
        assert_eq!(stats.prefetch_data_iohits, 0);
        assert_eq!(stats.prefetch_data_misses, 11565);
        assert_eq!(stats.prefetch_metadata_hits, 4462);
        assert_eq!(stats.prefetch_metadata_iohits, 12348);
        assert_eq!(stats.prefetch_metadata_misses, 2733);
    }

    #[test]
    fn parse_fixture_sizing() {
        let stats = fixture();
        assert_eq!(stats.size, 1472594864);
        assert_eq!(stats.c, 1520850909);
        assert_eq!(stats.c_min, 132823936);
        assert_eq!(stats.c_max, 3176624128);
    }

    #[test]
    fn parse_fixture_breakdown() {
        let stats = fixture();
        assert_eq!(stats.data_size, 1248682496);
        assert_eq!(stats.metadata_size, 92718592);
        assert_eq!(stats.mru_size, 65158656);
        assert_eq!(stats.mru_data, 26234880);
        assert_eq!(stats.mru_metadata, 38923776);
        assert_eq!(stats.mfu_size, 1275973632);
        assert_eq!(stats.mfu_data, 1222442496);
        assert_eq!(stats.mfu_metadata, 53531136);
        assert_eq!(stats.anon_size, 268288);
    }

    #[test]
    fn parse_fixture_compression() {
        let stats = fixture();
        assert_eq!(stats.compressed_size, 1242330624);
        assert_eq!(stats.uncompressed_size, 2752199168);
    }

    #[test]
    fn parse_fixture_memory() {
        let stats = fixture();
        assert_eq!(stats.memory_all_bytes, 4250365952);
        assert_eq!(stats.memory_free_bytes, 109547520);
        assert_eq!(stats.memory_available_bytes, 21221376);
        assert_eq!(stats.arc_meta_used, 214094256);
    }
}
