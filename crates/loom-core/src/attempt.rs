// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Attempts — one placement of a job on a node.
//!
//! A job has the lifecycle in [`crate::job`]; each placement is an [`Attempt`]
//! with its own narrower phase (control-plane §2–3). A job with three lost
//! nodes has one job and four attempts, chained by [`crate::ids::AttemptNo`] and
//! carried checkpoints — the requeue lineage the fencing rules protect.

use crate::ids::{AttemptId, AttemptNo, CheckpointUri, FenceToken, JobId, LeaseId, NodeId};
use crate::time::Timestamp;

/// The narrow per-attempt phase (control-plane §2 DDL).
///
/// `Lost` and `Preempted` are neither live nor terminal: they are the exits
/// that spawn a requeued successor attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AttemptPhase {
    /// Lease committed, not yet pushed to the agent.
    #[default]
    Scheduled,
    /// Offer pushed to the agent.
    Dispatched,
    /// Agent pulling image / checkpoint.
    Preparing,
    /// Workload running.
    Running,
    /// Snapshotting (periodic or on preempt).
    Checkpointing,
    /// Reported exit 0.
    Succeeded,
    /// Reported non-zero / crash.
    Failed,
    /// Declared lost (silence past the timeout or lease lapse).
    Lost,
    /// Owner-ejected or preempted by a higher class.
    Preempted,
    /// Cancelled by the renter.
    Cancelled,
}

impl AttemptPhase {
    /// Whether the attempt is actively occupying its node.
    #[must_use]
    pub const fn is_live(self) -> bool {
        matches!(
            self,
            Self::Scheduled
                | Self::Dispatched
                | Self::Preparing
                | Self::Running
                | Self::Checkpointing
        )
    }

    /// Whether the attempt has reached a final phase for itself.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// Whether this exit requeues the job into a successor attempt.
    #[must_use]
    pub const fn requeues(self) -> bool {
        matches!(self, Self::Lost | Self::Preempted)
    }
}

/// One placement of a job on a node, with the fence it was granted and the
/// checkpoints it resumed from / produced.
// `attempt_no` mirrors the `job_attempts.attempt_no` DDL column (control-plane
// §2); the type-name echo is deliberate domain vocabulary.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// Stable attempt identifier.
    pub id: AttemptId,
    /// The job this attempt belongs to (the fencing lineage key).
    pub job: JobId,
    /// Monotonic per-job attempt number (1, 2, 3, …).
    pub attempt_no: AttemptNo,
    /// The node placement, once scheduled.
    pub node: Option<NodeId>,
    /// The lease authorizing this attempt, once committed.
    pub lease: Option<LeaseId>,
    /// The fencing token stamped on this attempt's messages.
    pub fence: Option<FenceToken>,
    /// Current phase.
    pub phase: AttemptPhase,
    /// Checkpoint this attempt resumed from.
    pub start_checkpoint: Option<CheckpointUri>,
    /// Checkpoint this attempt produced.
    pub end_checkpoint: Option<CheckpointUri>,
    /// Last state/heartbeat instant — drives the silence timeout.
    pub last_event_at: Option<Timestamp>,
}

impl Attempt {
    /// A scheduled attempt: [`AttemptPhase::Scheduled`] means the lease is already
    /// committed, so the authoritative placement (`node`, `lease`, `fence`) is carried
    /// from the start. `start_checkpoint` is the checkpoint this attempt resumes from:
    /// `None` for a first, cold-start attempt, and the predecessor's carried checkpoint
    /// for a requeued successor (control-plane §3.1), so a successor's domain state
    /// truthfully reports a warm resume rather than a cold start. Nothing has been
    /// *produced* or reported yet, so `end_checkpoint` and `last_event_at` start empty.
    #[must_use]
    pub fn scheduled(
        id: AttemptId,
        job: JobId,
        attempt_no: AttemptNo,
        node: NodeId,
        lease: LeaseId,
        fence: FenceToken,
        start_checkpoint: Option<CheckpointUri>,
    ) -> Self {
        Self {
            id,
            job,
            attempt_no,
            node: Some(node),
            lease: Some(lease),
            fence: Some(fence),
            phase: AttemptPhase::Scheduled,
            start_checkpoint,
            end_checkpoint: None,
            last_event_at: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[test]
    fn phase_classification_is_disjoint() {
        for phase in [
            AttemptPhase::Scheduled,
            AttemptPhase::Dispatched,
            AttemptPhase::Preparing,
            AttemptPhase::Running,
            AttemptPhase::Checkpointing,
            AttemptPhase::Succeeded,
            AttemptPhase::Failed,
            AttemptPhase::Lost,
            AttemptPhase::Preempted,
            AttemptPhase::Cancelled,
        ] {
            let classes = u8::from(phase.is_live())
                + u8::from(phase.is_terminal())
                + u8::from(phase.requeues());
            assert_eq!(
                classes, 1,
                "{phase:?} must be exactly one of live/terminal/requeue"
            );
        }
    }

    #[test]
    fn lost_and_preempted_are_the_requeue_exits() {
        assert!(AttemptPhase::Lost.requeues());
        assert!(AttemptPhase::Preempted.requeues());
        assert!(!AttemptPhase::Running.requeues());
    }

    #[test]
    fn scheduled_attempt_carries_committed_placement() {
        // A first, cold-start attempt: no checkpoint to resume from.
        let a = Attempt::scheduled(
            AttemptId::new("at-1"),
            JobId::new("job-1"),
            AttemptNo::FIRST,
            NodeId::new("node-1"),
            LeaseId::new("lease-1"),
            FenceToken(7),
            None,
        );
        assert_eq!(a.phase, AttemptPhase::Scheduled);
        // Scheduled means the lease is committed, so the placement is authoritative.
        assert_eq!(a.node, Some(NodeId::new("node-1")));
        assert_eq!(a.lease, Some(LeaseId::new("lease-1")));
        assert_eq!(a.fence, Some(FenceToken(7)));
        // Cold start: no checkpoint resumed/produced and nothing reported yet.
        assert!(a.start_checkpoint.is_none());
        assert!(a.end_checkpoint.is_none());
        assert!(a.last_event_at.is_none());
        assert_eq!(a.attempt_no, AttemptNo(1));
    }

    #[test]
    fn requeued_successor_carries_its_resume_checkpoint() {
        // A successor attempt (attempt_no > 1) is scheduled with the checkpoint its
        // predecessor carried across the requeue, so its domain state reports a warm
        // resume — not a false cold start (control-plane §3.1 checkpoint-resume lineage).
        let ckpt = CheckpointUri::new("sha256:resume-from-attempt-1");
        let a = Attempt::scheduled(
            AttemptId::new("at-2"),
            JobId::new("job-1"),
            AttemptNo(2),
            NodeId::new("node-2"),
            LeaseId::new("lease-2"),
            FenceToken(8),
            Some(ckpt.clone()),
        );
        assert_eq!(a.attempt_no, AttemptNo(2));
        // The durable checkpoint survives requeue into the successor's start state.
        assert_eq!(a.start_checkpoint, Some(ckpt));
        // Still nothing produced or reported on the fresh successor.
        assert!(a.end_checkpoint.is_none());
        assert!(a.last_event_at.is_none());
    }
}
