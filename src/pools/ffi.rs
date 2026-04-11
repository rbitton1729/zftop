//! Raw FFI bindings to libzfs. Hand-rolled; no bindgen, no clang build dep.
//!
//! Authoritative upstream sources cross-checked against
//! `/usr/include/libzfs/` on the dev host:
//! - `libzfs.h`            — function signatures
//! - `sys/fs/zfs.h`        — `zpool_prop_t`, `vdev_state_t`, struct layouts
//! - `sys/nvpair.h`        — nvlist lookup functions
//!
//! # ABI stability
//!
//! The subset of libzfs we call has been stable since OpenZFS 0.7 (~2017).
//! `zpool_prop_t` values are appended-only (new props go at the end), so
//! SIZE / CAPACITY / HEALTH / FREE / ALLOCATED / FRAGMENTATION retain the
//! same integer values across versions. `vdev_stat_t` and `pool_scan_stat_t`
//! are similarly appended-only for fields added after ~2017; the offsets
//! we read are all in the "stable prefix" of those structs.
//!
//! A libzfs soname bump (libzfs.so.4 → libzfs.so.5 → ...) is a rebuild
//! signal; it's caught at link time, not runtime. See the README for the
//! distro-package fallback (`cargo install zftop`).
//!
//! # Safety
//!
//! Every function in this file is `unsafe`. Callers MUST:
//! - Hold a valid, non-null `*mut libzfs_handle_t` returned from
//!   `libzfs_init` and not yet closed by `libzfs_fini`.
//! - Treat all `*const c_char` returns as borrowed strings whose lifetime
//!   is tied to the owning nvlist or zpool handle — copy them into owned
//!   Rust `String`s before that owner is released.
//! - Check `c_int` return codes: 0 means success, non-zero is errno-ish
//!   failure and the out-pointer has not been written to.

#![allow(dead_code)] // Task 7 adds callers; many symbols temporarily unused.

use std::ffi::{c_char, c_int, c_uint, CStr};

// ---------------------------------------------------------------------------
// Opaque types
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct libzfs_handle_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct zpool_handle_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct nvlist_t {
    _private: [u8; 0],
}

// ---------------------------------------------------------------------------
// zpool_prop_t — pool property ids
//
// Hand-copied from enum zpool_prop in sys/fs/zfs.h. Values are the enum's
// positional integer encoding. Appended-only across OpenZFS versions, so
// the values below are stable from ~OpenZFS 0.7 onward. Only the properties
// zftop actually reads are listed.
// ---------------------------------------------------------------------------

pub const ZPOOL_PROP_SIZE: c_int = 1;
pub const ZPOOL_PROP_CAPACITY: c_int = 2;
pub const ZPOOL_PROP_HEALTH: c_int = 4;
pub const ZPOOL_PROP_FREE: c_int = 16;
pub const ZPOOL_PROP_ALLOCATED: c_int = 17;
pub const ZPOOL_PROP_FRAGMENTATION: c_int = 23;

// ---------------------------------------------------------------------------
// vdev_state_t — vdev health
//
// From enum vdev_state in sys/fs/zfs.h. UNKNOWN..HEALTHY, in this order.
// ---------------------------------------------------------------------------

pub const VDEV_STATE_UNKNOWN: u64 = 0;
pub const VDEV_STATE_CLOSED: u64 = 1;
pub const VDEV_STATE_OFFLINE: u64 = 2;
pub const VDEV_STATE_REMOVED: u64 = 3;
pub const VDEV_STATE_CANT_OPEN: u64 = 4;
pub const VDEV_STATE_FAULTED: u64 = 5;
pub const VDEV_STATE_DEGRADED: u64 = 6;
pub const VDEV_STATE_HEALTHY: u64 = 7;

// ---------------------------------------------------------------------------
// pool_scan_func_t / dsl_scan_state_t — scrub / resilver func + state
//
// From sys/fs/zfs.h.
// ---------------------------------------------------------------------------

