//! `LibzfsPoolsSource` — the real libzfs-backed implementation of
//! `PoolsSource`. Wraps a `*mut libzfs_handle_t` and walks zpool handles +
//! config nvlists into the plain-Rust domain types from `super::types`.
//!
//! # Walkthrough of a refresh tick
//!
//! 1. `zpool_iter` is called with a thunk that collects a `Vec<*mut
//!    zpool_handle_t>` into a caller-owned buffer. libzfs hands us a borrow
//!    to each zpool_handle_t; ownership transfers to us if we return 0 from
//!    the callback (which we always do).
//! 2. For each collected handle:
//!    a. Read name via `zpool_get_name`.
//!    b. Read size / free / allocated / fragmentation via `zpool_get_prop_int`.
//!    c. Read the pool config nvlist via `zpool_get_config`.
//!    d. Look up `vdev_tree` nvlist inside the config and recursively walk
//!       it into a `VdevNode`, also filling in vdev_stats for each level.
//!    e. Read `scan_stats` off the *root* vdev nvlist for `ScrubState`.
//!    f. Close the handle with `zpool_close`.
//! 3. Replace `self.snapshot` with the freshly built `Vec<PoolInfo>`.
//!
//! The config nvlist is owned by libzfs (borrowed from the handle), and
//! every `*const c_char` it exposes is only valid until `zpool_close`.
//! Every string is eagerly copied into an owned `String` *before* the
//! handle is closed so the `PoolInfo` values returned to callers are
//! standalone.

use anyhow::{anyhow, Result};
use std::ffi::{c_int, c_uint, c_void, CStr};
use std::ptr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::ffi;
use super::types::{
    ErrorCounts, PoolHealth, PoolInfo, ScrubState, VdevKind, VdevNode, VdevState,
};
use super::PoolsSource;

/// Owns a `*mut libzfs_handle_t`. Dropping this value calls `libzfs_fini`
/// on the handle.
pub struct LibzfsPoolsSource {
    handle: *mut ffi::libzfs_handle_t,
    snapshot: Vec<PoolInfo>,
}

// libzfs_handle_t is not thread-safe in general, but `LibzfsPoolsSource`
// lives inside `App` which is driven from a single thread (the event loop).
// Manual `Send` to satisfy `Box<dyn PoolsSource>` — the trait object must
// be Send so it can live behind `Option<Box<dyn PoolsSource>>` in `App`.
// NOT `Sync` — concurrent access is unsound.
unsafe impl Send for LibzfsPoolsSource {}

impl LibzfsPoolsSource {
    /// Attempt to initialize libzfs. Returns an error describing why
    /// `libzfs_init` returned null — typically `/dev/zfs not accessible`
    /// (kernel module not loaded), or a permission failure.
    pub fn new() -> Result<Self> {
        // SAFETY: libzfs_init takes no arguments and returns either a valid
        // handle or null. We check for null before treating it as valid.
        let handle = unsafe { ffi::libzfs_init() };
        if handle.is_null() {
            return Err(anyhow!(
                "libzfs_init returned null — is the ZFS kernel module loaded and /dev/zfs accessible?"
            ));
        }
        Ok(Self {
            handle,
            snapshot: Vec::new(),
        })
    }

