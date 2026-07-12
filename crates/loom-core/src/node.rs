// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Nodes — concrete, schedulable capacity offers.
//!
//! A node is a host plus a specific GPU, an isolation tier, advertised compute
//! backends, a memory model, and a price (control-plane §2). The scheduler's
//! filter reads exactly these fields.

use crate::capability::{BackendSet, IsolationTier, MemoryModel, Version};
use crate::ids::{HostId, NodeId};
use crate::time::Timestamp;

/// Lifecycle status of a node's schedulable capacity (control-plane §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NodeStatus {
    /// Not currently reachable; never schedulable.
    #[default]
    Offline,
    /// Reachable and free — the only status the filter accepts.
    Available,
    /// Currently running an attempt/replica under a live lease.
    Leased,
    /// Winding down; not accepting new work.
    Draining,
    /// The host owner reclaimed the machine — owner always wins (control-plane §4).
    OwnerEjected,
}

impl NodeStatus {
    /// Whether a node in this status may be offered fresh work.
    #[must_use]
    pub const fn is_schedulable(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// A schedulable capacity offer. Reliability and price are integers to sidestep
/// float/decimal divergence: reliability is thousandths in `0..=1000`, price is
/// integer micro-USD (control-plane §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Stable node identifier.
    pub id: NodeId,
    /// The host advertising this node.
    pub host: HostId,
    /// Current schedulable status.
    pub status: NodeStatus,
    /// GPU model string, e.g. `RTX 4090` or `M3 Max`.
    pub gpu_model: String,
    /// Advertised memory pool (unified or discrete) and size.
    pub memory: MemoryModel,
    /// Compute backends this node can execute.
    pub backends: BackendSet,
    /// Installed GPU driver version.
    pub driver: Version,
    /// CUDA line the node can serve, when applicable.
    pub cuda: Option<Version>,
    /// Isolation tier this node can offer.
    pub isolation: IsolationTier,
    /// Coarse placement domain.
    pub region: String,
    /// Reliability score in thousandths, `0..=1000` (control-plane §5).
    pub reliability_milli: u16,
    /// Host ask, integer micro-USD per second.
    pub price_per_sec_micro_usd: i64,
    /// Last heartbeat instant, if the node has ever reported.
    pub last_heartbeat_at: Option<Timestamp>,
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;
    use crate::capability::{Backend, MemoryKind};

    fn m3_max() -> Node {
        Node {
            id: NodeId::new("node-m3"),
            host: HostId::new("host-founder"),
            status: NodeStatus::Available,
            gpu_model: "M3 Max".to_owned(),
            memory: MemoryModel::new(MemoryKind::Unified, 48_000),
            backends: BackendSet::from_backends(&[Backend::Mlx, Backend::Cpu]),
            driver: Version::new(1, 0, 0),
            cuda: None,
            isolation: IsolationTier::B,
            region: "local".to_owned(),
            reliability_milli: 900,
            price_per_sec_micro_usd: 0,
            last_heartbeat_at: Some(Timestamp::from_millis(1_000)),
        }
    }

    #[test]
    fn only_available_nodes_are_schedulable() {
        assert!(NodeStatus::Available.is_schedulable());
        assert!(!NodeStatus::Offline.is_schedulable());
        assert!(!NodeStatus::Leased.is_schedulable());
        assert!(!NodeStatus::Draining.is_schedulable());
        assert!(!NodeStatus::OwnerEjected.is_schedulable());
    }

    #[test]
    fn node_advertises_its_capability_tuple() {
        let node = m3_max();
        assert!(node.backends.contains(Backend::Mlx));
        assert_eq!(node.memory.kind, MemoryKind::Unified);
        assert_eq!(node.memory.size_mb(), 48_000);
        assert!(node.status.is_schedulable());
    }
}
