//! Raw FFI bindings to libzfs — runtime-loaded via `dlopen`.
//!
//! The previous iteration link-bound against `libzfs.so` via
//! `cargo:rustc-link-lib=zfs` in build.rs, which froze the binary's
//! `DT_NEEDED` entry to whatever soname the build host happened to ship.
//! Debian 11 currently ships `libzfs.so.4`; Arch ships `libzfs.so.7`;
//! Fedora and Ubuntu 24.04 ship `libzfs.so.6`. A binary built against one
//! won't launch on another.
//!
//! Fix: dlopen libzfs (and libnvpair) at runtime, trying a list of known
//! sonames in order. The binary no longer has any libzfs reference in
//! `DT_NEEDED` — just `libc.so.6` and `libdl.so.2` — and the same binary
//! works across OpenZFS 0.8 through 2.3+.
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
//! # Safety
//!
//! Every function pointer on `Libzfs` is `unsafe` to invoke. Callers MUST:
//! - Hold a valid, non-null `*mut libzfs_handle_t` returned from
//!   `libzfs_init` and not yet closed by `libzfs_fini`.
//! - Treat all `*const c_char` returns as borrowed strings whose lifetime
//!   is tied to the owning nvlist or zpool handle — copy them into owned
//!   Rust `String`s before that owner is released.
//! - Check `c_int` return codes: 0 means success, non-zero is errno-ish
//!   failure and the out-pointer has not been written to.

use anyhow::{anyhow, Context, Result};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::ptr;

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
pub const ZPOOL_CONFIG_NPARITY: &CStr = c"nparity";

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
// Indices 9..=14 are the runtime-only "pass" fields that OpenZFS 0.8
// (2019) added when it introduced sequential scrub — `pss_issued` at
// index 14 is what `zpool status` actually divides by `pss_to_examine`
// to compute the displayed "% done" number. Reading `pss_examined` for
// progress instead (as we did in v0.2) is wrong for all modern pools
// because the metadata walk finishes in seconds while `pss_issued`
// climbs over the real duration of the scrub.
//
// We support two array lengths:
// - `PSS_MIN_LEN = 9`: legacy pre-0.8 layout (no sequential scrub). Fall
//   back to `pss_examined / pss_to_examine`, which was accurate for that
//   era because every byte was scanned in one pass.
// - `PSS_MIN_LEN_WITH_ISSUED = 15`: OpenZFS 0.8+ layout with pass fields.
//   Use `pss_issued / (pss_to_examine - pss_skipped)` to match zpool
//   status byte-for-byte.
// ---------------------------------------------------------------------------

pub const PSS_IDX_FUNC: usize = 0;
pub const PSS_IDX_STATE: usize = 1;
pub const PSS_IDX_START_TIME: usize = 2;
pub const PSS_IDX_END_TIME: usize = 3;
pub const PSS_IDX_TO_EXAMINE: usize = 4;
pub const PSS_IDX_EXAMINED: usize = 5;
pub const PSS_IDX_SKIPPED: usize = 6;
#[allow(dead_code)]
pub const PSS_IDX_PROCESSED: usize = 7;
pub const PSS_IDX_ERRORS: usize = 8;
pub const PSS_MIN_LEN: usize = 9; // Check `nelem >= PSS_MIN_LEN` before indexing.

// Runtime-only "pass" fields (OpenZFS 0.8+). See long comment above.
#[allow(dead_code)]
pub const PSS_IDX_PASS_EXAM: usize = 9;
pub const PSS_IDX_PASS_START: usize = 10;
#[allow(dead_code)]
pub const PSS_IDX_PASS_SCRUB_PAUSE: usize = 11;
pub const PSS_IDX_PASS_SCRUB_SPENT_PAUSED: usize = 12;
pub const PSS_IDX_PASS_ISSUED: usize = 13;
pub const PSS_IDX_ISSUED: usize = 14;
pub const PSS_MIN_LEN_WITH_ISSUED: usize = 15;

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

#[allow(dead_code)]
pub const VS_IDX_TIMESTAMP: usize = 0;
pub const VS_IDX_STATE: usize = 1;
#[allow(dead_code)]
pub const VS_IDX_AUX: usize = 2;
#[allow(dead_code)]
pub const VS_IDX_ALLOC: usize = 3;
pub const VS_IDX_SPACE: usize = 4;
#[allow(dead_code)]
pub const VS_IDX_DSPACE: usize = 5;
#[allow(dead_code)]
pub const VS_IDX_RSIZE: usize = 6;
#[allow(dead_code)]
pub const VS_IDX_ESIZE: usize = 7;
// vs_ops[6] at 8..13, vs_bytes[6] at 14..19
pub const VS_IDX_READ_ERRORS: usize = 20;
pub const VS_IDX_WRITE_ERRORS: usize = 21;
pub const VS_IDX_CHECKSUM_ERRORS: usize = 22;
pub const VS_MIN_LEN: usize = 23; // through vs_checksum_errors inclusive