pub const POOL_SCAN_NONE: u64 = 0;
pub const POOL_SCAN_SCRUB: u64 = 1;
pub const POOL_SCAN_RESILVER: u64 = 2;

pub const DSS_NONE: u64 = 0;
pub const DSS_SCANNING: u64 = 1;
pub const DSS_FINISHED: u64 = 2;
pub const DSS_CANCELED: u64 = 3;

// ---------------------------------------------------------------------------
// ZPOOL_CONFIG_* nvlist key strings
//
// From sys/fs/zfs.h (#define ZPOOL_CONFIG_*). Used with `nvlist_lookup_*`.
// ---------------------------------------------------------------------------

pub const ZPOOL_CONFIG_VDEV_TREE: &CStr = c"vdev_tree";
pub const ZPOOL_CONFIG_TYPE: &CStr = c"type";
pub const ZPOOL_CONFIG_CHILDREN: &CStr = c"children";
pub const ZPOOL_CONFIG_PATH: &CStr = c"path";
pub const ZPOOL_CONFIG_SCAN_STATS: &CStr = c"scan_stats";
pub const ZPOOL_CONFIG_VDEV_STATS: &CStr = c"vdev_stats";

// ---------------------------------------------------------------------------
// vdev "type" values — the string value behind ZPOOL_CONFIG_TYPE. Used to
// identify root / raidz / mirror / disk / file / log group / cache group /
// spare group nodes during the recursive vdev walk.
//
// From sys/fs/zfs.h VDEV_TYPE_* defines.
// ---------------------------------------------------------------------------

pub const VDEV_TYPE_ROOT: &str = "root";
pub const VDEV_TYPE_MIRROR: &str = "mirror";
pub const VDEV_TYPE_RAIDZ: &str = "raidz";
pub const VDEV_TYPE_DRAID: &str = "draid";
pub const VDEV_TYPE_DISK: &str = "disk";
pub const VDEV_TYPE_FILE: &str = "file";
pub const VDEV_TYPE_LOG: &str = "log";
pub const VDEV_TYPE_SPARE: &str = "spare";
pub const VDEV_TYPE_L2CACHE: &str = "l2cache";
pub const VDEV_TYPE_REPLACING: &str = "replacing";

// ---------------------------------------------------------------------------
// pool_scan_stat_t uint64_array indices
//
// The `scan_stats` nvlist key returns a `uint64_array`. Each index maps to
// a field of `struct pool_scan_stat` in sys/fs/zfs.h. The 0..=8 indices are
// the "stored on disk" prefix and have been stable since OpenZFS 0.7.
// Newer versions append runtime-only fields after index 8 — we don't read
// those, and we check `nelem >= 9` before indexing to guard against an
// older libzfs returning a shorter array.
// ---------------------------------------------------------------------------

pub const PSS_IDX_FUNC: usize = 0;
pub const PSS_IDX_STATE: usize = 1;
pub const PSS_IDX_START_TIME: usize = 2;
pub const PSS_IDX_END_TIME: usize = 3;
pub const PSS_IDX_TO_EXAMINE: usize = 4;
pub const PSS_IDX_EXAMINED: usize = 5;
pub const PSS_IDX_SKIPPED: usize = 6;
pub const PSS_IDX_PROCESSED: usize = 7;
pub const PSS_IDX_ERRORS: usize = 8;
pub const PSS_MIN_LEN: usize = 9; // Check `nelem >= PSS_MIN_LEN` before indexing.

// ---------------------------------------------------------------------------
// vdev_stat_t uint64_array indices
//
// The `vdev_stats` nvlist key returns a `uint64_array`. Each index maps to
// a field of `struct vdev_stat` in sys/fs/zfs.h. `VS_ZIO_TYPES` has been 6
// since OpenZFS 2.0 (flush was the last added type).
//
// Layout (assuming VS_ZIO_TYPES = 6):
//   0  vs_timestamp (hrtime_t = int64_t, 1 u64)
//   1  vs_state
//   2  vs_aux
//   3  vs_alloc
//   4  vs_space
//   5  vs_dspace
//   6  vs_rsize
//   7  vs_esize
//   8..13   vs_ops[0..6]
//   14..19  vs_bytes[0..6]
//   20  vs_read_errors
//   21  vs_write_errors
//   22  vs_checksum_errors
//
// We guard with `nelem >= VS_MIN_LEN` before indexing — on an older libzfs
// with VS_ZIO_TYPES=5, the error indices shift down by 2 and we'd read
// wrong fields. Safer to refuse to decode than to silently report garbage.
// ---------------------------------------------------------------------------

