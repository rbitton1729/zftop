// ArcStats struct + cross-platform field-population logic.
//
// The data shape is identical on Linux and FreeBSD because OpenZFS exposes
// the same kstat keys on both. Only the *source* differs:
//   - Linux: text from /proc/spl/kstat/zfs/arcstats
//   - FreeBSD: typed sysctl values under kstat.zfs.misc.arcstats.*
//
// `populate` lists every field exactly once and takes a closure that returns
// a u64 for a given key name. Each OS submodule provides its own closure.

use anyhow::{Context, Result};

#[cfg(any(test, target_os = "linux"))]
pub mod linux;
#[cfg(any(test, target_os = "freebsd"))]
pub mod freebsd;

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

/// Populate an `ArcStats` by querying each field through the supplied closure.
///
/// The closure receives a kstat key name (e.g. `"hits"`, `"demand_data_hits"`)
/// and returns its current value as a `u64`. This is the single source of truth
/// for the field list — all OS-specific code paths funnel through here.
pub(crate) fn populate<F>(get: F) -> Result<ArcStats>
where
    F: Fn(&str) -> Result<u64>,
{
    Ok(ArcStats {
        hits: get("hits").context("missing 'hits'")?,
        iohits: get("iohits").context("missing 'iohits'")?,
        misses: get("misses").context("missing 'misses'")?,
        demand_data_hits: get("demand_data_hits").context("missing 'demand_data_hits'")?,
        demand_data_iohits: get("demand_data_iohits").context("missing 'demand_data_iohits'")?,
        demand_data_misses: get("demand_data_misses").context("missing 'demand_data_misses'")?,
        demand_metadata_hits: get("demand_metadata_hits").context("missing 'demand_metadata_hits'")?,
        demand_metadata_iohits: get("demand_metadata_iohits").context("missing 'demand_metadata_iohits'")?,
        demand_metadata_misses: get("demand_metadata_misses").context("missing 'demand_metadata_misses'")?,
        prefetch_data_hits: get("prefetch_data_hits").context("missing 'prefetch_data_hits'")?,
        prefetch_data_iohits: get("prefetch_data_iohits").context("missing 'prefetch_data_iohits'")?,
        prefetch_data_misses: get("prefetch_data_misses").context("missing 'prefetch_data_misses'")?,
        prefetch_metadata_hits: get("prefetch_metadata_hits").context("missing 'prefetch_metadata_hits'")?,
        prefetch_metadata_iohits: get("prefetch_metadata_iohits").context("missing 'prefetch_metadata_iohits'")?,
        prefetch_metadata_misses: get("prefetch_metadata_misses").context("missing 'prefetch_metadata_misses'")?,
        size: get("size").context("missing 'size'")?,
        c: get("c").context("missing 'c'")?,
        c_min: get("c_min").context("missing 'c_min'")?,
        c_max: get("c_max").context("missing 'c_max'")?,
        data_size: get("data_size").context("missing 'data_size'")?,
        metadata_size: get("metadata_size").context("missing 'metadata_size'")?,
        anon_size: get("anon_size").context("missing 'anon_size'")?,
        overhead_size: get("overhead_size").context("missing 'overhead_size'")?,
        hdr_size: get("hdr_size").context("missing 'hdr_size'")?,
        dbuf_size: get("dbuf_size").context("missing 'dbuf_size'")?,
        dnode_size: get("dnode_size").context("missing 'dnode_size'")?,
        bonus_size: get("bonus_size").context("missing 'bonus_size'")?,
        mru_size: get("mru_size").context("missing 'mru_size'")?,
        mru_data: get("mru_data").context("missing 'mru_data'")?,
        mru_metadata: get("mru_metadata").context("missing 'mru_metadata'")?,
        mfu_size: get("mfu_size").context("missing 'mfu_size'")?,
        mfu_data: get("mfu_data").context("missing 'mfu_data'")?,
        mfu_metadata: get("mfu_metadata").context("missing 'mfu_metadata'")?,
        compressed_size: get("compressed_size").context("missing 'compressed_size'")?,
        uncompressed_size: get("uncompressed_size").context("missing 'uncompressed_size'")?,
        memory_all_bytes: get("memory_all_bytes").context("missing 'memory_all_bytes'")?,
        memory_free_bytes: get("memory_free_bytes").context("missing 'memory_free_bytes'")?,
        memory_available_bytes: get("memory_available_bytes").context("missing 'memory_available_bytes'")?,
        arc_meta_used: get("arc_meta_used").context("missing 'arc_meta_used'")?,
    })
}
