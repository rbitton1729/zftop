// FreeBSD source: read vm.stats.vm.* and hw.physmem via the sysctl(3) interface.
//
// FreeBSD reports memory in page counts (multiplied by hw.pagesize at display
// time) plus a single hw.physmem byte total. The memory model is genuinely
// different from Linux's MemFree/Buffers/Cached split, so the snapshot uses
// FreeBSD-native segments: Wired-minus-ARC / ARC / Active / Inactive+Laundry.
//
// At runtime (FreeBSD only) the `sysctl` crate is used. For tests on the dev
// host (Linux), `parse_sysctl_text` consumes captured `sysctl(8)` output from
// `fixtures/bsd/{vm-stats,hw-mem}.freebsd.txt`.

use anyhow::Result;
#[cfg(target_os = "freebsd")]
use anyhow::Context;
#[cfg(any(test, not(target_os = "freebsd")))]
use anyhow::anyhow;
use ratatui::style::Color;
#[cfg(test)]
use std::collections::HashMap;

use super::{MemSnapshot, MemSource, RamSegment};

#[derive(Debug, Clone)]
pub struct FreeBsdMemInfo {
    pub page_size: u64,
    pub total_bytes: u64,
    pub wired_pages: u64,
    pub active_pages: u64,
    pub inactive_pages: u64,
    pub laundry_pages: u64,
    #[allow(dead_code)] // parsed from sysctl as a sanity check; the bar's free area is computed implicitly
    pub free_pages: u64,
}

impl FreeBsdMemInfo {
    /// Read all required sysctls in one shot.
    #[cfg(target_os = "freebsd")]
    pub fn from_sysctl() -> Result<Self> {
        Ok(Self {
            page_size: read_sysctl_u64("hw.pagesize")?,
            total_bytes: read_sysctl_u64("hw.physmem")?,
            wired_pages: read_sysctl_u64("vm.stats.vm.v_wire_count")?,
            active_pages: read_sysctl_u64("vm.stats.vm.v_active_count")?,
            inactive_pages: read_sysctl_u64("vm.stats.vm.v_inactive_count")?,
            laundry_pages: read_sysctl_u64("vm.stats.vm.v_laundry_count")?,
            free_pages: read_sysctl_u64("vm.stats.vm.v_free_count")?,
        })
    }

    /// Cross-platform constructor used by tests. Takes the captured text
    /// output of `sysctl vm.stats.vm` and `sysctl hw.physmem hw.realmem hw.pagesize`.
    #[cfg(test)]
    pub fn parse_sysctl_text(vm_text: &str, hw_text: &str) -> Result<Self> {
        let vm = parse_to_map(vm_text);
        let hw = parse_to_map(hw_text);
        let get = |map: &HashMap<String, u64>, key: &str| -> Result<u64> {
            map.get(key)
                .copied()
                .ok_or_else(|| anyhow!("missing sysctl '{key}'"))
        };
        Ok(Self {
            page_size: get(&hw, "hw.pagesize")?,
            total_bytes: get(&hw, "hw.physmem")?,
            wired_pages: get(&vm, "vm.stats.vm.v_wire_count")?,
            active_pages: get(&vm, "vm.stats.vm.v_active_count")?,
            inactive_pages: get(&vm, "vm.stats.vm.v_inactive_count")?,
            laundry_pages: get(&vm, "vm.stats.vm.v_laundry_count")?,
            free_pages: get(&vm, "vm.stats.vm.v_free_count")?,
        })
    }
}

#[cfg(target_os = "freebsd")]
fn read_sysctl_u64(key: &str) -> Result<u64> {
    use sysctl::Sysctl;
    let ctl = sysctl::Ctl::new(key)
        .with_context(|| format!("failed to open sysctl {key}"))?;
    let s = ctl
        .value_string()
        .with_context(|| format!("failed to read sysctl {key}"))?;
    s.trim()
        .parse::<u64>()
        .with_context(|| format!("sysctl {key} returned non-numeric value: {s:?}"))
}

#[cfg(test)]
fn parse_to_map(content: &str) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Ok(parsed) = value.trim().parse::<u64>() else {
            continue;
        };
        map.insert(key.trim().to_string(), parsed);
    }
    map
}

/// `MemSource` impl backed by `FreeBsdMemInfo`.
pub struct FreeBsdMemSource {
    last: Option<FreeBsdMemInfo>,
}

impl FreeBsdMemSource {
    #[cfg(target_os = "freebsd")]
    pub fn new() -> Result<Self> {
        Ok(Self {
            last: Some(FreeBsdMemInfo::from_sysctl()?),
        })
    }
}

impl MemSource for FreeBsdMemSource {
    fn refresh(&mut self) -> Result<()> {
        #[cfg(target_os = "freebsd")]
        {
            self.last = Some(FreeBsdMemInfo::from_sysctl()?);
            Ok(())
        }
        #[cfg(not(target_os = "freebsd"))]
        {
            // Stub: this code path is unreachable in practice on non-FreeBSD,
            // because main.rs constructs LinuxMemSource on Linux. The trait
            // impl exists so the type is usable in tests across platforms.
            Err(anyhow!("FreeBsdMemSource::refresh called on non-FreeBSD"))
        }
    }