pub const VS_IDX_TIMESTAMP: usize = 0;
pub const VS_IDX_STATE: usize = 1;
pub const VS_IDX_AUX: usize = 2;
pub const VS_IDX_ALLOC: usize = 3;
pub const VS_IDX_SPACE: usize = 4;
pub const VS_IDX_DSPACE: usize = 5;
pub const VS_IDX_RSIZE: usize = 6;
pub const VS_IDX_ESIZE: usize = 7;
// vs_ops[6] at 8..13, vs_bytes[6] at 14..19
pub const VS_IDX_READ_ERRORS: usize = 20;
pub const VS_IDX_WRITE_ERRORS: usize = 21;
pub const VS_IDX_CHECKSUM_ERRORS: usize = 22;
pub const VS_MIN_LEN: usize = 23; // through vs_checksum_errors inclusive

// ---------------------------------------------------------------------------
// Function declarations
// ---------------------------------------------------------------------------

// The C typedef is `zpool_iter_f` (snake_case) — keep the same name so the
// signature lines up with the header at a glance.
#[allow(non_camel_case_types)]
pub type zpool_iter_f = unsafe extern "C" fn(
    zhp: *mut zpool_handle_t,
    data: *mut std::ffi::c_void,
) -> c_int;

unsafe extern "C" {
    // Library lifecycle
    pub fn libzfs_init() -> *mut libzfs_handle_t;
    pub fn libzfs_fini(handle: *mut libzfs_handle_t);
    pub fn libzfs_error_description(handle: *mut libzfs_handle_t) -> *const c_char;

    // Pool iteration
    pub fn zpool_iter(
        handle: *mut libzfs_handle_t,
        func: zpool_iter_f,
        data: *mut std::ffi::c_void,
    ) -> c_int;

    // Pool handle getters
    pub fn zpool_get_name(zhp: *mut zpool_handle_t) -> *const c_char;
    pub fn zpool_get_state(zhp: *mut zpool_handle_t) -> c_int;
    pub fn zpool_get_config(
        zhp: *mut zpool_handle_t,
        oldconfig: *mut *mut nvlist_t,
    ) -> *mut nvlist_t;
    pub fn zpool_close(zhp: *mut zpool_handle_t);

    // Pool properties (int flavor — for SIZE, CAPACITY, ALLOCATED, etc.)
    // The third arg is `zprop_source_t *` which we don't use; pass null.
    pub fn zpool_get_prop_int(
        zhp: *mut zpool_handle_t,
        prop: c_int,
        src: *mut c_int,
    ) -> u64;

    // nvlist walking
    pub fn nvlist_lookup_string(
        nvl: *const nvlist_t,
        name: *const c_char,
        value: *mut *const c_char,
    ) -> c_int;
    pub fn nvlist_lookup_uint64(
        nvl: *const nvlist_t,
        name: *const c_char,
        value: *mut u64,
    ) -> c_int;
    pub fn nvlist_lookup_nvlist(
        nvl: *mut nvlist_t,
        name: *const c_char,
        value: *mut *mut nvlist_t,
    ) -> c_int;
    pub fn nvlist_lookup_nvlist_array(
        nvl: *mut nvlist_t,
        name: *const c_char,
        value: *mut *mut *mut nvlist_t,
        nelem: *mut c_uint,
    ) -> c_int;
    pub fn nvlist_lookup_uint64_array(
        nvl: *mut nvlist_t,
        name: *const c_char,
        value: *mut *mut u64,
        nelem: *mut c_uint,
    ) -> c_int;
}
