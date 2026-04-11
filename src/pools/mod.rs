//! Pool observability layer. Wraps libzfs (hand-rolled FFI) and exposes a
//! `PoolsSource` trait plus plain-Rust domain types so the rest of the crate
//! can consume pool data without touching `unsafe` or libzfs nvlists.
//!
//! Module layout:
//! - `types`   — `PoolInfo`, `VdevNode`, `ScrubState`, etc. Zero libzfs dep.
//! - `ffi`     — `extern "C"` signatures for libzfs (Task 6).
//! - `libzfs`  — `LibzfsPoolsSource` calling `ffi` and building `types` (Task 7).
//! - `fake`    — test-only `FakePoolsSource` used by unit tests.

pub mod types;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod libzfs;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod ffi;

#[cfg(test)]
pub mod fake;

// Some of these re-exports are only reached by `#[cfg(test)]` code in
// sibling modules right now (tests construct VdevNode etc. via the
// `crate::pools::*` path). Task 11 brings the first production caller.
#[allow(unused_imports)]
pub use self::types::{
    ErrorCounts, PoolHealth, PoolInfo, ScrubState, VdevKind, VdevNode, VdevState,
};

/// Source of pool observability data. Real implementation wraps libzfs.
pub trait PoolsSource {
    /// Refresh internal state from the underlying data source. Called on
    /// every app refresh tick. Errors are non-fatal — `App` keeps the last
    /// successful snapshot and surfaces the error string in the UI.
    fn refresh(&mut self) -> anyhow::Result<()>;

    /// Current pool snapshot. The returned `Vec` is owned; callers may
    /// sort / filter it freely.
    fn pools(&self) -> Vec<PoolInfo>;
}