    fn snapshot(&self, arc_segments: &[RamSegment]) -> Option<MemSnapshot> {
        let m = self.last.as_ref()?;
        if m.total_bytes == 0 || m.page_size == 0 {
            return None;
        }
        let arc_total: u64 = arc_segments.iter().map(|s| s.bytes).sum();
        let wired_bytes = m.wired_pages * m.page_size;
        let active_bytes = m.active_pages * m.page_size;
        let inactive_bytes = (m.inactive_pages + m.laundry_pages) * m.page_size;

        let mut segments = Vec::with_capacity(3 + arc_segments.len());
        segments.push(RamSegment {
            label: "Wired",
            color: Color::Cyan,
            bytes: wired_bytes.saturating_sub(arc_total),
        });
        segments.extend(arc_segments.iter().cloned());
        segments.push(RamSegment {
            label: "Active",
            color: Color::Green,
            bytes: active_bytes,
        });
        segments.push(RamSegment {
            label: "Inactive",
            color: Color::Yellow,
            bytes: inactive_bytes,
        });

        Some(MemSnapshot {
            total_bytes: m.total_bytes,
            segments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> FreeBsdMemInfo {
        let vm = std::fs::read_to_string("fixtures/bsd/vm-stats.freebsd.txt").unwrap();
        let hw = std::fs::read_to_string("fixtures/bsd/hw-mem.freebsd.txt").unwrap();
        FreeBsdMemInfo::parse_sysctl_text(&vm, &hw).unwrap()
    }

    #[test]
    fn parse_fixture_pagesize_and_total() {
        let m = fixture();
        assert_eq!(m.page_size, 4096);
        assert_eq!(m.total_bytes, 4_250_365_952);
    }

    #[test]
    fn parse_fixture_page_counts() {
        let m = fixture();
        assert_eq!(m.wired_pages, 440_983);
        assert_eq!(m.active_pages, 81);
        assert_eq!(m.inactive_pages, 11_358);
        assert_eq!(m.laundry_pages, 4_002);
        assert_eq!(m.free_pages, 26_766);
    }

    #[test]
    fn snapshot_segments_match_fixture() {
        let m = fixture();
        let src = FreeBsdMemSource { last: Some(m.clone()) };
        // Two ARC sub-segments: size + overhead. Sum = 1_472_594_864 (matches
        // arcstats fixture's `size` for back-compat with wired-minus-arc math).
        let arc_size: u64 = 1_400_000_000;
        let arc_ovh: u64 = 72_594_864;
        let arc_segs = vec![
            RamSegment { label: "ARC", color: Color::Magenta, bytes: arc_size },
            RamSegment { label: "ARC ovh", color: Color::Indexed(53), bytes: arc_ovh },
        ];

        let snap = src.snapshot(&arc_segs).unwrap();
        assert_eq!(snap.total_bytes, 4_250_365_952);
        // Wired + 2 ARC segs + Active + Inactive = 5
        assert_eq!(snap.segments.len(), 5);

        // Wired - (arc_size + arc_ovh)
        let wired_bytes = m.wired_pages * m.page_size;
        assert_eq!(snap.segments[0].label, "Wired");
        assert_eq!(snap.segments[0].bytes, wired_bytes - (arc_size + arc_ovh));

        // ARC sub-segments preserved verbatim
        assert_eq!(snap.segments[1].label, "ARC");
        assert_eq!(snap.segments[1].bytes, arc_size);
        assert_eq!(snap.segments[1].color, Color::Magenta);
        assert_eq!(snap.segments[2].label, "ARC ovh");
        assert_eq!(snap.segments[2].bytes, arc_ovh);
        assert_eq!(snap.segments[2].color, Color::Indexed(53));

        // Active
        assert_eq!(snap.segments[3].label, "Active");
        assert_eq!(snap.segments[3].bytes, 81 * 4096);

        // Inactive + Laundry
        assert_eq!(snap.segments[4].label, "Inactive");
        assert_eq!(snap.segments[4].bytes, (11_358 + 4_002) * 4096);
    }

    #[test]
    fn snapshot_underflow_saturates() {
        // Edge case: ARC larger than wired (race during sampling).
        let m = FreeBsdMemInfo {
            page_size: 4096,
            total_bytes: 4_000_000_000,
            wired_pages: 100,
            active_pages: 50,
            inactive_pages: 50,
            laundry_pages: 0,
            free_pages: 100,
        };
        let src = FreeBsdMemSource { last: Some(m) };
        let arc_segs = vec![
            RamSegment { label: "ARC", color: Color::Magenta, bytes: 999_999_999 },
        ];
        let snap = src.snapshot(&arc_segs).unwrap();
        assert_eq!(snap.segments[0].bytes, 0); // saturated, didn't underflow
    }
}