// ---------------------------------------------------------------------------
// dlopen/dlsym/dlclose — the runtime loader API itself.
//
// Declared inline so we don't drag in the `libc` crate. dlopen has been in
// POSIX forever. On glibc 2.34+ it's in libc.so.6 directly; on older glibc
// it's in libdl.so.2. `cargo:rustc-link-lib=dl` in build.rs makes both
// cases work (libdl.so.2 is still shipped as a compat stub on 2.34+).
// ---------------------------------------------------------------------------

// From /usr/include/dlfcn.h on glibc Linux. Values are stable.
const RTLD_NOW: c_int = 0x00002;
const RTLD_GLOBAL: c_int = 0x00100;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

/// Consume libdl's error state into an owned `String`. Call after any
/// `dlopen`/`dlsym` that returned null.
fn last_dl_error() -> String {
    // SAFETY: `dlerror` is thread-local and may return null if there's
    // no pending error. We null-check and copy the returned C string
    // into an owned Rust `String` before returning.
    unsafe {
        let ptr = dlerror();
        if ptr.is_null() {
            "(no dlerror)".into()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

/// Try each soname in turn; return the first handle that opens successfully.
/// All lookups use `RTLD_NOW | RTLD_GLOBAL`:
/// - `RTLD_NOW` forces symbol resolution at load time — we fail immediately
///   if the library is missing an expected function rather than at first
///   call.
/// - `RTLD_GLOBAL` makes the library's symbols visible to subsequent
///   `dlopen` / default-handle `dlsym`. This matters because libzfs's
///   transitive load of libnvpair benefits from already-loaded nvpair
///   symbols being visible process-wide.
fn try_dlopen_sonames(sonames: &[&CStr]) -> Result<*mut c_void> {
    // Clear any pre-existing dlerror state before we start.
    unsafe { let _ = dlerror(); }

    let mut last_err = String::new();
    for soname in sonames {
        // SAFETY: `soname` is a nul-terminated C string (CStr contract).
        // `dlopen` returns null on failure, valid handle on success.
        let handle = unsafe { dlopen(soname.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        if !handle.is_null() {
            return Ok(handle);
        }
        last_err = format!("{} ({})", soname.to_string_lossy(), last_dl_error());
    }
    Err(anyhow!(
        "could not open any of: {} — last error: {last_err}",
        sonames
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// dlsym the named symbol or return a descriptive error. The returned
/// raw pointer can be `std::mem::transmute`'d to the right function-
/// pointer type by the caller.
///
/// # Safety
///
/// `handle` must be a valid dlopen handle returned by a successful
/// `try_dlopen_sonames` call and not yet closed.
unsafe fn dlsym_required(handle: *mut c_void, name: &CStr) -> Result<*mut c_void> {
    // Clear any pre-existing dlerror state.
    unsafe { let _ = dlerror(); }
    // SAFETY: `handle` is valid per function contract; `name` is nul-terminated.
    let sym = unsafe { dlsym(handle, name.as_ptr()) };
    if sym.is_null() {
        Err(anyhow!(
            "dlsym({}) failed: {}",
            name.to_string_lossy(),
            last_dl_error()
        ))
    } else {
        Ok(sym)
    }
}

// ---------------------------------------------------------------------------
// Function-pointer typedefs for every libzfs/libnvpair symbol we call.
// These mirror the C signatures from libzfs.h and sys/nvpair.h.
// ---------------------------------------------------------------------------

pub type LibzfsInitFn = unsafe extern "C" fn() -> *mut libzfs_handle_t;
pub type LibzfsFiniFn = unsafe extern "C" fn(*mut libzfs_handle_t);
pub type LibzfsErrorDescriptionFn =
    unsafe extern "C" fn(*mut libzfs_handle_t) -> *const c_char;

// The C typedef is `zpool_iter_f` (snake_case) — keep the same name so the
// signature lines up with the header at a glance.
#[allow(non_camel_case_types)]
pub type zpool_iter_f = unsafe extern "C" fn(
    zhp: *mut zpool_handle_t,
    data: *mut c_void,
) -> c_int;

pub type ZpoolIterFn = unsafe extern "C" fn(
    handle: *mut libzfs_handle_t,
    func: zpool_iter_f,
    data: *mut c_void,
) -> c_int;
pub type ZpoolGetNameFn = unsafe extern "C" fn(*mut zpool_handle_t) -> *const c_char;
pub type ZpoolGetStateFn = unsafe extern "C" fn(*mut zpool_handle_t) -> c_int;
pub type ZpoolGetConfigFn =
    unsafe extern "C" fn(*mut zpool_handle_t, *mut *mut nvlist_t) -> *mut nvlist_t;
pub type ZpoolGetPropIntFn =
    unsafe extern "C" fn(*mut zpool_handle_t, c_int, *mut c_int) -> u64;
pub type ZpoolCloseFn = unsafe extern "C" fn(*mut zpool_handle_t);

pub type NvlistLookupStringFn = unsafe extern "C" fn(
    nvl: *const nvlist_t,
    name: *const c_char,
    value: *mut *const c_char,
) -> c_int;
pub type NvlistLookupUint64Fn =
    unsafe extern "C" fn(*const nvlist_t, *const c_char, *mut u64) -> c_int;
pub type NvlistLookupNvlistFn =
    unsafe extern "C" fn(*mut nvlist_t, *const c_char, *mut *mut nvlist_t) -> c_int;
pub type NvlistLookupNvlistArrayFn = unsafe extern "C" fn(
    *mut nvlist_t,
    *const c_char,
    *mut *mut *mut nvlist_t,
    *mut c_uint,
) -> c_int;
pub type NvlistLookupUint64ArrayFn = unsafe extern "C" fn(
    *mut nvlist_t,
    *const c_char,
    *mut *mut u64,
    *mut c_uint,
) -> c_int;

// ---------------------------------------------------------------------------
// Libzfs — owns dlopen handles + dlsym'd function pointers. Created once at
// `LibzfsPoolsSource::new()`, held for the lifetime of the source, dropped
// when the source drops (which calls dlclose on both handles).
//
// NOT thread-safe to mutate, but the function pointers are read-only after
// load and the C functions we call are reentrant-safe enough for our
// single-threaded event loop. Manually Send to live in Box<dyn PoolsSource>.
// ---------------------------------------------------------------------------

pub struct Libzfs {
    // Raw dlopen handles — held only for their Drop contract (dlclose).
    // Never dlsym'd directly after load() returns; the function pointers
    // below are what's actually called.
    nvpair_handle: *mut c_void,
    zfs_handle: *mut c_void,

    // libzfs symbols
    pub libzfs_init: LibzfsInitFn,
    pub libzfs_fini: LibzfsFiniFn,
    pub libzfs_error_description: LibzfsErrorDescriptionFn,
    pub zpool_iter: ZpoolIterFn,
    pub zpool_get_name: ZpoolGetNameFn,
    #[allow(dead_code)]
    pub zpool_get_state: ZpoolGetStateFn,
    pub zpool_get_config: ZpoolGetConfigFn,
    pub zpool_get_prop_int: ZpoolGetPropIntFn,
    pub zpool_close: ZpoolCloseFn,

    // libnvpair symbols
    pub nvlist_lookup_string: NvlistLookupStringFn,
    #[allow(dead_code)]
    pub nvlist_lookup_uint64: NvlistLookupUint64Fn,
    pub nvlist_lookup_nvlist: NvlistLookupNvlistFn,
    pub nvlist_lookup_nvlist_array: NvlistLookupNvlistArrayFn,
    pub nvlist_lookup_uint64_array: NvlistLookupUint64ArrayFn,
}

// SAFETY: All fields are either raw pointers (which are Send by Rust's
// rules but marked !Send when inside a struct, so we need this manual impl)
// or function pointers (which are Send+Sync natively). After `load()`
// returns, the struct is effectively read-only — we never mutate it, and
// the C library it wraps maintains its own thread safety at the libzfs
// handle level (which is separate from the function pointers).
unsafe impl Send for Libzfs {}

impl Libzfs {
    /// Load libnvpair and libzfs at runtime via dlopen, trying a list of
    /// known sonames for each. Returns a fully-populated `Libzfs` with all
    /// required function pointers, or a descriptive error if any required
    /// symbol is missing.
    pub fn load() -> Result<Self> {
        // libnvpair first. Its soname has been `.so.3` since OpenZFS 2.0,
        // but older OpenZFS 0.8 used `.so.2` and 0.7 used `.so.1` — support
        // both on the off chance a very old system is in the audience.
        let nvpair_handle = try_dlopen_sonames(&[
            c"libnvpair.so.3",
            c"libnvpair.so.2",
            c"libnvpair.so.1",
            c"libnvpair.so",
        ])
        .context("failed to load libnvpair — is OpenZFS installed?")?;

        // libzfs second. Soname fallback list covers the full OpenZFS
        // lifetime from 0.7 through 2.3+:
        //   libzfs.so.7 — OpenZFS 2.3.x (Arch current)
        //   libzfs.so.6 — OpenZFS 2.2.x (Ubuntu 24.04, Fedora 39+)
        //   libzfs.so.5 — OpenZFS 2.1.x briefly, some distros
        //   libzfs.so.4 — OpenZFS 2.1.x (Debian 11 w/ updates, Debian 12, Ubuntu 22.04)
        //   libzfs.so.2 — OpenZFS 2.0.x (Debian 11 original, Ubuntu 20.04)
        //   libzfs.so   — unversioned, only present via -dev packages
        let zfs_handle = try_dlopen_sonames(&[
            c"libzfs.so.7",
            c"libzfs.so.6",
            c"libzfs.so.5",
            c"libzfs.so.4",
            c"libzfs.so.2",
            c"libzfs.so",
        ])
        .context("failed to load libzfs — is OpenZFS installed?")?;

        // dlsym each symbol. Failures here mean the dynamic linker found
        // libzfs/libnvpair but they didn't export a function we need —
        // likely an unsupported OpenZFS version. Surface the symbol name
        // so users can report a useful error.
        //
        // SAFETY: both handles are non-null per try_dlopen_sonames contract.
        unsafe {
            let libzfs_init: LibzfsInitFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"libzfs_init")?);
            let libzfs_fini: LibzfsFiniFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"libzfs_fini")?);
            let libzfs_error_description: LibzfsErrorDescriptionFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"libzfs_error_description")?);
            let zpool_iter: ZpoolIterFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"zpool_iter")?);
            let zpool_get_name: ZpoolGetNameFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"zpool_get_name")?);
            let zpool_get_state: ZpoolGetStateFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"zpool_get_state")?);
            let zpool_get_config: ZpoolGetConfigFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"zpool_get_config")?);
            let zpool_get_prop_int: ZpoolGetPropIntFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"zpool_get_prop_int")?);
            let zpool_close: ZpoolCloseFn =
                std::mem::transmute(dlsym_required(zfs_handle, c"zpool_close")?);

            let nvlist_lookup_string: NvlistLookupStringFn =
                std::mem::transmute(dlsym_required(nvpair_handle, c"nvlist_lookup_string")?);
            let nvlist_lookup_uint64: NvlistLookupUint64Fn =
                std::mem::transmute(dlsym_required(nvpair_handle, c"nvlist_lookup_uint64")?);
            let nvlist_lookup_nvlist: NvlistLookupNvlistFn =
                std::mem::transmute(dlsym_required(nvpair_handle, c"nvlist_lookup_nvlist")?);
            let nvlist_lookup_nvlist_array: NvlistLookupNvlistArrayFn = std::mem::transmute(
                dlsym_required(nvpair_handle, c"nvlist_lookup_nvlist_array")?,
            );
            let nvlist_lookup_uint64_array: NvlistLookupUint64ArrayFn = std::mem::transmute(
                dlsym_required(nvpair_handle, c"nvlist_lookup_uint64_array")?,
            );

            Ok(Libzfs {
                nvpair_handle,
                zfs_handle,
                libzfs_init,
                libzfs_fini,
                libzfs_error_description,
                zpool_iter,
                zpool_get_name,
                zpool_get_state,
                zpool_get_config,
                zpool_get_prop_int,
                zpool_close,
                nvlist_lookup_string,
                nvlist_lookup_uint64,
                nvlist_lookup_nvlist,
                nvlist_lookup_nvlist_array,
                nvlist_lookup_uint64_array,
            })
        }
    }
}

impl Drop for Libzfs {
    fn drop(&mut self) {
        // SAFETY: both handles were returned by successful dlopen calls
        // and not yet dlclose'd. Safe to close in either order — libzfs
        // has finished using libnvpair by the time Drop runs, because
        // LibzfsPoolsSource's Drop runs first (unwind order) and calls
        // libzfs_fini() to release the libzfs handle.
        unsafe {
            if !self.zfs_handle.is_null() {
                dlclose(self.zfs_handle);
                self.zfs_handle = ptr::null_mut();
            }
            if !self.nvpair_handle.is_null() {
                dlclose(self.nvpair_handle);
                self.nvpair_handle = ptr::null_mut();
            }
        }
    }
}
