// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Jobs — renter intent — and the pure job-lifecycle state machine.
//!
//! The lifecycle mirrors the control-plane §3 state diagram. Transitions are a
//! total function [`JobState::apply`]: every legal `(state, event)` maps to a
//! successor, every illegal one returns [`IllegalTransition`] without mutating
//! anything. No async, no clock — just the transition relation, so it can be
//! exhaustively tested.
//!
//! One deliberate reading of the spec: the diagram draws renter-cancel edges
//! only from `queued`/`scheduled`/`running`, but the prose ("API owns …
//! renter-initiated → cancelled from any non-terminal state") is authoritative,
//! so [`JobEvent::Cancel`] is accepted from any non-terminal state.

use crate::capability::{Backend, BackendSet, IsolationTier, Version};
use crate::ids::{AccountId, CheckpointUri, JobId};
use crate::time::Timestamp;

/// Whether a job is a batch run or a long-lived serving deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WorkloadClass {
    /// Queued and bin-packed (control-plane §4).
    #[default]
    Batch,
    /// A long-lived replica reconciled to a desired count.
    Serving,
}

/// How a job selects its compute backend (compute-backends.md §resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BackendSelector {
    /// Resolve by intersecting the recipe's supported set with node capability,
    /// then the ADR-0015 priority order.
    #[default]
    Auto,
    /// Pin to exactly one backend; nodes lacking it are filtered out.
    Only(Backend),
}

/// The hard constraints a node must satisfy to run a job (control-plane §4
/// filter). Every field is a hard filter input; soft preferences live in the
/// scheduler's scoring, not here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceClaim {
    /// Minimum "GPU memory" in MB — a unified pool counts the whole pool.
    pub min_memory_mb: u64,
    /// Exact GPU model requirement, if any.
    pub gpu_model: Option<String>,
    /// Minimum driver version, if any.
    pub min_driver: Option<Version>,
    /// Minimum CUDA line, if any (only meaningful for CUDA nodes).
    pub min_cuda: Option<Version>,
    /// Minimum acceptable isolation tier.
    pub min_isolation: IsolationTier,
    /// Reliability floor in thousandths, `0..=1000`.
    pub min_reliability_milli: u16,
    /// Region preference; when set, only matching nodes qualify.
    pub region_pref: Option<String>,
    /// Price ceiling, integer micro-USD per second.
    pub max_price_per_sec_micro_usd: i64,
    /// How the backend is chosen.
    pub backend: BackendSelector,
    /// The recipe's supported backends, intersected with node capability under
    /// [`BackendSelector::Auto`].
    pub supported_backends: BackendSet,
}

/// A submitted job specification (renter intent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// Content-addressed image / runtime reference.
    pub image_ref: String,
    /// The hard resource constraints.
    pub claim: ResourceClaim,
    /// Batch vs serving.
    pub workload_class: WorkloadClass,
    /// Scheduling priority (higher runs first within a class).
    pub priority: i32,
    /// The checkpoint the next attempt resumes from, if any.
    pub checkpoint_uri: Option<CheckpointUri>,
}

/// A job row: identity, spec, lifecycle state, and timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Stable job identifier (the fencing lineage key).
    pub id: JobId,
    /// Owning account.
    pub account: AccountId,
    /// The submitted specification.
    pub spec: JobSpec,
    /// Current lifecycle state.
    pub state: JobState,
    /// When the job was submitted.
    pub submitted_at: Timestamp,
    /// When the job reached a terminal state, if it has.
    pub terminal_at: Option<Timestamp>,
}

/// The job lifecycle states (control-plane §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JobState {
    /// Accepted, not yet validated.
    Submitted,
    /// Validated and funds held; schedulable.
    Queued,
    /// The scheduler committed a lease.
    Scheduled,
    /// The gateway pushed the offer to an agent.
    Dispatched,
    /// The agent is pulling image / checkpoint.
    Preparing,
    /// The workload is running.
    Running,
    /// The workload is snapshotting.
    Checkpointing,
    /// A lost/preempted attempt is being requeued into a successor.
    Requeued,
    /// Preempted (owner-eject / higher class), awaiting requeue.
    Preempted,
    /// Finished successfully (terminal).
    Succeeded,
    /// Finished with failure (terminal).
    Failed,
    /// Cancelled by the renter (terminal).
    Cancelled,
}

