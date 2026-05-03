//! `LibzfsDatasetsSource` — the libzfs-backed implementation of
//! `DatasetsSource`. Reuses the extended `pools::ffi::Libzfs` loader by
//! calling `Libzfs::load()` independently — each `Libzfs*Source` owns
//! its own `libzfs_handle_t`. dlopen ref-counts the shared library so
//! the `.so` mapping is shared at the OS level.

use anyhow::{anyhow, Context, Result};
use std::ffi::{c_int, c_void, CStr};
use std::ptr;
use std::time::{Duration, UNIX_EPOCH};

use crate::datasets::types::{DatasetKind, DatasetNode, DatasetProperties};
use crate::datasets::DatasetsSource;
use crate::pools::ffi::{self, Libzfs};

pub struct LibzfsDatasetsSource {
    lz: Libzfs,
    handle: *mut ffi::libzfs_handle_t,
    snapshot: Vec<DatasetNode>,
}

// SAFETY: same constraint as LibzfsPoolsSource — single-threaded use
// inside App. NOT Sync.
unsafe impl Send for LibzfsDatasetsSource {}

impl LibzfsDatasetsSource {
    pub fn new() -> Result<Self> {
        let lz = Libzfs::load().context("failed to load libzfs for datasets")?;
        // SAFETY: dlopen+dlsym succeeded, libzfs_init takes no args and
        // returns either a valid handle or null.
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
}

impl Drop for LibzfsDatasetsSource {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: handle is non-null and was returned by a successful
            // libzfs_init in this Source's lifetime.
            unsafe { (self.lz.libzfs_fini)(self.handle) };
            self.handle = ptr::null_mut();
        }
    }
}

impl DatasetsSource for LibzfsDatasetsSource {
    fn refresh(&mut self) -> Result<()> {
        // Get pool names by iterating zpools. Collect names then open
        // each as a dataset via zfs_open.
        let mut pool_names: Vec<String> = Vec::new();
        let mut handles: Vec<*mut ffi::zpool_handle_t> = Vec::new();
        // SAFETY: same as LibzfsPoolsSource::refresh's zpool_iter call.
        let rc = unsafe {
            (self.lz.zpool_iter)(
                self.handle,
                collect_zpool_handle,
                &mut handles as *mut _ as *mut c_void,
            )
        };
        if rc != 0 {
            return Err(anyhow!("zpool_iter rc={rc}"));
        }
        for zhp in handles {
            // SAFETY: zhp valid until zpool_close.
            unsafe {
                let p = (self.lz.zpool_get_name)(zhp);
                if !p.is_null() {
                    pool_names.push(CStr::from_ptr(p).to_string_lossy().into_owned());
                }
                (self.lz.zpool_close)(zhp);
            }
        }

        let mut roots: Vec<DatasetNode> = Vec::with_capacity(pool_names.len());
        for name in &pool_names {
            match build_dataset_tree(&self.lz, self.handle, name) {
                Ok(node) => roots.push(node),
                Err(e) => {
                    eprintln!("zftop: skipping pool's datasets during refresh: {e}");
                }
            }
        }
        self.snapshot = roots;
        Ok(())
    }

    fn roots(&self) -> Vec<DatasetNode> {
        self.snapshot.clone()
    }
}

unsafe extern "C" fn collect_zpool_handle(
    zhp: *mut ffi::zpool_handle_t,
    data: *mut c_void,
) -> c_int {
    // SAFETY: data is &mut Vec<*mut zpool_handle_t> passed in by
    // refresh(). Vec lives for the duration of zpool_iter.
    let vec = unsafe { &mut *(data as *mut Vec<*mut ffi::zpool_handle_t>) };
    vec.push(zhp);
    0
}

fn build_dataset_tree(
    lz: &Libzfs,
    handle: *mut ffi::libzfs_handle_t,
    pool_name: &str,
) -> Result<DatasetNode> {
    let c_name = std::ffi::CString::new(pool_name)
        .map_err(|e| anyhow!("invalid pool name: {e}"))?;
    // SAFETY: handle non-null per Source contract; c_name is nul-
    // terminated; types is a valid bitmask.
    let zhp = unsafe {
        (lz.zfs_open)(
            handle,
            c_name.as_ptr(),
            ffi::ZFS_TYPE_FILESYSTEM | ffi::ZFS_TYPE_VOLUME,
        )
    };
    if zhp.is_null() {
        return Err(anyhow!("zfs_open({pool_name}) returned null"));
    }
    let node = build_dataset_node(lz, zhp);
    // SAFETY: zhp returned from zfs_open.
    unsafe { (lz.zfs_close)(zhp) };
    Ok(node)
}

