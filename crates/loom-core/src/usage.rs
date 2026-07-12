// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Usage records — the signed, per-window meter readings that drive local usage
//! accounting (control-plane §6).
//!
//! `loom-core` owns the record *shape* and the pure plausibility check; the
//! agent signs them and the store persists them (idempotent on
//! `(attempt, seq)`). Each record carries the attempt's `fence` so stale-node
//! readings are fenced off before they can bill.

use crate::ids::{AttemptId, FenceToken, HostId, NodeId};
use crate::time::Timestamp;

/// Validation state of a usage record (control-plane §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UsageValidation {
    /// Not yet checked.
    #[default]
    Pending,
    /// Passed plausibility and signature checks.
    Valid,
    /// Failed plausibility — quarantined, not billed.
    Implausible,
    /// Under dispute.
    Disputed,
}

/// A per-window meter reading. `seq` is monotonic per attempt (gaps/rollbacks
/// are fraud signals); `(attempt, seq)` is the idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// The attempt being metered.
    pub attempt: AttemptId,
    /// The node that produced the reading.
    pub node: NodeId,
    /// The host that owns the node.
    pub host: HostId,
    /// The attempt's fencing token — a stale fence is rejected before billing.
    pub fence: FenceToken,
    /// Monotonic per-attempt sequence number.
    pub seq: u64,
    /// Start of the metered window.
    pub window_start: Timestamp,
    /// End of the metered window.
    pub window_end: Timestamp,
    /// Billable seconds in the window (never more than wall-clock).
    pub billable_secs: u32,
    /// Reported GPU utilization percent, if measured.
    pub gpu_util_pct: Option<u8>,
    /// Current validation state.
    pub validation: UsageValidation,
}

impl UsageRecord {
    /// Whole seconds spanned by the window (never negative).
    #[must_use]
    pub fn window_secs(&self) -> i64 {
        self.window_end
            .saturating_millis_since(self.window_start)
            .max(0)
            / 1_000
    }

    /// Whether the record is self-consistent: the window is non-negative,
    /// billable seconds do not exceed wall-clock, and utilization is in range.
    ///
    /// This is the pure, per-record half of the aggregator's checks
    /// (control-plane §6); signature validation is the agent-gateway's job.
    #[must_use]
    pub fn is_plausible(&self) -> bool {
        self.window_end >= self.window_start
            && i64::from(self.billable_secs) <= self.window_secs()
            && self.gpu_util_pct.is_none_or(|util| util <= 100)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    fn record() -> UsageRecord {
        UsageRecord {
            attempt: AttemptId::new("at-1"),
            node: NodeId::new("node-1"),
            host: HostId::new("host-1"),
            fence: FenceToken(1),
            seq: 0,
            window_start: Timestamp::from_millis(0),
            window_end: Timestamp::from_millis(10_000),
            billable_secs: 10,
            gpu_util_pct: Some(85),
            validation: UsageValidation::Pending,
        }
    }

    #[test]
    fn a_well_formed_record_is_plausible() {
        assert!(record().is_plausible());
        assert_eq!(record().window_secs(), 10);
    }

    #[test]
    fn billing_more_than_wall_clock_is_implausible() {
        let mut r = record();
        r.billable_secs = 11;
        assert!(!r.is_plausible());
    }

    #[test]
    fn reversed_window_is_implausible() {
        let mut r = record();
        r.window_start = Timestamp::from_millis(20_000);
        assert!(!r.is_plausible());
    }

    #[test]
    fn utilization_over_one_hundred_is_implausible() {
        let mut r = record();
        r.gpu_util_pct = Some(101);
        assert!(!r.is_plausible());
    }
}