    /// Fetch the last-known libzfs error string for the current handle.
    #[allow(dead_code)]
    fn error_description(&self) -> String {
        if self.handle.is_null() {
            return "(no libzfs handle)".into();
        }
        // SAFETY: handle is non-null and was returned by a successful
        // libzfs_init. libzfs_error_description returns a pointer into
        // libzfs-internal storage; we copy it out immediately.
        let ptr = unsafe { ffi::libzfs_error_description(self.handle) };
        if ptr.is_null() {
            "(no error)".into()
        } else {
            // SAFETY: libzfs guarantees the returned string is nul-terminated.
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Drop for LibzfsPoolsSource {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: we hold the only reference to the handle, it was
            // returned from a successful libzfs_init, and we haven't yet
            // called libzfs_fini on it.
            unsafe { ffi::libzfs_fini(self.handle) };
            self.handle = ptr::null_mut();
        }
    }
}

impl PoolsSource for LibzfsPoolsSource {
    fn refresh(&mut self) -> Result<()> {
        // Collect zpool handles into a Vec via a zpool_iter callback.
        let mut handles: Vec<*mut ffi::zpool_handle_t> = Vec::new();

        // SAFETY:
        // - `self.handle` is non-null (constructor guarantees it).
        // - `collect_handle` has the `zpool_iter_f` signature and is sound
        //   against the `&mut Vec` it receives via `data`.
        // - `&mut handles as *mut _ as *mut c_void` is valid for the
        //   duration of the `zpool_iter` call (the Vec lives on our stack
        //   and isn't moved).
        let rc = unsafe {
            ffi::zpool_iter(
                self.handle,
                collect_handle,
                &mut handles as *mut Vec<*mut ffi::zpool_handle_t> as *mut c_void,
            )
        };
        if rc != 0 {
            // Non-zero return from zpool_iter means the callback returned
            // non-zero, or libzfs encountered an error iterating. Our
            // callback always returns 0, so this is the latter.
            return Err(anyhow!(
                "zpool_iter returned {rc}: {}",
                self.error_description()
            ));
        }

        // For each handle, build a PoolInfo and close.
        let mut pools: Vec<PoolInfo> = Vec::with_capacity(handles.len());
        for zhp in handles {
            let info = build_pool_info(zhp);
            // SAFETY: zhp was returned by zpool_iter and transferred to us
            // (we returned 0 from the callback, taking ownership). Close it
            // now that we've finished extracting data.
            unsafe { ffi::zpool_close(zhp) };
            match info {
                Ok(p) => pools.push(p),
                Err(e) => {
                    // Swallow a single bad pool — don't let it poison the
                    // whole refresh. Surface via error_description later if
                    // needed.
                    eprintln!("zftop: skipping pool during refresh: {e}");
                }
            }
        }

        self.snapshot = pools;
        Ok(())
    }

