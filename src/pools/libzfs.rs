//! `LibzfsPoolsSource` — the real libzfs-backed implementation of
//! `PoolsSource`. Wraps a `*mut libzfs_handle_t` and walks zpool handles +
//! config nvlists into the plain-Rust domain types from `super::types`.
//!
//! libzfs itself is loaded at runtime via `dlopen` (see `ffi::Libzfs`)
//! so the binary has no `DT_NEEDED = libzfs.so.N` entry and works across
//! every soname bump OpenZFS has shipped since 0.7.
//!
//! # Walkthrough of a refresh tick
//!
//! 1. `zpool_iter` is called with a thunk that collects a `Vec<*mut
//!    zpool_handle_t>` into a caller-owned buffer. libzfs hands us a borrow
//!    to each zpool_handle_t; ownership transfers to us when we return 0
//!    from the callback.
//! 2. For each collected handle:
//!    a. Read name via `zpool_get_name`.
//!    b. Read size / free / allocated / fragmentation via `zpool_get_prop_int`.
//!    c. Read the pool config nvlist via `zpool_get_config`.
//!    d. Look up `vdev_tree` nvlist inside the config and recursively walk
//!       it into a `VdevNode`, filling in vdev_stats for each level.
//!    e. Read `scan_stats` off the root vdev nvlist for `ScrubState`.
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

use super::ffi::{self, Libzfs};
use super::types::{
    ErrorCounts, PoolHealth, PoolInfo, ScrubState, VdevKind, VdevNode, VdevState,
};
use super::PoolsSource;