/// Build a DatasetNode for the given handle and recursively walk its
/// children. Closes child handles as the recursion unwinds (the parent
/// handle is closed by the caller).
fn build_dataset_node(lz: &Libzfs, zhp: *mut ffi::zfs_handle_t) -> DatasetNode {
    // Name. SAFETY: zhp non-null until caller's zfs_close.
    let name = unsafe {
        let p = (lz.zfs_get_name)(zhp);
        CStr::from_ptr(p).to_string_lossy().into_owned()
    };

    // Type bitmask.
    // SAFETY: zhp non-null.
    let raw_type = unsafe { (lz.zfs_get_type)(zhp) };
    let kind = if raw_type & ffi::ZFS_TYPE_VOLUME != 0 {
        DatasetKind::Volume
    } else {
        DatasetKind::Filesystem
    };

    // Numeric properties.
    // SAFETY: zhp non-null; ZFS_PROP_* are valid prop ids.
    let used_bytes = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_USED) };
    let refer_bytes = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_REFERENCED) };
    let available_bytes = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_AVAILABLE) };

    // Compression ratio is stored as percent × 100 (e.g. 142 == 1.42x).
    let compress_raw = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_COMPRESSRATIO) };
    let compression_ratio = if compress_raw == 0 {
        0.0
    } else {
        compress_raw as f64 / 100.0
    };

    let properties = read_dataset_properties(lz, zhp, kind);

    // Recurse on children — only filesystems can have children.
    let mut children: Vec<DatasetNode> = Vec::new();
    if matches!(kind, DatasetKind::Filesystem) {
        let mut child_handles: Vec<*mut ffi::zfs_handle_t> = Vec::new();
        // SAFETY: zhp non-null; collect_zfs_handle has the right signature
        // and stores into the &mut Vec we pass via data.
        let _ = unsafe {
            (lz.zfs_iter_filesystems)(
                zhp,
                collect_zfs_handle,
                &mut child_handles as *mut _ as *mut c_void,
            )
        };
        for child_zhp in child_handles {
            let child_zhp: *mut ffi::zfs_handle_t = child_zhp;
            if child_zhp.is_null() {
                continue;
            }
            let child = build_dataset_node(lz, child_zhp);
            // SAFETY: child_zhp returned by zfs_iter_filesystems.
            unsafe { (lz.zfs_close)(child_zhp) };
            children.push(child);
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
    }

    DatasetNode {
        name,
        kind,
        used_bytes,
        refer_bytes,
        available_bytes,
        compression_ratio,
        properties,
        children,
    }
}

unsafe extern "C" fn collect_zfs_handle(
    zhp: *mut ffi::zfs_handle_t,
    data: *mut c_void,
) -> c_int {
    // SAFETY: data is &mut Vec<*mut zfs_handle_t> passed in by
    // build_dataset_node. Vec outlives this callback.
    let vec = unsafe { &mut *(data as *mut Vec<*mut ffi::zfs_handle_t>) };
    vec.push(zhp);
    0
}

fn read_dataset_properties(
    lz: &Libzfs,
    zhp: *mut ffi::zfs_handle_t,
    kind: DatasetKind,
) -> DatasetProperties {
    let mut p = DatasetProperties::default();

    // Mountpoint — string, only meaningful for filesystems.
    if matches!(kind, DatasetKind::Filesystem) {
        if let Some(s) = read_string_prop(lz, zhp, ffi::ZFS_PROP_MOUNTPOINT) {
            // libzfs returns "none" / "legacy" for unset/legacy.
            if s != "none" && s != "legacy" {
                p.mountpoint = Some(s);
            }
        }
    }

    // Compression algorithm — string ("lz4", "zstd", "off", ...).
    if let Some(s) = read_string_prop(lz, zhp, ffi::ZFS_PROP_COMPRESSION) {
        p.compression_algorithm = Some(s);
    }

    // Recordsize / Volblocksize.
    match kind {
        DatasetKind::Filesystem => {
            // SAFETY: zhp non-null, prop id valid.
            let v = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_RECORDSIZE) };
            if v > 0 {
                p.recordsize_bytes = Some(v);
            }
        }
        DatasetKind::Volume => {
            let v = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_VOLBLOCKSIZE) };
            if v > 0 {
                p.volblocksize_bytes = Some(v);
            }
        }
    }

    // atime/snapdir — only meaningful for filesystems.
    if matches!(kind, DatasetKind::Filesystem) {
        if let Some(s) = read_string_prop(lz, zhp, ffi::ZFS_PROP_ATIME) {
            p.atime_on = Some(s == "on");
        }
        if let Some(s) = read_string_prop(lz, zhp, ffi::ZFS_PROP_SNAPDIR) {
            p.snapdir_visible = Some(s == "visible");
        }
    }

    if let Some(s) = read_string_prop(lz, zhp, ffi::ZFS_PROP_SYNC) {
        p.sync_mode = Some(s);
    }

    // Quotas / reservations — int, 0 means unset.
    let q = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_QUOTA) };
    if q > 0 {
        p.quota_bytes = Some(q);
    }
    let rq = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_REFQUOTA) };
    if rq > 0 {
        p.refquota_bytes = Some(rq);
    }
    let r = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_RESERVATION) };
    if r > 0 {
        p.reservation_bytes = Some(r);
    }
    let rr = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_REFRESERVATION) };
    if rr > 0 {
        p.refreservation_bytes = Some(rr);
    }

    if let Some(s) = read_string_prop(lz, zhp, ffi::ZFS_PROP_DEDUP) {
        p.dedup_on = Some(s != "off");
    }
    let copies = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_COPIES) };
    if copies > 0 {
        p.copies = Some(copies as u8);
    }
    if let Some(s) = read_string_prop(lz, zhp, ffi::ZFS_PROP_ENCRYPTION) {
        p.encryption_algorithm = Some(s);
    }

    let creation = unsafe { (lz.zfs_prop_get_int)(zhp, ffi::ZFS_PROP_CREATION) };
    if creation > 0 {
        p.creation_time = Some(UNIX_EPOCH + Duration::from_secs(creation));
    }

    p
}