impl JobState {
    /// Every state, for exhaustive enumeration in tests.
    pub const ALL: [Self; 12] = [
        Self::Submitted,
        Self::Queued,
        Self::Scheduled,
        Self::Dispatched,
        Self::Preparing,
        Self::Running,
        Self::Checkpointing,
        Self::Requeued,
        Self::Preempted,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
    ];

    /// Whether no further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// Applies `event`, returning the successor state.
    ///
    /// # Errors
    /// Returns [`IllegalTransition`] when `event` is not permitted from `self`;
    /// `self` is left unchanged (the function is pure — it returns the successor
    /// rather than mutating).
    pub fn apply(self, event: JobEvent) -> Result<Self, IllegalTransition> {
        // Renter cancel is permitted from any non-terminal state (control-plane
        // §3 prose), which is broader than the diagram's three drawn edges.
        if matches!(event, JobEvent::Cancel) {
            return if self.is_terminal() {
                Err(IllegalTransition { from: self, event })
            } else {
                Ok(Self::Cancelled)
            };
        }

        // One arm per lifecycle edge (control-plane §3): distinct transitions
        // that happen to share a successor state are kept separate for audit
        // clarity rather than merged into `|` patterns.
        #[allow(clippy::match_same_arms)]
        let next = match (self, event) {
            (Self::Submitted, JobEvent::Validate) => Self::Queued,
            (Self::Queued, JobEvent::Schedule) => Self::Scheduled,
            (Self::Scheduled, JobEvent::Dispatch) => Self::Dispatched,
            (Self::Dispatched, JobEvent::PrepareStarted) => Self::Preparing,
            (Self::Preparing, JobEvent::RunStarted) => Self::Running,
            (Self::Running, JobEvent::CheckpointStarted) => Self::Checkpointing,
            (Self::Checkpointing, JobEvent::CheckpointCompleted) => Self::Running,
            (Self::Running, JobEvent::Succeed) => Self::Succeeded,
            (Self::Running, JobEvent::Fail) => Self::Failed,
            (Self::Running, JobEvent::Lose) => Self::Requeued,
            (Self::Running, JobEvent::Preempt) => Self::Preempted,
            (Self::Checkpointing, JobEvent::Preempt) => Self::Requeued,
            (Self::Preempted, JobEvent::Requeue) => Self::Requeued,
            (Self::Requeued, JobEvent::Restart) => Self::Queued,
            _ => return Err(IllegalTransition { from: self, event }),
        };
        Ok(next)
    }
}

/// The events that drive the job lifecycle, grouped by the actor that owns them
/// (control-plane §3: API / scheduler / agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobEvent {
    /// API: validated the spec and held funds.
    Validate,
    /// Scheduler: committed a lease.
    Schedule,
    /// Gateway: pushed the offer to the agent.
    Dispatch,
    /// Agent: began pulling image / checkpoint.
    PrepareStarted,
    /// Agent: workload started.
    RunStarted,
    /// Agent: began a snapshot.
    CheckpointStarted,
    /// Agent: snapshot completed, resuming.
    CheckpointCompleted,
    /// Agent: reported exit 0.
    Succeed,
    /// Agent: reported non-zero / crash.
    Fail,
    /// Scheduler: attempt declared lost (silence / lease lapse).
    Lose,
    /// Scheduler: preempted (owner-eject / higher class).
    Preempt,
    /// Scheduler: a preempted job is requeued.
    Requeue,
    /// Scheduler: a requeued job re-enters the queue as a new attempt.
    Restart,
    /// API: renter cancellation (any non-terminal state).
    Cancel,
}

impl JobEvent {
    /// Every event, for exhaustive enumeration in tests.
    pub const ALL: [Self; 14] = [
        Self::Validate,
        Self::Schedule,
        Self::Dispatch,
        Self::PrepareStarted,
        Self::RunStarted,
        Self::CheckpointStarted,
        Self::CheckpointCompleted,
        Self::Succeed,
        Self::Fail,
        Self::Lose,
        Self::Preempt,
        Self::Requeue,
        Self::Restart,
        Self::Cancel,
    ];
}