/// Owns a runtime-loaded `Libzfs` (dlopen handles + function pointers) and
/// a `*mut libzfs_handle_t` from `libzfs_init`. Dropping this value calls
/// `libzfs_fini` on the handle, then drops `Libzfs` which dlcloses the
/// shared libraries.
pub struct LibzfsPoolsSource {
    lz: Libzfs,
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
    /// dlopen libzfs+libnvpair, then call `libzfs_init()` to get a usable
    /// handle. Returns an error if either the dlopen/dlsym or the init
    /// step fails — the latter is common when `/dev/zfs` isn't accessible
    /// (kernel module not loaded, permission denied).
    pub fn new() -> Result<Self> {
        let lz = Libzfs::load()?;
        // SAFETY: dlopen+dlsym succeeded, so libzfs_init is a valid
        // function pointer. libzfs_init takes no arguments and returns
        // either a valid handle or null — we null-check before using.
        let handle = unsafe { (lz.libzfs_init)() };
        if handle.is_null() {
            return Err(anyhow!(
                "libzfs_init returned null — is the ZFS kernel module loaded and /dev/zfs accessible?"
            ));
        }
        Ok(Self {
            lz,
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
        let ptr = unsafe { (self.lz.libzfs_error_description)(self.handle) };
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
            unsafe { (self.lz.libzfs_fini)(self.handle) };
            self.handle = ptr::null_mut();
        }
        // `self.lz` drops after this method returns — its Drop impl
        // dlcloses both libzfs.so and libnvpair.so in that order.
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
            (self.lz.zpool_iter)(
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
            let info = build_pool_info(&self.lz, zhp);
            // SAFETY: zhp was returned by zpool_iter and transferred to us
            // (we returned 0 from the callback, taking ownership). Close it
            // now that we've finished extracting data.
            unsafe { (self.lz.zpool_close)(zhp) };
            match info {
                Ok(p) => pools.push(p),
                Err(e) => {
                    // Swallow a single bad pool — don't let it poison the
                    // whole refresh.
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
// transfers ownership to the caller. This is a plain Rust function with
// no libzfs refs — it doesn't need access to `Libzfs`.
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

fn build_pool_info(lz: &Libzfs, zhp: *mut ffi::zpool_handle_t) -> Result<PoolInfo> {
    // Name. SAFETY: zpool_get_name returns a pointer into handle-internal
    // storage; it's valid until zpool_close. We copy it immediately.
    let name = unsafe {
        let ptr = (lz.zpool_get_name)(zhp);
        if ptr.is_null() {
            return Err(anyhow!("zpool_get_name returned null"));
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    };

    // Properties via zpool_get_prop_int.
    // SAFETY: zhp is non-null, valid for the duration of this function,
    // and the prop enum values are hand-copied from sys/fs/zfs.h.
    let size_bytes =
        unsafe { (lz.zpool_get_prop_int)(zhp, ffi::ZPOOL_PROP_SIZE, ptr::null_mut()) };
    let allocated_bytes =
        unsafe { (lz.zpool_get_prop_int)(zhp, ffi::ZPOOL_PROP_ALLOCATED, ptr::null_mut()) };
    let free_bytes =
        unsafe { (lz.zpool_get_prop_int)(zhp, ffi::ZPOOL_PROP_FREE, ptr::null_mut()) };
    // Fragmentation is a percentage 0..=100, or a sentinel (u64::MAX) when
    // unavailable. Store as `Option<u8>`.
    let frag_raw = unsafe {
        (lz.zpool_get_prop_int)(zhp, ffi::ZPOOL_PROP_FRAGMENTATION, ptr::null_mut())
    };
    let fragmentation_pct = if frag_raw <= 100 {
        Some(frag_raw as u8)
    } else {
        None
    };

    // Config nvlist (borrowed from the handle — don't free).
    // SAFETY: zpool_get_config returns an nvlist owned by the handle. The
    // second arg is for the caller-out "old config"; we don't need it.
    let config = unsafe { (lz.zpool_get_config)(zhp, ptr::null_mut()) };
    if config.is_null() {
        return Err(anyhow!("zpool_get_config returned null for '{name}'"));
    }

    // Look up vdev_tree child of the config.
    let vdev_tree = nvlist_get_nvlist(lz, config, ffi::ZPOOL_CONFIG_VDEV_TREE)?;

    // Walk the root vdev. We use the pool name as the root node's display
    // name (matches `zpool status` layout).
    let root_vdev = walk_root_vdev(lz, vdev_tree, &name)?;

    // Scan state lives on the root vdev nvlist.
    let scrub = read_scan_state(lz, vdev_tree);

    // Sum errors across the whole tree.
    let errors_sum = root_vdev.total_errors();
    let errors = ErrorCounts {
        // We don't track per-type totals at the pool level — store the
        // summed total in `read` and leave write/checksum at 0. The UI
        // surfaces either `errors.sum()` or the individual vdev counts.
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
/// into our `PoolHealth` enum.
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
fn walk_root_vdev(
    lz: &Libzfs,
    nvl: *mut ffi::nvlist_t,
    pool_name: &str,
) -> Result<VdevNode> {
    let (state, errors) = read_vdev_stats(lz, nvl);
    let size_bytes =
        read_vdev_stats_raw(lz, nvl).and_then(|stats| stats.get(ffi::VS_IDX_SPACE).copied());

    let mut children = Vec::new();
    if let Ok((child_nvls, count)) =
        nvlist_get_nvlist_array(lz, nvl, ffi::ZPOOL_CONFIG_CHILDREN)
    {
        for i in 0..count {
            // SAFETY: libzfs returned an nvlist_t** + element count. Indices
            // 0..count are valid nvlist pointers for the lifetime of the
            // parent config nvlist, which outlives this recursive walk.
            let child_nvl = unsafe { *child_nvls.add(i) };
            if child_nvl.is_null() {
                continue;
            }
            match walk_child_vdev(lz, child_nvl, None) {
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
        device_path: None,
    })
}

/// Recursive walker for non-root vdevs. `parent_group` carries Log/Cache/
/// Spare group context down to leaves so they can be tagged as
/// LogVdev/CacheVdev/SpareVdev for render purposes.
fn walk_child_vdev(
    lz: &Libzfs,
    nvl: *mut ffi::nvlist_t,
    parent_group: Option<VdevKind>,
) -> Result<VdevNode> {
    let type_str = nvlist_get_string(lz, nvl, ffi::ZPOOL_CONFIG_TYPE).unwrap_or_default();

    let kind = match (parent_group, type_str.as_str()) {
        (_, ffi::VDEV_TYPE_MIRROR) => VdevKind::Mirror,
        (_, ffi::VDEV_TYPE_RAIDZ | ffi::VDEV_TYPE_DRAID) => {
            let parity = nvlist_get_uint64(lz, nvl, ffi::ZPOOL_CONFIG_NPARITY)
                .unwrap_or(1) as u8;
            VdevKind::Raidz { parity }
        }
        (_, ffi::VDEV_TYPE_REPLACING) => VdevKind::Mirror,
        (Some(VdevKind::LogGroup), ffi::VDEV_TYPE_DISK | ffi::VDEV_TYPE_FILE) => {
            VdevKind::LogVdev
        }
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

    // Read the raw path once. Leaves get both a stripped `name` (for
    // compact display) and the full `device_path` (for the v0.3.1 tree
    // view's wide-layout DEVICE_PATH column). Interior vdevs and group
    // headers get only `name` from `type_str` and `device_path = None`.
    let (name, device_path) = match kind {
        VdevKind::Disk
        | VdevKind::File
        | VdevKind::LogVdev
        | VdevKind::CacheVdev
        | VdevKind::SpareVdev => {
            match nvlist_get_string(lz, nvl, ffi::ZPOOL_CONFIG_PATH) {
                Ok(full) => {
                    let stripped = full
                        .strip_prefix("/dev/")
                        .unwrap_or(&full)
                        .to_string();
                    (stripped, Some(full))
                }
                Err(_) => (type_str.clone(), None),
            }
        }
        _ => (type_str.clone(), None),
    };

    let (state, errors) = read_vdev_stats(lz, nvl);

    let size_bytes = match kind {
        VdevKind::LogGroup | VdevKind::CacheGroup | VdevKind::SpareGroup => None,
        _ => read_vdev_stats_raw(lz, nvl)
            .and_then(|stats| stats.get(ffi::VS_IDX_SPACE).copied()),
    };

    // Recurse on children. Group-context for leaves propagates down.
    let child_group = match kind {
        VdevKind::LogGroup | VdevKind::CacheGroup | VdevKind::SpareGroup => Some(kind),
        _ => parent_group,
    };

    let mut children = Vec::new();
    if let Ok((child_nvls, count)) =
        nvlist_get_nvlist_array(lz, nvl, ffi::ZPOOL_CONFIG_CHILDREN)
    {
        for i in 0..count {
            // SAFETY: same as walk_root_vdev's recursion.
            let child_nvl = unsafe { *child_nvls.add(i) };
            if child_nvl.is_null() {
                continue;
            }
            match walk_child_vdev(lz, child_nvl, child_group) {
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
        device_path,
    })
}

/// Read `vdev_stats` uint64 array and extract (state, errors). Returns
/// `(Online, zeros)` when the array is missing or shorter than we expect.
fn read_vdev_stats(lz: &Libzfs, nvl: *mut ffi::nvlist_t) -> (VdevState, ErrorCounts) {
    let Some(stats) = read_vdev_stats_raw(lz, nvl) else {
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
fn read_vdev_stats_raw(lz: &Libzfs, nvl: *mut ffi::nvlist_t) -> Option<Vec<u64>> {
    let mut out_ptr: *mut u64 = ptr::null_mut();
    let mut nelem: c_uint = 0;
    // SAFETY: nvl is a valid nvlist_t from libzfs. The key C-string is
    // nul-terminated (static const). The out-pointers are writable and
    // only read on rc==0.
    let rc = unsafe {
        (lz.nvlist_lookup_uint64_array)(
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

/// Read `scan_stats` uint64 array from a vdev nvlist and decode into a
/// `ScrubState`. Thin FFI wrapper — the actual math lives in
/// [`decode_scan_state`] so it can be unit-tested without libzfs.
fn read_scan_state(lz: &Libzfs, nvl: *mut ffi::nvlist_t) -> ScrubState {
    let mut out_ptr: *mut u64 = ptr::null_mut();
    let mut nelem: c_uint = 0;
    // SAFETY: same as read_vdev_stats_raw.
    let rc = unsafe {
        (lz.nvlist_lookup_uint64_array)(
            nvl,
            ffi::ZPOOL_CONFIG_SCAN_STATS.as_ptr(),
            &mut out_ptr,
            &mut nelem,
        )
    };
    if rc != 0 || out_ptr.is_null() {
        return ScrubState::Never;
    }
    // SAFETY: nelem is the element count libzfs wrote; backing memory is
    // owned by the parent nvlist and only borrowed for this call.
    let stats = unsafe { std::slice::from_raw_parts(out_ptr, nelem as usize) };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    decode_scan_state(stats, now)
}

/// Decode a `pool_scan_stat_t` u64 array into a `ScrubState`. Pure,
/// deterministic function — all libzfs I/O happens in the caller. `now_secs`
/// is the current UNIX time, passed in so tests can drive the speed/ETA
/// calculations with a known clock.
///
/// Matches the math in OpenZFS `cmd/zpool/zpool_main.c` — specifically
/// `print_scan_scrub_resilver_status` — so our "% complete" and "X.X B/s"
/// numbers line up with what `zpool status` prints for the same pool.
///
/// On OpenZFS 0.8+ (array length >= [`ffi::PSS_MIN_LEN_WITH_ISSUED`]) we
/// use the sequential-scrub-aware formula:
///
/// ```text
/// progress_pct     = pss_issued / (pss_to_examine - pss_skipped)
/// rate             = pss_pass_issued / pass_elapsed
/// eta_seconds      = (total_i - pss_issued) / rate
/// pass_elapsed     = now - pss_pass_start - pss_pass_scrub_spent_paused
/// ```
///
/// On pre-0.8 (length 9..15) we fall back to the legacy
/// `pss_examined / pss_to_examine` math, which was correct back when the
/// scanner read every byte in a single pass. In practice the fallback is
/// only reached on extremely old systems — every libzfs soname in our
/// dlopen list (`libzfs.so.2` onward) ships with sequential scrub.
fn decode_scan_state(stats: &[u64], now_secs: u64) -> ScrubState {
    if stats.len() < ffi::PSS_MIN_LEN {
        return ScrubState::Never;
    }
    let func = stats[ffi::PSS_IDX_FUNC];
    let state = stats[ffi::PSS_IDX_STATE];

    match state {
        ffi::DSS_NONE => ScrubState::Never,
        ffi::DSS_SCANNING => {
            let to_examine = stats[ffi::PSS_IDX_TO_EXAMINE];
            let examined = stats[ffi::PSS_IDX_EXAMINED];
            let has_issued = stats.len() >= ffi::PSS_MIN_LEN_WITH_ISSUED;

            // Progress numerator/denominator: modern (issued vs. total_i)
            // if the pass fields are present, otherwise legacy (examined
            // vs. to_examine).
            let (numerator, denominator) = if has_issued {
                let issued = stats[ffi::PSS_IDX_ISSUED];
                let skipped = stats[ffi::PSS_IDX_SKIPPED];
                let total_i = to_examine.saturating_sub(skipped);
                (issued, total_i)
            } else {
                (examined, to_examine)
            };
            let progress_pct = if denominator == 0 {
                0
            } else {
                ((numerator.saturating_mul(100)) / denominator).min(100) as u8
            };

            // Speed/ETA: same modern/legacy split. The modern path mirrors
            // zpool_main.c's `pass_elapsed = MAX(1, now - pass_start -
            // pass_scrub_spent_paused)`.
            let (speed_bytes_per_sec, eta_seconds) = if has_issued {
                let issued = stats[ffi::PSS_IDX_ISSUED];
                let skipped = stats[ffi::PSS_IDX_SKIPPED];
                let pass_start = stats[ffi::PSS_IDX_PASS_START];
                let pass_paused = stats[ffi::PSS_IDX_PASS_SCRUB_SPENT_PAUSED];
                let pass_issued = stats[ffi::PSS_IDX_PASS_ISSUED];
                let total_i = to_examine.saturating_sub(skipped);

                let pass_elapsed = now_secs
                    .saturating_sub(pass_start)
                    .saturating_sub(pass_paused)
                    .max(1);
                let rate = pass_issued / pass_elapsed;
                let speed = if rate > 0 { Some(rate) } else { None };
                let eta = if rate > 0 && total_i > issued {
                    Some((total_i - issued) / rate)
                } else {
                    None
                };
                (speed, eta)
            } else {
                let start_time = stats[ffi::PSS_IDX_START_TIME];
                let elapsed = now_secs.saturating_sub(start_time);
                let speed = if elapsed > 0 && examined > 0 {
                    Some(examined / elapsed)
                } else {
                    None
                };
                let eta = speed.and_then(|rate| {
                    if rate > 0 && to_examine > examined {
                        Some((to_examine - examined) / rate)
                    } else {
                        None
                    }
                });
                (speed, eta)
            };

            ScrubState::InProgress {
                progress_pct,
                eta_seconds,
                speed_bytes_per_sec,
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
// These wrap the raw function-pointer calls in something callable from safe
// code, copying strings out into owned `String`s where appropriate and
// returning `anyhow::Error` on libzfs-reported failures.
// ---------------------------------------------------------------------------

fn nvlist_get_string(
    lz: &Libzfs,
    nvl: *const ffi::nvlist_t,
    key: &CStr,
) -> Result<String> {
    let mut out: *const std::ffi::c_char = ptr::null();
    // SAFETY: nvl is a valid nvlist_t borrowed from libzfs; the key is
    // nul-terminated (CStr contract); out is a writable slot only read
    // on rc == 0.
    let rc = unsafe { (lz.nvlist_lookup_string)(nvl, key.as_ptr(), &mut out) };
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
fn nvlist_get_uint64(lz: &Libzfs, nvl: *const ffi::nvlist_t, key: &CStr) -> Result<u64> {
    let mut out: u64 = 0;
    // SAFETY: nvl + key are valid; out is a writable stack slot.
    let rc = unsafe { (lz.nvlist_lookup_uint64)(nvl, key.as_ptr(), &mut out) };
    if rc != 0 {
        return Err(anyhow!(
            "nvlist_lookup_uint64({}) failed: rc={rc}",
            key.to_string_lossy()
        ));
    }
    Ok(out)
}

fn nvlist_get_nvlist(
    lz: &Libzfs,
    nvl: *mut ffi::nvlist_t,
    key: &CStr,
) -> Result<*mut ffi::nvlist_t> {
    let mut out: *mut ffi::nvlist_t = ptr::null_mut();
    // SAFETY: nvl + key are valid; out is a writable stack slot.
    let rc = unsafe { (lz.nvlist_lookup_nvlist)(nvl, key.as_ptr(), &mut out) };
    if rc != 0 || out.is_null() {
        return Err(anyhow!(
            "nvlist_lookup_nvlist({}) failed: rc={rc}",
            key.to_string_lossy()
        ));
    }
    Ok(out)
}

fn nvlist_get_nvlist_array(
    lz: &Libzfs,
    nvl: *mut ffi::nvlist_t,
    key: &CStr,
) -> Result<(*mut *mut ffi::nvlist_t, usize)> {
    let mut out_ptr: *mut *mut ffi::nvlist_t = ptr::null_mut();
    let mut nelem: c_uint = 0;
    // SAFETY: nvl + key are valid; out-slots are writable.
    let rc = unsafe {
        (lz.nvlist_lookup_nvlist_array)(nvl, key.as_ptr(), &mut out_ptr, &mut nelem)
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

    // ---- decode_scan_state --------------------------------------------------
    //
    // Pure-function tests for the scan-stats decoder. Hand-build a
    // pool_scan_stat_t u64 array of the right length, pass it through with a
    // synthetic "now" clock, and assert on the resulting `ScrubState`. No
    // libzfs involvement, no `#[ignore]`.

    const GIB: u64 = 1 << 30;

    /// Build a 15-element (OpenZFS 0.8+) scan_stats array with the fields
    /// we set by name; unset indices stay zero. Keeps the tests below from
    /// drowning in `stats[N] = ...` noise.
    fn build_modern_scan_stats(
        func: u64,
        state: u64,
        start_time: u64,
        to_examine: u64,
        examined: u64,
        skipped: u64,
        pass_start: u64,
        pass_paused: u64,
        pass_issued: u64,
        issued: u64,
    ) -> Vec<u64> {
        let mut stats = vec![0u64; ffi::PSS_MIN_LEN_WITH_ISSUED];
        stats[ffi::PSS_IDX_FUNC] = func;
        stats[ffi::PSS_IDX_STATE] = state;
        stats[ffi::PSS_IDX_START_TIME] = start_time;
        stats[ffi::PSS_IDX_TO_EXAMINE] = to_examine;
        stats[ffi::PSS_IDX_EXAMINED] = examined;
        stats[ffi::PSS_IDX_SKIPPED] = skipped;
        stats[ffi::PSS_IDX_PASS_START] = pass_start;
        stats[ffi::PSS_IDX_PASS_SCRUB_SPENT_PAUSED] = pass_paused;
        stats[ffi::PSS_IDX_PASS_ISSUED] = pass_issued;
        stats[ffi::PSS_IDX_ISSUED] = issued;
        stats
    }

    /// Regression test for the exact user-reported bug: zpool status said
    /// "15.39% done" but the zftop pools tab showed "scrub 100%". The cause
    /// was decoding `pss_examined / pss_to_examine` instead of the
    /// sequential-scrub-aware `pss_issued / (pss_to_examine - pss_skipped)`.
    ///
    /// Numbers mirror the real rpool snapshot the user pasted:
    ///   228G / 227G scanned, 34.9G / 227G issued at 1.25G/s, 15.39% done
    #[test]
    fn decode_scan_state_matches_zpool_status_after_metadata_walk() {
        // 228 GiB examined vs. 227 GiB to_examine — the buggy calc pins at 100%.
        // 34.9 GiB issued / 227 GiB denominator = 15.37% (rounds to "15").
        // Pass elapsed ~28s so pass_issued / elapsed ≈ 1.25 GiB/s.
        let scan_start = 1_000_000;
        let pass_issued_bytes = 34_900_000_000; // 34.9 GB in bytes
        let issued_bytes = 34_900_000_000;
        let to_examine = 227 * GIB;
        let examined = 228 * GIB;

        let stats = build_modern_scan_stats(
            ffi::POOL_SCAN_SCRUB,
            ffi::DSS_SCANNING,
            scan_start,
            to_examine,
            examined,
            0, // skipped
            scan_start,
            0, // pass_paused
            pass_issued_bytes,
            issued_bytes,
        );
        let now = scan_start + 28;

        match decode_scan_state(&stats, now) {
            ScrubState::InProgress {
                progress_pct,
                eta_seconds,
                speed_bytes_per_sec,
                is_resilver,
            } => {
                assert!(!is_resilver);
                // 34.9e9 / (227 * 2^30) = 0.1433... ≈ 14% (int truncation
                // from integer math, matches what zpool shows modulo
                // rounding). The important thing is that it's nowhere
                // near 100%.
                assert!(
                    (13..=16).contains(&progress_pct),
                    "expected ~15% progress, got {progress_pct}% \
                     — did we regress back to examined/to_examine?"
                );

                // ~1.25 GiB/s. The pass_issued is in decimal bytes but
                // the GIB constant is binary, so the printed number in
                // the UI is ≈ 1.16 GiB/s. Allow a generous window —
                // anything within an order of magnitude of 1 GiB/s
                // proves we're using pass_issued/pass_elapsed.
                let rate = speed_bytes_per_sec.expect("speed should be Some");
                assert!(
                    rate > 500 * (1 << 20) && rate < 3 * GIB,
                    "expected ~1 GiB/s, got {rate} B/s"
                );

                // ETA should be roughly (227 - 34.9) GB / 1.25 GB/s ≈ 150s.
                let eta = eta_seconds.expect("eta should be Some");
                assert!(
                    (60..=600).contains(&eta),
                    "expected eta roughly 2-10 min, got {eta}s"
                );
            }
            other => panic!("expected InProgress, got {other:?}"),
        }
    }

    /// When `pss_pass_issued == 0` (e.g. the first second of a scrub while
    /// the metadata walk is still building the issue queue), the rate is 0
    /// and we should report `speed = None` / `eta = None` rather than
    /// divide-by-zero. Progress should be 0, matching zpool status's
    /// "0.00% done".
    #[test]
    fn decode_scan_state_just_started_scrub_with_no_issued_yet() {
        let start = 2_000_000;
        let stats = build_modern_scan_stats(
            ffi::POOL_SCAN_SCRUB,
            ffi::DSS_SCANNING,
            start,
            500 * GIB, // to_examine
            1 * GIB,   // examined — metadata walk just starting
            0,
            start,
            0,
            0, // pass_issued = 0: no I/O yet
            0, // issued = 0
        );

        match decode_scan_state(&stats, start + 2) {
            ScrubState::InProgress {
                progress_pct,
                eta_seconds,
                speed_bytes_per_sec,
                ..
            } => {
                assert_eq!(progress_pct, 0);
                assert!(speed_bytes_per_sec.is_none());
                assert!(eta_seconds.is_none());
            }
            other => panic!("expected InProgress, got {other:?}"),
        }
    }

    /// Resilver on a modern pool — same math as scrub, but
    /// `is_resilver` should flip to `true` so the UI picks the right label.
    #[test]
    fn decode_scan_state_modern_resilver_sets_flag() {
        let start = 3_000_000;
        let stats = build_modern_scan_stats(
            ffi::POOL_SCAN_RESILVER,
            ffi::DSS_SCANNING,
            start,
            100 * GIB,
            100 * GIB, // metadata walk complete
            0,
            start,
            0,
            25 * GIB, // pass_issued: 25 GiB in 25s → ~1 GiB/s
            25 * GIB,
        );
        match decode_scan_state(&stats, start + 25) {
            ScrubState::InProgress {
                progress_pct,
                is_resilver,
                ..
            } => {
                assert!(is_resilver);
                // 25 / 100 = 25%
                assert_eq!(progress_pct, 25);
            }
            other => panic!("expected InProgress, got {other:?}"),
        }
    }

    /// Legacy scan_stats array (pre-0.8, only the 9 on-disk fields). We
    /// have no `pss_issued` to read, so fall back to `examined/to_examine`
    /// and to the `elapsed = now - start_time` speed formula. This path is
    /// essentially dead on any currently-supported OpenZFS but we still
    /// want it to produce sane numbers rather than panic.
    #[test]
    fn decode_scan_state_legacy_array_uses_examined_fallback() {
        let start = 4_000_000;
        let mut stats = vec![0u64; ffi::PSS_MIN_LEN]; // only 9 elements
        stats[ffi::PSS_IDX_FUNC] = ffi::POOL_SCAN_SCRUB;
        stats[ffi::PSS_IDX_STATE] = ffi::DSS_SCANNING;
        stats[ffi::PSS_IDX_START_TIME] = start;
        stats[ffi::PSS_IDX_TO_EXAMINE] = 100 * GIB;
        stats[ffi::PSS_IDX_EXAMINED] = 50 * GIB; // halfway done under old semantics

        match decode_scan_state(&stats, start + 50) {
            ScrubState::InProgress {
                progress_pct,
                speed_bytes_per_sec,
                ..
            } => {
                assert_eq!(progress_pct, 50);
                // 50 GiB / 50s = 1 GiB/s
                let rate = speed_bytes_per_sec.expect("legacy path should compute speed");
                assert!(rate > 0);
            }
            other => panic!("expected InProgress, got {other:?}"),
        }
    }

    /// Finished scrub decodes into `ScrubState::Finished` with the errors
    /// count and completion time from the array.
    #[test]
    fn decode_scan_state_finished_carries_errors_and_end_time() {
        let mut stats = vec![0u64; ffi::PSS_MIN_LEN];
        stats[ffi::PSS_IDX_FUNC] = ffi::POOL_SCAN_SCRUB;
        stats[ffi::PSS_IDX_STATE] = ffi::DSS_FINISHED;
        stats[ffi::PSS_IDX_END_TIME] = 1_700_000_000;
        stats[ffi::PSS_IDX_ERRORS] = 7;

        match decode_scan_state(&stats, 1_700_500_000) {
            ScrubState::Finished {
                errors_repaired,
                completed_at,
            } => {
                assert_eq!(errors_repaired, 7);
                let secs = completed_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                assert_eq!(secs, 1_700_000_000);
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    /// `DSS_NONE` (never scrubbed) and a too-short array both decode to
    /// `ScrubState::Never`.
    #[test]
    fn decode_scan_state_never_and_empty() {
        let mut stats = vec![0u64; ffi::PSS_MIN_LEN];
        stats[ffi::PSS_IDX_STATE] = ffi::DSS_NONE;
        assert!(matches!(decode_scan_state(&stats, 0), ScrubState::Never));

        // Array shorter than PSS_MIN_LEN -> Never (defensive guard).
        let too_short = vec![0u64; 3];
        assert!(matches!(
            decode_scan_state(&too_short, 0),
            ScrubState::Never
        ));
    }

    /// Canceled scrub maps to `ScrubState::Error`.
    #[test]
    fn decode_scan_state_canceled_maps_to_error() {
        let mut stats = vec![0u64; ffi::PSS_MIN_LEN];
        stats[ffi::PSS_IDX_FUNC] = ffi::POOL_SCAN_SCRUB;
        stats[ffi::PSS_IDX_STATE] = ffi::DSS_CANCELED;
        assert!(matches!(decode_scan_state(&stats, 0), ScrubState::Error));
    }

    // ---- live libzfs smoke tests --------------------------------------------

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
    /// is cfg-gated out so dev hosts without loaded ZFS skip it silently.
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