/// Read a string property using `zfs_prop_get`. Returns None on failure
/// or when the property source is purely NONE/DEFAULT (treated as "unset"
/// for UI purposes).
fn read_string_prop(
    lz: &Libzfs,
    zhp: *mut ffi::zfs_handle_t,
    prop: c_int,
) -> Option<String> {
    let mut propbuf = [0i8; 1024];
    let mut statbuf = [0i8; 1024];
    let mut src: c_int = 0;
    // SAFETY: zhp non-null; prop id valid; bufs are large enough for
    // every native property's text representation; src is a writable
    // out-slot.
    let rc = unsafe {
        (lz.zfs_prop_get)(
            zhp,
            prop,
            propbuf.as_mut_ptr() as *mut _,
            propbuf.len(),
            &mut src,
            statbuf.as_mut_ptr() as *mut _,
            statbuf.len(),
            0, // literal=0: cooked output
        )
    };
    if rc != 0 {
        return None;
    }
    // Treat NONE+DEFAULT as "unset"; LOCAL/INHERITED/RECEIVED count as set.
    let only_unset = src == ffi::ZPROP_SRC_NONE
        || src == ffi::ZPROP_SRC_DEFAULT
        || src == (ffi::ZPROP_SRC_NONE | ffi::ZPROP_SRC_DEFAULT);
    if only_unset {
        return None;
    }
    // SAFETY: propbuf is nul-terminated by zfs_prop_get on success.
    let s = unsafe { CStr::from_ptr(propbuf.as_ptr() as *const _) }
        .to_string_lossy()
        .into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test — only meaningful on a host with libzfs + /dev/zfs.
    /// Marked `#[ignore]` so regular cargo test runs skip it; run with
    /// `cargo nextest run --run-ignored only` on a live ZFS host.
    #[test]
    #[ignore]
    fn libzfs_init_on_live_host() {
        match LibzfsDatasetsSource::new() {
            Ok(_) => eprintln!("LibzfsDatasetsSource::new succeeded"),
            Err(e) => panic!("LibzfsDatasetsSource::new failed: {e}"),
        }
    }

    #[test]
    #[ignore]
    fn libzfs_refresh_and_dump_on_live_host() {
        let mut src = LibzfsDatasetsSource::new().expect("init");
        src.refresh().expect("refresh");
        for root in src.roots() {
            eprintln!(
                "- {} ({:?}) used={} refer={} compress={:.2}",
                root.name,
                root.kind,
                root.used_bytes,
                root.refer_bytes,
                root.compression_ratio
            );
            for c in &root.children {
                eprintln!("  - {} ({:?})", c.name, c.kind);
            }
        }
    }

    /// Live libzfs integration test for the FreeBSD CI host. Mirrors
    /// `pools/libzfs.rs::tests::libzfs_freebsd_integration`.
    #[cfg(target_os = "freebsd")]
    #[test]
    fn libzfs_freebsd_integration() {
        let mut src = LibzfsDatasetsSource::new()
            .expect("init on bsd-1 should succeed");
        src.refresh().expect("refresh");
        let roots = src.roots();
        assert!(
            !roots.is_empty(),
            "bsd-1 has at least one test pool — refresh returned no roots"
        );
        assert!(
            !roots[0].name.is_empty(),
            "first root should have a non-empty name"
        );
    }
}