/// A rejected job-lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("illegal job transition: {event:?} is not permitted from {from:?}")]
pub struct IllegalTransition {
    /// The state the transition was attempted from.
    pub from: JobState,
    /// The event that was rejected.
    pub event: JobEvent,
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    /// The legal transitions other than cancel, written out by hand so the test
    /// is an independent check on [`JobState::apply`] rather than a mirror of it.
    const LEGAL: &[(JobState, JobEvent, JobState)] = &[
        (JobState::Submitted, JobEvent::Validate, JobState::Queued),
        (JobState::Queued, JobEvent::Schedule, JobState::Scheduled),
        (
            JobState::Scheduled,
            JobEvent::Dispatch,
            JobState::Dispatched,
        ),
        (
            JobState::Dispatched,
            JobEvent::PrepareStarted,
            JobState::Preparing,
        ),
        (JobState::Preparing, JobEvent::RunStarted, JobState::Running),
        (
            JobState::Running,
            JobEvent::CheckpointStarted,
            JobState::Checkpointing,
        ),
        (
            JobState::Checkpointing,
            JobEvent::CheckpointCompleted,
            JobState::Running,
        ),
        (JobState::Running, JobEvent::Succeed, JobState::Succeeded),
        (JobState::Running, JobEvent::Fail, JobState::Failed),
        (JobState::Running, JobEvent::Lose, JobState::Requeued),
        (JobState::Running, JobEvent::Preempt, JobState::Preempted),
        (
            JobState::Checkpointing,
            JobEvent::Preempt,
            JobState::Requeued,
        ),
        (JobState::Preempted, JobEvent::Requeue, JobState::Requeued),
        (JobState::Requeued, JobEvent::Restart, JobState::Queued),
    ];

    /// The independently-computed expected result of a transition.
    fn expected(state: JobState, event: JobEvent) -> Option<JobState> {
        if matches!(event, JobEvent::Cancel) {
            return if state.is_terminal() {
                None
            } else {
                Some(JobState::Cancelled)
            };
        }
        LEGAL
            .iter()
            .find(|(from, ev, _)| *from == state && *ev == event)
            .map(|(_, _, to)| *to)
    }

    #[test]
    fn every_state_event_pair_matches_the_hand_written_table() {
        let mut legal_count = 0_usize;
        for state in JobState::ALL {
            for event in JobEvent::ALL {
                let want = expected(state, event);
                let got = state.apply(event).ok();
                assert_eq!(got, want, "transition ({state:?}, {event:?}) disagreed");
                if want.is_some() {
                    legal_count += 1;
                }
            }
        }
        // 14 non-cancel edges + one cancel edge from each of the 9 non-terminal states.
        assert_eq!(legal_count, LEGAL.len() + 9);
    }

    #[test]
    fn terminal_states_reject_every_event() {
        for state in [JobState::Succeeded, JobState::Failed, JobState::Cancelled] {
            assert!(state.is_terminal());
            for event in JobEvent::ALL {
                assert!(state.apply(event).is_err(), "{state:?} accepted {event:?}");
            }
        }
    }

    #[test]
    fn cancel_is_accepted_from_every_non_terminal_state() {
        for state in JobState::ALL {
            let result = state.apply(JobEvent::Cancel);
            if state.is_terminal() {
                assert!(result.is_err());
            } else {
                assert_eq!(result, Ok(JobState::Cancelled));
            }
        }
    }

    #[test]
    fn illegal_transition_reports_its_context() {
        let err = JobState::Queued.apply(JobEvent::Succeed);
        let Err(err) = err else {
            panic!("expected rejection")
        };
        assert_eq!(err.from, JobState::Queued);
        assert_eq!(err.event, JobEvent::Succeed);
    }

    #[test]
    fn a_requeue_lineage_walks_back_to_queued() {
        // running → lost → requeued → restart → queued (a checkpoint-resume cycle).
        let mut state = JobState::Running;
        state = state.apply(JobEvent::Lose).expect("running loses");
        assert_eq!(state, JobState::Requeued);
        state = state.apply(JobEvent::Restart).expect("requeued restarts");
        assert_eq!(state, JobState::Queued);
    }

    #[test]
    fn happy_path_runs_submitted_to_succeeded() {
        let steps = [
            JobEvent::Validate,
            JobEvent::Schedule,
            JobEvent::Dispatch,
            JobEvent::PrepareStarted,
            JobEvent::RunStarted,
            JobEvent::Succeed,
        ];
        let mut state = JobState::Submitted;
        for event in steps {
            state = state.apply(event).expect("happy-path step is legal");
        }
        assert_eq!(state, JobState::Succeeded);
        assert!(state.is_terminal());
    }
}
