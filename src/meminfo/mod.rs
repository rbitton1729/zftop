// System memory abstraction.
//
// The data shape genuinely differs between Linux and FreeBSD, so we expose a
// `MemSource` trait whose implementations build a uniform `MemSnapshot` for
// the UI to render. The UI never needs to know which OS produced the snapshot.

use anyhow::Result;
use ratatui::style::Color;

#[cfg(any(test, target_os = "linux"))]
pub mod linux;
#[cfg(any(test, target_os = "freebsd"))]
pub mod freebsd;

/// One coloured segment of the system RAM bar.
#[derive(Debug, Clone)]
pub struct RamSegment {
    pub label: &'static str,
    pub color: Color,
    pub bytes: u64,
}

/// A point-in-time snapshot of memory state, ready for the UI to render.
///
/// `total_bytes` is the bar's denominator. `segments` are drawn in order;
/// any space not covered by segments is left empty (interpreted as "free").
#[derive(Debug, Clone)]
pub struct MemSnapshot {
    pub total_bytes: u64,
    pub segments: Vec<RamSegment>,
}

/// Pluggable source of system memory state.
///
/// Implementations are constructed once at startup and `refresh()`-ed each
/// tick. `snapshot(arc_segments)` then composes a `MemSnapshot` using the
/// last-fetched data plus pre-built ARC segments supplied by the caller
/// (the ARC is sourced separately from `ArcStats` because it's part of the
/// wired/anonymous accounting). Callers pass one or more `RamSegment`s so
/// they can split ARC into sub-segments (e.g. `size` vs `overhead_size`)
/// without `meminfo` needing to know what counts as ARC.
pub trait MemSource {
    fn refresh(&mut self) -> Result<()>;
    fn snapshot(&self, arc_segments: &[RamSegment]) -> Option<MemSnapshot>;
}
