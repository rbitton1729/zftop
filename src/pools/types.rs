//! Plain-Rust domain types representing a snapshot of pool state. No libzfs
//! types leak through this interface — callers upstream (`app.rs`, `ui/*`,
//! tests) see only the shapes defined here. This decoupling is load-bearing
//! for (a) unit-testing the app and UI against `FakePoolsSource`, and (b)
//! the future v1.0 fleet/remote mode where pool data may arrive as
//! serialized `PoolInfo` over SSH rather than from a local libzfs call.

use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct PoolInfo {
    pub name: String,
    pub health: PoolHealth,
    pub allocated_bytes: u64,
    pub size_bytes: u64,
    pub free_bytes: u64,
    /// Integer percentage `0..=100`. `None` if libzfs reports the
    /// "unavailable" sentinel — common on pools that haven't been scrubbed
    /// or that predate fragmentation accounting.
    pub fragmentation_pct: Option<u8>,
    pub scrub: ScrubState,
    /// Sum of read + write + checksum errors across every vdev under this
    /// pool. Populated during nvlist walking by calling
    /// `VdevNode::total_errors()` on the root.
    pub errors: ErrorCounts,
    pub root_vdev: VdevNode,
}

impl PoolInfo {
    /// `0.0..=1.0` share of capacity used. Returns `0.0` if `size_bytes`
    /// is `0` — shouldn't happen on a real pool but defends against
    /// divide-by-zero on a degraded-import pool.
    /// Human-readable RAID label derived from the pool's top-level vdevs.
    /// E.g. "RAIDZ2", "Mirror", "Stripe", "Mixed".
    pub fn raid_label(&self) -> String {
        // Top-level vdevs are the direct children of root, excluding
        // log/cache/spare groups which aren't storage vdevs.
        let tops: Vec<&VdevNode> = self
            .root_vdev
            .children
            .iter()
            .filter(|v| !matches!(v.kind, VdevKind::LogGroup | VdevKind::CacheGroup | VdevKind::SpareGroup))
            .collect();

        if tops.is_empty() {
            return "—".into();
        }

        // Single top-level vdev.
        if tops.len() == 1 {
            return label_for_vdev(&tops[0]);
        }

        // Multiple top-level vdevs — if they're all the same type, use that
        // label (e.g. 3x mirror = "Mirror"). Otherwise "Mixed".
        let first = label_for_vdev(&tops[0]);
        if tops[1..].iter().all(|v| label_for_vdev(v) == first) {
            first
        } else {
            "Mixed".into()
        }
    }

