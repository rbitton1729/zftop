//! Dataset observability layer. Mirrors `src/pools/` in shape: a
//! `DatasetsSource` trait, plain-Rust domain types in `types`, a libzfs-
//! backed implementation that reuses the extended `pools::ffi::Libzfs`
//! loader, and a test-only fake. The `unsafe` lives only in
//! `libzfs.rs` and only when calling into `pools::ffi`.

pub mod types;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod libzfs;

#[cfg(test)]
pub mod fake;

#[allow(unused_imports)]
pub use self::types::{DatasetKind, DatasetNode, DatasetProperties};

/// Source of dataset observability data. Real implementation wraps libzfs.
pub trait DatasetsSource {
    /// Refresh internal state from the underlying data source. Called on
    /// every app refresh tick. Errors are non-fatal — `App` keeps the
    /// last successful snapshot and surfaces the error string in the UI.
    fn refresh(&mut self) -> anyhow::Result<()>;

    /// One root per imported pool. Each root's `children` reflects the
    /// nested filesystem/volume hierarchy. Empty `Vec` when no pools are
    /// imported.
    fn roots(&self) -> Vec<DatasetNode>;
}