    fn pools(&self) -> Vec<PoolInfo> {
        self.snapshot.clone()
    }
}

// ---------------------------------------------------------------------------
// zpool_iter callback: append each handle to a Vec and return 0 so libzfs
// transfers ownership to the caller.
// ---------------------------------------------------------------------------

unsafe extern "C" fn collect_handle(
    zhp: *mut ffi::zpool_handle_t,
    data: *mut c_void,
) -> c_int {
    // SAFETY: `data` was passed in as `&mut Vec<...> as *mut c_void` by
    // `refresh()` above; cast it back. The Vec outlives this callback
    // (it lives on the caller's stack for the duration of zpool_iter).
    let vec = unsafe { &mut *(data as *mut Vec<*mut ffi::zpool_handle_t>) };
    vec.push(zhp);
    0
}

// ---------------------------------------------------------------------------
// Build a PoolInfo for a single zpool handle.
//
// All string copies happen before returning — once the caller closes the
// zpool handle, every `*const c_char` libzfs gave us becomes dangling.
// ---------------------------------------------------------------------------

fn build_pool_info(zhp: *mut ffi::zpool_handle_t) -> Result<PoolInfo> {
    // Name. SAFETY: zpool_get_name returns a pointer into handle-internal
    // storage; it's valid until zpool_close. We copy it immediately.
    let name = unsafe {
        let ptr = ffi::zpool_get_name(zhp);
        if ptr.is_null() {
            return Err(anyhow!("zpool_get_name returned null"));
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    };

    // Properties via zpool_get_prop_int.
    // SAFETY: zhp is non-null, valid for the duration of this function,
    // and the prop enum values are hand-copied from sys/fs/zfs.h.
    let size_bytes = unsafe { ffi::zpool_get_prop_int(zhp, ffi::ZPOOL_PROP_SIZE, ptr::null_mut()) };
    let allocated_bytes =
        unsafe { ffi::zpool_get_prop_int(zhp, ffi::ZPOOL_PROP_ALLOCATED, ptr::null_mut()) };
    let free_bytes = unsafe { ffi::zpool_get_prop_int(zhp, ffi::ZPOOL_PROP_FREE, ptr::null_mut()) };
    // Fragmentation is a percentage 0..=100, or a sentinel (u64::MAX) when
    // unavailable. Store as `Option<u8>`.
    let frag_raw =
        unsafe { ffi::zpool_get_prop_int(zhp, ffi::ZPOOL_PROP_FRAGMENTATION, ptr::null_mut()) };
    let fragmentation_pct = if frag_raw <= 100 {
        Some(frag_raw as u8)
    } else {
        None
    };

    // Config nvlist (borrowed from the handle — don't free).
    // SAFETY: zpool_get_config returns an nvlist owned by the handle. The
    // second arg is for the caller-out "old config"; we don't need it.
    let config =
        unsafe { ffi::zpool_get_config(zhp, ptr::null_mut()) };
    if config.is_null() {
        return Err(anyhow!("zpool_get_config returned null for '{name}'"));
    }

    // Look up vdev_tree child of the config.
    let vdev_tree =
        nvlist_get_nvlist(config, ffi::ZPOOL_CONFIG_VDEV_TREE)?;

    // Walk the root vdev. We use the pool name as the root node's display
    // name (matches `zpool status` layout).
    let root_vdev = walk_root_vdev(vdev_tree, &name)?;

    // Scan state lives on the root vdev nvlist.
    let scrub = read_scan_state(vdev_tree);

    // Sum errors across the whole tree.
    let errors_sum = root_vdev.total_errors();
    let errors = ErrorCounts {
        // We don't track per-type totals at the pool level — store the
        // summed total in `read` and leave write/checksum at 0. The UI
        // surfaces either `errors.sum()` or the individual vdev counts,
        // never the pool-level ErrorCounts decomposed.
        //
        // NOTE: this is a minor simplification — the plan's
        // `PoolInfo::errors` field is documented as the sum across all
        // vdevs, and callers that care about per-type breakdown walk
        // VdevNode::errors directly. Keeping it as a plain `u64` sum in
        // `errors.read` avoids an API split.
        read: errors_sum,
        write: 0,
        checksum: 0,
    };

    Ok(PoolInfo {
        name,
        health: derive_pool_health(&root_vdev),
        allocated_bytes,
        size_bytes,
        free_bytes,
        fragmentation_pct,
        scrub,
        errors,
        root_vdev,
    })
}

/// Pool-level health is the root vdev's state, mapped from `vdev_state_t`
/// into our `PoolHealth` enum. The root vdev is "HEALTHY" when every child
/// is HEALTHY, DEGRADED when one or more children are DEGRADED/OFFLINE/etc.
fn derive_pool_health(root: &VdevNode) -> PoolHealth {
    match root.state {
        VdevState::Online => PoolHealth::Online,
        VdevState::Degraded => PoolHealth::Degraded,
        VdevState::Faulted => PoolHealth::Faulted,
        VdevState::Offline => PoolHealth::Offline,
        VdevState::Removed => PoolHealth::Removed,
        VdevState::Unavail => PoolHealth::Unavail,
    }
}

// ---------------------------------------------------------------------------
// Recursive vdev tree walker
// ---------------------------------------------------------------------------

/// Top-level walker. The vdev_tree nvlist IS the pool's root vdev; its
/// children are the top-level vdevs (raidz groups, mirrors, single disks,
/// log/cache/spare groups). Each child is walked via `walk_child_vdev`.
///
/// The root VdevNode takes the pool's name as its display label and its
/// state/errors from the vdev_tree's own vdev_stats. size_bytes is the
/// deflated allocatable size (same as `zpool list`'s SIZE column).
fn walk_root_vdev(nvl: *mut ffi::nvlist_t, pool_name: &str) -> Result<VdevNode> {
    let (state, errors) = read_vdev_stats(nvl);
    let size_bytes =
        read_vdev_stats_raw(nvl).and_then(|stats| stats.get(ffi::VS_IDX_SPACE).copied());

    let mut children = Vec::new();
    if let Ok((child_nvls, count)) = nvlist_get_nvlist_array(nvl, ffi::ZPOOL_CONFIG_CHILDREN) {
        for i in 0..count {
            // SAFETY: libzfs returned an nvlist_t** + element count. Indices
            // 0..count are valid nvlist pointers for the lifetime of the
            // parent config nvlist, which outlives this recursive walk.
            let child_nvl = unsafe { *child_nvls.add(i) };
            if child_nvl.is_null() {
                continue;
            }
            match walk_child_vdev(child_nvl, None) {
                Ok(child) => children.push(child),
                Err(e) => eprintln!("zftop: skipping top-level vdev during walk: {e}"),
            }
        }
    }

    Ok(VdevNode {
        name: pool_name.to_string(),
        kind: VdevKind::Root,
        state,
        size_bytes,
        errors,
        children,
    })
}

/// Recursive walker for non-root vdevs. `parent_group` carries Log/Cache/
/// Spare group context down to leaves so they can be tagged as
/// LogVdev/CacheVdev/SpareVdev for render purposes.
fn walk_child_vdev(
    nvl: *mut ffi::nvlist_t,
    parent_group: Option<VdevKind>,
) -> Result<VdevNode> {
    let type_str = nvlist_get_string(nvl, ffi::ZPOOL_CONFIG_TYPE).unwrap_or_default();

    // Classify by type_str. Group-context only affects leaf disk/file
    // tagging (LogVdev vs plain Disk).
    let kind = match (parent_group, type_str.as_str()) {
        (_, ffi::VDEV_TYPE_MIRROR) => VdevKind::Mirror,
        (_, ffi::VDEV_TYPE_RAIDZ) => VdevKind::Raidz,
        (_, ffi::VDEV_TYPE_DRAID) => VdevKind::Raidz,
        (_, ffi::VDEV_TYPE_REPLACING) => VdevKind::Mirror,
        (Some(VdevKind::LogGroup), ffi::VDEV_TYPE_DISK | ffi::VDEV_TYPE_FILE) => VdevKind::LogVdev,
        (Some(VdevKind::CacheGroup), ffi::VDEV_TYPE_DISK | ffi::VDEV_TYPE_FILE) => {
            VdevKind::CacheVdev
        }
        (Some(VdevKind::SpareGroup), ffi::VDEV_TYPE_DISK | ffi::VDEV_TYPE_FILE) => {
            VdevKind::SpareVdev
        }
        (_, ffi::VDEV_TYPE_DISK) => VdevKind::Disk,
        (_, ffi::VDEV_TYPE_FILE) => VdevKind::File,
        (_, _) => VdevKind::Disk, // fallback for unknown types
    };

    // Display name. Leaf disks/files get their `path` (stripped of "/dev/"
    // prefix for readability). Groups/RAIDZ/Mirror inherit the type_str
    // as their label; libzfs's config nvlist doesn't expose an index like
    // "raidz1-0" on the nvlist directly — that label comes from
    // `zpool_vdev_name` which we don't call. Using the raw type_str is
    // fine for v0.2.
    let name = match kind {
        VdevKind::Disk
        | VdevKind::File
        | VdevKind::LogVdev
        | VdevKind::CacheVdev
        | VdevKind::SpareVdev => nvlist_get_string(nvl, ffi::ZPOOL_CONFIG_PATH)
            .map(|p| p.strip_prefix("/dev/").unwrap_or(&p).to_string())
            .unwrap_or_else(|_| type_str.clone()),
        _ => type_str.clone(),
    };

    let (state, errors) = read_vdev_stats(nvl);

    let size_bytes = match kind {
        VdevKind::LogGroup | VdevKind::CacheGroup | VdevKind::SpareGroup => None,
        _ => read_vdev_stats_raw(nvl).and_then(|stats| stats.get(ffi::VS_IDX_SPACE).copied()),
    };

    // Recurse on children. Group-context for leaves propagates down.
    let child_group = match kind {
        VdevKind::LogGroup | VdevKind::CacheGroup | VdevKind::SpareGroup => Some(kind),
        _ => parent_group, // inherit — raidz/mirror inside a log group still ultimately contain log leaves
    };

    let mut children = Vec::new();
    if let Ok((child_nvls, count)) = nvlist_get_nvlist_array(nvl, ffi::ZPOOL_CONFIG_CHILDREN) {
        for i in 0..count {
            // SAFETY: same as walk_root_vdev's recursion.
            let child_nvl = unsafe { *child_nvls.add(i) };
            if child_nvl.is_null() {
                continue;
            }
            match walk_child_vdev(child_nvl, child_group) {
                Ok(child) => children.push(child),
                Err(e) => eprintln!("zftop: skipping child vdev during walk: {e}"),
            }
        }
    }

    Ok(VdevNode {
        name,
        kind,
        state,
        size_bytes,
        errors,
        children,
    })
}

/// Read `vdev_stats` uint64 array and extract (state, errors). Returns
/// `(Online, zeros)` when the array is missing or shorter than we expect —
/// an older libzfs with a different VS_ZIO_TYPES layout, or a top-level
/// group node (logs/cache/spares wrapper) that doesn't carry stats.
fn read_vdev_stats(nvl: *mut ffi::nvlist_t) -> (VdevState, ErrorCounts) {
    let Some(stats) = read_vdev_stats_raw(nvl) else {
        return (VdevState::Online, ErrorCounts::default());
    };
    let state_u64 = stats.get(ffi::VS_IDX_STATE).copied().unwrap_or(0);
    let state = map_vdev_state(state_u64);
    let errors = if stats.len() >= ffi::VS_MIN_LEN {
        ErrorCounts {
            read: stats[ffi::VS_IDX_READ_ERRORS],
            write: stats[ffi::VS_IDX_WRITE_ERRORS],
            checksum: stats[ffi::VS_IDX_CHECKSUM_ERRORS],
        }
    } else {
        ErrorCounts::default()
    };
    (state, errors)
}

/// Return the raw vdev_stats uint64 slice, or None if the key is missing.
fn read_vdev_stats_raw(nvl: *mut ffi::nvlist_t) -> Option<Vec<u64>> {
    let mut out_ptr: *mut u64 = ptr::null_mut();
    let mut nelem: c_uint = 0;
    // SAFETY: nvl is a valid nvlist_t from libzfs. The key C-string is
    // nul-terminated (static const). The out-pointers are writable and
    // only read on rc==0.
    let rc = unsafe {
        ffi::nvlist_lookup_uint64_array(
            nvl,
            ffi::ZPOOL_CONFIG_VDEV_STATS.as_ptr(),
            &mut out_ptr,
            &mut nelem,
        )
    };
    if rc != 0 || out_ptr.is_null() {
        return None;
    }
    // SAFETY: nelem is the element count libzfs wrote; the backing memory
    // is owned by the parent nvlist and lives at least until we close the
    // zpool handle. Copy into an owned Vec so we don't keep a borrow.
    let slice = unsafe { std::slice::from_raw_parts(out_ptr, nelem as usize) };
    Some(slice.to_vec())
}

/// Read `scan_stats` uint64 array from a vdev nvlist (typically the root
/// vdev). Decode into a `ScrubState`. Returns `ScrubState::Never` when the
/// key is missing or the array is shorter than we expect.
fn read_scan_state(nvl: *mut ffi::nvlist_t) -> ScrubState {
    let mut out_ptr: *mut u64 = ptr::null_mut();
    let mut nelem: c_uint = 0;
    // SAFETY: same as read_vdev_stats_raw.
    let rc = unsafe {
        ffi::nvlist_lookup_uint64_array(
            nvl,
            ffi::ZPOOL_CONFIG_SCAN_STATS.as_ptr(),
            &mut out_ptr,
            &mut nelem,
        )
    };
    if rc != 0 || out_ptr.is_null() || (nelem as usize) < ffi::PSS_MIN_LEN {
        return ScrubState::Never;
    }
    // SAFETY: nelem is the element count libzfs wrote; backing memory is
    // owned by the parent nvlist.
    let stats = unsafe { std::slice::from_raw_parts(out_ptr, nelem as usize) };
    let func = stats[ffi::PSS_IDX_FUNC];
    let state = stats[ffi::PSS_IDX_STATE];

    match state {
        ffi::DSS_NONE => ScrubState::Never,
        ffi::DSS_SCANNING => {
            let to_examine = stats[ffi::PSS_IDX_TO_EXAMINE];
            let examined = stats[ffi::PSS_IDX_EXAMINED];
            let progress_pct = if to_examine == 0 {
                0
            } else {
                ((examined * 100) / to_examine).min(100) as u8
            };
            let start_time = stats[ffi::PSS_IDX_START_TIME];
            let speed = if start_time > 0 {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let elapsed = now.saturating_sub(start_time);
                if elapsed > 0 {
                    Some(examined / elapsed)
                } else {
                    None
                }
            } else {
                None
            };
            let eta = if examined > 0 && to_examine > examined {
                let speed_val = speed.unwrap_or(0);
                if speed_val > 0 {
                    Some((to_examine - examined) / speed_val)
                } else {
                    None
                }
            } else {
                None
            };
            ScrubState::InProgress {
                progress_pct,
                eta_seconds: eta,
                speed_bytes_per_sec: speed,
                is_resilver: func == ffi::POOL_SCAN_RESILVER,
            }
        }
        ffi::DSS_FINISHED => {
            let end_time = stats[ffi::PSS_IDX_END_TIME];
            let errors_repaired = stats[ffi::PSS_IDX_ERRORS];
            let completed_at = UNIX_EPOCH + Duration::from_secs(end_time);
            ScrubState::Finished {
                completed_at,
                errors_repaired,
            }
        }
        ffi::DSS_CANCELED => ScrubState::Error,
        _ => ScrubState::Never,
    }
}

fn map_vdev_state(v: u64) -> VdevState {
    match v {
        ffi::VDEV_STATE_HEALTHY => VdevState::Online,
        ffi::VDEV_STATE_DEGRADED => VdevState::Degraded,
        ffi::VDEV_STATE_FAULTED | ffi::VDEV_STATE_CANT_OPEN => VdevState::Faulted,
        ffi::VDEV_STATE_OFFLINE | ffi::VDEV_STATE_CLOSED => VdevState::Offline,
        ffi::VDEV_STATE_REMOVED => VdevState::Removed,
        ffi::VDEV_STATE_UNKNOWN => VdevState::Unavail,
        _ => VdevState::Unavail,
    }
}

// ---------------------------------------------------------------------------
// Safe nvlist lookup helpers
//
// These wrap the raw FFI lookup functions in something callable from safe
// code, copying strings out into owned `String`s where appropriate and
// returning `anyhow::Error` on libzfs-reported failures.
// ---------------------------------------------------------------------------

fn nvlist_get_string(nvl: *const ffi::nvlist_t, key: &CStr) -> Result<String> {
    let mut out: *const std::ffi::c_char = ptr::null();
    // SAFETY: nvl is a valid nvlist_t borrowed from libzfs; the key is
    // nul-terminated (CStr contract); out is a writable slot only read
    // on rc == 0.
    let rc = unsafe { ffi::nvlist_lookup_string(nvl, key.as_ptr(), &mut out) };
    if rc != 0 || out.is_null() {
        return Err(anyhow!(
            "nvlist_lookup_string({}) failed: rc={rc}",
            key.to_string_lossy()
        ));
    }
    // SAFETY: libzfs guarantees the returned string is nul-terminated and
    // lives at least as long as the parent nvlist (which outlives this call).
    let s = unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned();
    Ok(s)
}

#[allow(dead_code)]
fn nvlist_get_uint64(nvl: *const ffi::nvlist_t, key: &CStr) -> Result<u64> {
    let mut out: u64 = 0;
    // SAFETY: nvl + key are valid; out is a writable stack slot.
    let rc = unsafe { ffi::nvlist_lookup_uint64(nvl, key.as_ptr(), &mut out) };
    if rc != 0 {
        return Err(anyhow!(
            "nvlist_lookup_uint64({}) failed: rc={rc}",
            key.to_string_lossy()
        ));
    }
    Ok(out)
}

fn nvlist_get_nvlist(nvl: *mut ffi::nvlist_t, key: &CStr) -> Result<*mut ffi::nvlist_t> {
    let mut out: *mut ffi::nvlist_t = ptr::null_mut();
    // SAFETY: nvl + key are valid; out is a writable stack slot.
    let rc = unsafe { ffi::nvlist_lookup_nvlist(nvl, key.as_ptr(), &mut out) };
    if rc != 0 || out.is_null() {
        return Err(anyhow!(
            "nvlist_lookup_nvlist({}) failed: rc={rc}",
            key.to_string_lossy()
        ));
    }
    Ok(out)
}

fn nvlist_get_nvlist_array(
    nvl: *mut ffi::nvlist_t,
    key: &CStr,
) -> Result<(*mut *mut ffi::nvlist_t, usize)> {
    let mut out_ptr: *mut *mut ffi::nvlist_t = ptr::null_mut();
    let mut nelem: c_uint = 0;
    // SAFETY: nvl + key are valid; out-slots are writable.
    let rc = unsafe {
        ffi::nvlist_lookup_nvlist_array(nvl, key.as_ptr(), &mut out_ptr, &mut nelem)
    };
    if rc != 0 || out_ptr.is_null() {
        return Err(anyhow!(
            "nvlist_lookup_nvlist_array({}) failed: rc={rc}",
            key.to_string_lossy()
        ));
    }
    Ok((out_ptr, nelem as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test — only meaningful on a host with libzfs + /dev/zfs
    /// (kernel module loaded, `/dev/zfs` accessible). Marked `#[ignore]`
    /// so regular `cargo test` runs on dev hosts without ZFS skip it;
    /// run with `cargo nextest run --run-ignored only` when validating
    /// against a live ZFS kernel module.
    #[test]
    #[ignore]
    fn libzfs_init_on_live_host() {
        match LibzfsPoolsSource::new() {
            Ok(_) => eprintln!("libzfs_init succeeded"),
            Err(e) => panic!("libzfs_init failed: {e}"),
        }
    }

    /// Smoke dump of the full refresh() path against a live libzfs.
    /// Prints the resulting `Vec<PoolInfo>` to stderr so we can eyeball
    /// it against `zpool list -Hp` / `zpool status` output.
    #[test]
    #[ignore]
    fn libzfs_refresh_and_dump_on_live_host() {
        let mut src = LibzfsPoolsSource::new().expect("libzfs_init");
        src.refresh().expect("refresh");
        let pools = src.pools();
        eprintln!("pools.len() = {}", pools.len());
        for p in &pools {
            eprintln!(
                "- {} ({:?}) {}/{}B frag={:?} scrub={:?} errors={}",
                p.name,
                p.health,
                p.allocated_bytes,
                p.size_bytes,
                p.fragmentation_pct,
                p.scrub,
                p.errors.sum()
            );
            dump_vdev(&p.root_vdev, 0);
        }
    }

    #[cfg(test)]
    fn dump_vdev(node: &VdevNode, depth: usize) {
        let indent = "  ".repeat(depth);
        eprintln!(
            "{}- {} [{:?}] state={:?} size={:?} errors={}",
            indent,
            node.name,
            node.kind,
            node.state,
            node.size_bytes,
            node.errors.sum()
        );
        for child in &node.children {
            dump_vdev(child, depth + 1);
        }
    }

    /// Live libzfs integration test. Runs automatically on FreeBSD builds
    /// (where the bsd-1 CI host has libzfs in base + at least one imported
    /// test pool set up by `scripts/setup-bsd-ci.sh`). On Linux this test
    /// is cfg-gated out so dev hosts without loaded ZFS skip it silently —
    /// Linux CI containers don't have the kernel module either.
    ///
    /// Asserts:
    /// - `LibzfsPoolsSource::new()` succeeds.
    /// - `refresh()` returns Ok.
    /// - At least one pool is reported.
    /// - The first pool has a non-empty name.
    /// - The first pool's root vdev has at least one child vdev (so we
    ///   know the recursive walker ran for real, not just the root row).
    #[cfg(target_os = "freebsd")]
    #[test]
    fn libzfs_freebsd_integration() {
        let mut src = LibzfsPoolsSource::new()
            .expect("libzfs_init on bsd-1 should succeed — is libzfs linked?");
        src.refresh()
            .expect("refresh should not error on a known-good host");

        let pools = src.pools();
        assert!(
            !pools.is_empty(),
            "bsd-1 is provisioned with at least one test pool — refresh returned none"
        );

        let first = &pools[0];
        assert!(!first.name.is_empty(), "first pool should have a name");
        assert!(
            !first.root_vdev.children.is_empty(),
            "first pool's root vdev should have at least one child vdev"
        );
    }
}