    pub fn capacity_fraction(&self) -> f64 {
        if self.size_bytes == 0 {
            0.0
        } else {
            self.allocated_bytes as f64 / self.size_bytes as f64
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PoolHealth {
    Online,
    Degraded,
    Faulted,
    Offline,
    Removed,
    Unavail,
}

#[derive(Clone, Debug)]
pub enum ScrubState {
    /// Pool has never been scrubbed (or libzfs returned no scan_stats).
    Never,
    /// Scrub or resilver currently running.
    InProgress {
        progress_pct: u8,
        eta_seconds: Option<u64>,
        speed_bytes_per_sec: Option<u64>,
        is_resilver: bool,
    },
    /// Most recent scrub completed successfully.
    Finished {
        completed_at: SystemTime,
        errors_repaired: u64,
    },
    /// Most recent scrub errored or was canceled.
    Error,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorCounts {
    pub read: u64,
    pub write: u64,
    pub checksum: u64,
}

impl ErrorCounts {
    pub fn sum(&self) -> u64 {
        self.read + self.write + self.checksum
    }
}

#[derive(Clone, Debug)]
pub struct VdevNode {
    pub name: String,
    pub kind: VdevKind,
    pub state: VdevState,
    /// `None` for non-storage group headers (`logs` / `cache` / `spares`
    /// wrapper nodes under the root).
    pub size_bytes: Option<u64>,
    pub errors: ErrorCounts,
    pub children: Vec<VdevNode>,
}

impl VdevNode {
    /// Recursively sum `errors.sum()` across this node and every descendant.
    /// Used to populate `PoolInfo::errors` from the root vdev tree.
    pub fn total_errors(&self) -> u64 {
        let here = self.errors.sum();
        let kids: u64 = self.children.iter().map(|c| c.total_errors()).sum();
        here + kids
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VdevState {
    Online,
    Degraded,
    Faulted,
    Offline,
    Removed,
    Unavail,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VdevKind {
    /// The pool's root vdev — the parent of every top-level vdev.
    Root,
    /// RAID-Z vdev. Parity level: 1 = RAIDZ1, 2 = RAIDZ2, 3 = RAIDZ3.
    Raidz { parity: u8 },
    /// Mirror vdev.
    Mirror,
    /// Leaf disk backed by a block device.
    Disk,
    /// Leaf disk backed by a regular file (sparse-file pools, test setups).
    File,
    /// Placeholder wrapper under Root that groups log vdevs.
    LogGroup,
    /// A vdev under a `LogGroup`.
    LogVdev,
    /// Placeholder wrapper under Root that groups L2ARC cache vdevs.
    CacheGroup,
    /// A vdev under a `CacheGroup`.
    CacheVdev,
    /// Placeholder wrapper under Root that groups spare vdevs.
    SpareGroup,
    /// A vdev under a `SpareGroup`.
    SpareVdev,
}

fn label_for_vdev(v: &VdevNode) -> String {
    match v.kind {
        VdevKind::Raidz { parity } => format!("RAIDZ{parity}"),
        VdevKind::Mirror => "Mirror".into(),
        VdevKind::Disk | VdevKind::File => "Stripe".into(),
        _ => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(name: &str, errors: ErrorCounts) -> VdevNode {
        VdevNode {
            name: name.into(),
            kind: VdevKind::Disk,
            state: VdevState::Online,
            size_bytes: Some(1024 * 1024 * 1024),
            errors,
            children: vec![],
        }
    }

    #[test]
    fn vdev_total_errors_sums_tree() {
        let node = VdevNode {
            name: "raidz2-0".into(),
            kind: VdevKind::Raidz { parity: 2 },
            state: VdevState::Online,
            size_bytes: None,
            errors: ErrorCounts::default(),
            children: vec![
                leaf("sda", ErrorCounts { read: 1, write: 0, checksum: 0 }),
                leaf("sdb", ErrorCounts { read: 0, write: 2, checksum: 0 }),
                leaf("sdc", ErrorCounts { read: 0, write: 0, checksum: 3 }),
            ],
        };
        assert_eq!(node.total_errors(), 6);
    }

    #[test]
    fn vdev_total_errors_nested() {
        // Nested tree: root -> raidz -> 2 disks, root also has a log group.
        let node = VdevNode {
            name: "tank".into(),
            kind: VdevKind::Root,
            state: VdevState::Online,
            size_bytes: None,
            errors: ErrorCounts::default(),
            children: vec![
                VdevNode {
                    name: "raidz1-0".into(),
                    kind: VdevKind::Raidz { parity: 1 },
                    state: VdevState::Online,
                    size_bytes: Some(2 * 1024 * 1024 * 1024),
                    errors: ErrorCounts::default(),
                    children: vec![
                        leaf("sda", ErrorCounts { read: 1, ..Default::default() }),
                        leaf("sdb", ErrorCounts { write: 2, ..Default::default() }),
                    ],
                },
                VdevNode {
                    name: "logs".into(),
                    kind: VdevKind::LogGroup,
                    state: VdevState::Online,
                    size_bytes: None,
                    errors: ErrorCounts::default(),
                    children: vec![leaf(
                        "nvme0n1p1",
                        ErrorCounts { checksum: 4, ..Default::default() },
                    )],
                },
            ],
        };
        assert_eq!(node.total_errors(), 7);
    }

    #[test]
    fn capacity_fraction_handles_zero_size() {
        let p = PoolInfo {
            name: "empty".into(),
            health: PoolHealth::Online,
            allocated_bytes: 0,
            size_bytes: 0,
            free_bytes: 0,
            fragmentation_pct: None,
            scrub: ScrubState::Never,
            errors: ErrorCounts::default(),
            root_vdev: leaf("empty", ErrorCounts::default()),
        };
        assert_eq!(p.capacity_fraction(), 0.0);
    }

    #[test]
    fn capacity_fraction_basic() {
        let p = PoolInfo {
            name: "half".into(),
            health: PoolHealth::Online,
            allocated_bytes: 500,
            size_bytes: 1000,
            free_bytes: 500,
            fragmentation_pct: Some(5),
            scrub: ScrubState::Never,
            errors: ErrorCounts::default(),
            root_vdev: leaf("half", ErrorCounts::default()),
        };
        assert!((p.capacity_fraction() - 0.5).abs() < f64::EPSILON);
    }
}
