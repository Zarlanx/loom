// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Lease/fencing rules — the split-brain guard (agent-protocol §5).
//!
//! Two nodes must never both *effect* work on the same attempt lineage. The
//! guard is a **monotonic fencing token**: the scheduler is the single writer
//! that mints fences, every lease carries one, a requeued attempt always gets a
//! **strictly greater** fence, and any message stamped with a **stale (lower)**
//! fence is rejected. The old node learns its fence is stale from the rejection
//! and tears down.
//!
//! A *lineage* is a job's chain of attempts (control-plane §2–3). [`LeaseBook`]
//! is the pure model of the scheduler's fencing authority: one global monotonic
//! counter (so every fence is globally unique and increasing) plus the current
//! high-water fence per lineage. It mints leases, requeues them, and verifies
//! inbound fences — with no I/O and no clock.

use core::cmp::Ordering;
use std::collections::BTreeMap;

use crate::ids::{AttemptId, AttemptNo, CheckpointUri, FenceToken, JobId, LeaseId, NodeId};
use crate::lease::{Lease, LeaseState};
use crate::time::Timestamp;

/// The verdict of checking an inbound message's fence against the lineage's
/// current fence (agent-protocol §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceVerdict {
    /// The fence matches the current lease — the message is authoritative.
    Accept,
    /// A lower fence than the current lease — a fenced-off, superseded node.
    Stale,
    /// An unknown lineage or an impossibly-high fence (only the book mints).
    Invalid,
}

impl FenceVerdict {
    /// Whether the message may take effect.
    #[must_use]
    pub const fn is_accept(self) -> bool {
        matches!(self, Self::Accept)
    }
}

/// A newly minted lease plus the lineage bookkeeping it advanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseGrant {
    /// The lease to persist and dispatch.
    pub lease: Lease,
    /// The attempt number this grant created (1 for the first, then monotone).
    pub attempt_no: AttemptNo,
    /// The fence minted for this grant.
    pub fence: FenceToken,
}

/// A failed fencing operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FenceError {
    /// Tried to grant an initial lease for a lineage that already exists.
    #[error("lineage for job {job} already exists")]
    DuplicateLineage {
        /// The offending job.
        job: JobId,
    },
    /// Tried to requeue or inspect a lineage that was never granted.
    #[error("no lease lineage exists for job {job}")]
    UnknownLineage {
        /// The offending job.
        job: JobId,
    },
    /// A fenced write presented a fence *lower* than the lineage's current one — a
    /// superseded attempt trying to take effect after a greater fence was minted
    /// (mirrors [`FenceVerdict::Stale`]).
    #[error("stale fence {presented} for job {job}; current fence is {current}")]
    StaleFence {
        /// The lineage whose fence was checked.
        job: JobId,
        /// The fence the writer presented.
        presented: FenceToken,
        /// The lineage's current high-water fence.
        current: FenceToken,
    },
    /// A fenced write presented a fence *higher* than the lineage's current one — an
    /// impossibly-high/forged fence, since only the book mints (mirrors
    /// [`FenceVerdict::Invalid`]).
    #[error("invalid fence {presented} for job {job}; current fence is {current}")]
    InvalidFence {
        /// The lineage whose fence was checked.
        job: JobId,
        /// The fence the writer presented.
        presented: FenceToken,
        /// The lineage's current high-water fence.
        current: FenceToken,
    },
    /// The `u64` fence counter is exhausted (unreachable in practice).
    #[error("fence token space exhausted")]
    FenceExhausted,
}

/// The current fencing state of one lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lineage {
    attempt_no: AttemptNo,
    fence: FenceToken,
    node: NodeId,
    /// Whether the current attempt still holds a live lease (vs. declared lost).
    live: bool,
    /// The checkpoint the lineage resumes from, carried across requeues.
    checkpoint: Option<CheckpointUri>,
}

/// The pure fencing authority: the single writer that mints and verifies fences.
///
/// Construct with [`LeaseBook::new`], drive it with [`grant_initial`](Self::grant_initial)
/// and [`requeue`](Self::requeue), and verify inbound fences with [`check`](Self::check).
///
/// **Not `Clone`/`Copy` by design.** The book *is* the single global minter
/// (`next_fence`); duplicating it would fork that counter, and the two copies could
/// each mint the *same* next fence for conflicting grants — breaking the global
/// uniqueness and split-brain guarantees this type exists to hold. There is exactly
/// one authority, so it is moved or borrowed, never copied. Do not add a `Clone`
/// (or `Copy`) derive here.
#[derive(Debug)]
pub struct LeaseBook {
    /// Global monotonic minter — the next fence value to hand out.
    next_fence: u64,
    lineages: BTreeMap<JobId, Lineage>,
}

impl Default for LeaseBook {
    fn default() -> Self {
        // Fences start at 1 so 0 can denote "no fence yet" at call sites.
        Self {
            next_fence: 1,
            lineages: BTreeMap::new(),
        }
    }
}

impl LeaseBook {
    /// Creates an empty book whose first minted fence is `1`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mints the next globally-unique, strictly-increasing fence.
    fn mint(&mut self) -> Result<FenceToken, FenceError> {
        let value = self.next_fence;
        self.next_fence = self
            .next_fence
            .checked_add(1)
            .ok_or(FenceError::FenceExhausted)?;
        Ok(FenceToken(value))
    }

    /// Builds the deterministic lease that a grant hands out.
    fn make_lease(
        attempt: AttemptId,
        node: NodeId,
        fence: FenceToken,
        granted_at: Timestamp,
        expires_at: Timestamp,
    ) -> Lease {
        Lease {
            // Fences are globally unique, so they make a stable, collision-free id.
            id: LeaseId::new(format!("lease-{}", fence.value())),
            attempt,
            node,
            fence,
            granted_at,
            expires_at,
            state: LeaseState::Active,
        }
    }

    /// Grants the first lease of a lineage (`attempt_no = 1`).
    ///
    /// # Errors
    /// Returns [`FenceError::DuplicateLineage`] if the lineage already exists, or
    /// [`FenceError::FenceExhausted`] if the fence counter overflows.
    pub fn grant_initial(
        &mut self,
        job: JobId,
        attempt: AttemptId,
        node: NodeId,
        granted_at: Timestamp,
        expires_at: Timestamp,
    ) -> Result<LeaseGrant, FenceError> {
        if self.lineages.contains_key(&job) {
            return Err(FenceError::DuplicateLineage { job });
        }
        let fence = self.mint()?;
        let attempt_no = AttemptNo::FIRST;
        self.lineages.insert(
            job,
            Lineage {
                attempt_no,
                fence,
                node: node.clone(),
                live: true,
                checkpoint: None,
            },
        );
        Ok(LeaseGrant {
            lease: Self::make_lease(attempt, node, fence, granted_at, expires_at),
            attempt_no,
            fence,
        })
    }

    /// Requeues a lost/preempted attempt as its successor, minting a
    /// **strictly greater** fence and incrementing the attempt number.
    ///
    /// `resume_from` is the checkpoint the new attempt starts from; when `None`
    /// the lineage's last carried checkpoint is reused.
    ///
    /// # Errors
    /// Returns [`FenceError::UnknownLineage`] if the lineage was never granted, or
    /// [`FenceError::FenceExhausted`] if the fence counter overflows.
    pub fn requeue(
        &mut self,
        job: JobId,
        attempt: AttemptId,
        node: NodeId,
        granted_at: Timestamp,
        expires_at: Timestamp,
        resume_from: Option<CheckpointUri>,
    ) -> Result<LeaseGrant, FenceError> {
        let Some(existing) = self.lineages.get(&job) else {
            return Err(FenceError::UnknownLineage { job });
        };
        let attempt_no = existing.attempt_no.next();
        let checkpoint = resume_from.or_else(|| existing.checkpoint.clone());
        // Borrow of `existing` ends here; mint needs `&mut self`.
        let fence = self.mint()?;
        self.lineages.insert(
            job,
            Lineage {
                attempt_no,
                fence,
                node: node.clone(),
                live: true,
                checkpoint,
            },
        );
        Ok(LeaseGrant {
            lease: Self::make_lease(attempt, node, fence, granted_at, expires_at),
            attempt_no,
            fence,
        })
    }

    /// Records the checkpoint the current attempt produced, so the next requeue
    /// resumes from it.
    ///
    /// The write is fence-checked: `fence` must be the lineage's *current* fence, so
    /// only the current-fence holder may record — not merely a "live" attempt.
    /// [`mark_lost`](Self::mark_lost) clears `live` without advancing the fence, so a
    /// lost-but-not-yet-requeued attempt still holds the current fence and may record;
    /// what is rejected is a *superseded* attempt — one still holding a fence from before
    /// a requeue minted a greater one — so it can never clobber the successor's carried
    /// checkpoint after a requeue (agent-protocol §5).
    ///
    /// # Errors
    /// Returns [`FenceError::UnknownLineage`] if the lineage does not exist,
    /// [`FenceError::StaleFence`] if `fence` is lower than the lineage's current fence, or
    /// [`FenceError::InvalidFence`] if it is higher (impossibly-high/forged). The fence
    /// is classified with the same three-way ordering as [`check`](Self::check).
    pub fn record_checkpoint(
        &mut self,
        job: &JobId,
        fence: FenceToken,
        checkpoint: CheckpointUri,
    ) -> Result<(), FenceError> {
        let Some(lineage) = self.lineages.get_mut(job) else {
            return Err(FenceError::UnknownLineage { job: job.clone() });
        };
        match fence.cmp(&lineage.fence) {
            Ordering::Equal => {}
            Ordering::Less => {
                return Err(FenceError::StaleFence {
                    job: job.clone(),
                    presented: fence,
                    current: lineage.fence,
                });
            }
            // Higher than the only minter ever produced — impossible/forged, so it is
            // reported distinctly from a merely-superseded (stale) fence, matching `check`.
            Ordering::Greater => {
                return Err(FenceError::InvalidFence {
                    job: job.clone(),
                    presented: fence,
                    current: lineage.fence,
                });
            }
        }
        lineage.checkpoint = Some(checkpoint);
        Ok(())
    }

    /// Marks the current attempt lost (awaiting requeue). The fence is unchanged,
    /// so — until a requeue mints a greater fence — the still-current fence is
    /// the one that verifies (agent-protocol §5: rejection follows the *fence*,
    /// not liveness).
    ///
    /// # Errors
    /// Returns [`FenceError::UnknownLineage`] if the lineage does not exist.
    pub fn mark_lost(&mut self, job: &JobId) -> Result<(), FenceError> {
        let Some(lineage) = self.lineages.get_mut(job) else {
            return Err(FenceError::UnknownLineage { job: job.clone() });
        };
        lineage.live = false;
        Ok(())
    }

    /// Retires a completed lineage, removing its bookkeeping from the book.
    ///
    /// [`LeaseBook`] is the long-lived scheduling authority, so without a retirement path
    /// its `lineages` map would grow for the life of the process — one entry per job ever
    /// scheduled. `complete` is the hook the store layer calls to reclaim a lineage once
    /// its job reaches a terminal state.
    ///
    /// The retirement is fence-checked exactly like
    /// [`record_checkpoint`](Self::record_checkpoint): `fence` must be the lineage's *current*
    /// fence, so only the current-fence holder may retire it — completion follows the fence,
    /// not liveness. [`mark_lost`](Self::mark_lost) clears `live` while leaving the fence
    /// authoritative, so a lost-but-current-fence attempt may still complete the lineage; a
    /// *superseded* attempt — one still holding a pre-requeue fence — cannot tear down a
    /// lineage that has already moved on to a greater fence. Because the global minter only
    /// ever increases, a `JobId` scheduled
    /// again after retirement still receives a strictly greater fence than any it held
    /// before, so reclaiming an entry never risks fence reuse.
    ///
    /// # Errors
    /// Returns [`FenceError::UnknownLineage`] if the lineage does not exist,
    /// [`FenceError::StaleFence`] if `fence` is lower than the lineage's current fence, or
    /// [`FenceError::InvalidFence`] if it is higher (impossibly-high/forged). The fence is
    /// classified with the same three-way ordering as [`check`](Self::check).
    pub fn complete(&mut self, job: &JobId, fence: FenceToken) -> Result<(), FenceError> {
        let Some(lineage) = self.lineages.get(job) else {
            return Err(FenceError::UnknownLineage { job: job.clone() });
        };
        match fence.cmp(&lineage.fence) {
            Ordering::Equal => {}
            Ordering::Less => {
                return Err(FenceError::StaleFence {
                    job: job.clone(),
                    presented: fence,
                    current: lineage.fence,
                });
            }
            // Higher than the only minter ever produced — impossible/forged, reported
            // distinctly from a merely-superseded (stale) fence, matching `check`.
            Ordering::Greater => {
                return Err(FenceError::InvalidFence {
                    job: job.clone(),
                    presented: fence,
                    current: lineage.fence,
                });
            }
        }
        self.lineages.remove(job);
        Ok(())
    }

    /// Verifies an inbound message's fence for a lineage (agent-protocol §5).
    #[must_use]
    pub fn check(&self, job: &JobId, fence: FenceToken) -> FenceVerdict {
        match self.lineages.get(job) {
            None => FenceVerdict::Invalid,
            Some(lineage) => match fence.cmp(&lineage.fence) {
                Ordering::Equal => FenceVerdict::Accept,
                Ordering::Less => FenceVerdict::Stale,
                // Higher than the only minter ever produced — impossible/forged.
                Ordering::Greater => FenceVerdict::Invalid,
            },
        }
    }

    /// The lineage's current high-water fence, if it exists.
    #[must_use]
    pub fn current_fence(&self, job: &JobId) -> Option<FenceToken> {
        self.lineages.get(job).map(|lineage| lineage.fence)
    }

    /// The lineage's current attempt number, if it exists.
    #[must_use]
    pub fn attempt_no(&self, job: &JobId) -> Option<AttemptNo> {
        self.lineages.get(job).map(|lineage| lineage.attempt_no)
    }

    /// The node the lineage's current attempt is placed on, if it exists.
    #[must_use]
    pub fn current_node(&self, job: &JobId) -> Option<NodeId> {
        self.lineages.get(job).map(|lineage| lineage.node.clone())
    }

    /// Whether the lineage's current attempt still holds a live lease.
    #[must_use]
    pub fn is_live(&self, job: &JobId) -> Option<bool> {
        self.lineages.get(job).map(|lineage| lineage.live)
    }

    /// The checkpoint the lineage would resume from on its next requeue.
    #[must_use]
    pub fn lineage_checkpoint(&self, job: &JobId) -> Option<CheckpointUri> {
        self.lineages
            .get(job)
            .and_then(|lineage| lineage.checkpoint.clone())
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    fn t(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    fn grant(book: &mut LeaseBook, job: &str) -> LeaseGrant {
        let n = book.attempt_no(&JobId::new(job)).map_or(1, |a| a.get() + 1);
        let attempt = AttemptId::new(format!("{job}-{n}"));
        let node = NodeId::new(format!("node-{job}-{n}"));
        let exists = book.current_fence(&JobId::new(job)).is_some();
        let result = if exists {
            book.requeue(JobId::new(job), attempt, node, t(0), t(30_000), None)
        } else {
            book.grant_initial(JobId::new(job), attempt, node, t(0), t(30_000))
        };
        match result {
            Ok(g) => g,
            Err(e) => panic!("grant failed: {e}"),
        }
    }

    #[test]
    fn first_grant_is_attempt_one_and_lease_is_active() {
        let mut book = LeaseBook::new();
        let g = grant(&mut book, "j");
        assert_eq!(g.attempt_no, AttemptNo::FIRST);
        assert_eq!(g.lease.state, LeaseState::Active);
        assert_eq!(g.lease.fence, g.fence);
        assert_eq!(book.current_fence(&JobId::new("j")), Some(g.fence));
        assert_eq!(
            book.current_node(&JobId::new("j")),
            Some(NodeId::new("node-j-1"))
        );
    }

    #[test]
    fn duplicate_initial_grant_is_rejected() {
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let first = book.grant_initial(
            job.clone(),
            AttemptId::new("a"),
            NodeId::new("n"),
            t(0),
            t(1),
        );
        assert!(first.is_ok());
        let dup = book.grant_initial(
            job.clone(),
            AttemptId::new("b"),
            NodeId::new("m"),
            t(0),
            t(1),
        );
        assert_eq!(dup, Err(FenceError::DuplicateLineage { job }));
    }

    #[test]
    fn requeue_without_lineage_is_rejected() {
        let mut book = LeaseBook::new();
        let job = JobId::new("ghost");
        let out = book.requeue(
            job.clone(),
            AttemptId::new("a"),
            NodeId::new("n"),
            t(0),
            t(1),
            None,
        );
        assert_eq!(out, Err(FenceError::UnknownLineage { job }));
    }

    #[test]
    fn requeue_mints_a_strictly_greater_fence_and_fences_off_the_old_node() {
        // The agent-protocol §5 walk-through: f=7 then f=8, stale 7 rejected.
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let first = grant(&mut book, "j");
        let second = grant(&mut book, "j");
        assert!(
            second.fence > first.fence,
            "requeue fence must be strictly greater"
        );
        assert_eq!(second.attempt_no, AttemptNo(2));

        // Only the current fence is authoritative; the superseded one is fenced off.
        assert_eq!(book.check(&job, second.fence), FenceVerdict::Accept);
        assert_eq!(book.check(&job, first.fence), FenceVerdict::Stale);
        assert!(!book.check(&job, first.fence).is_accept());
    }

    #[test]
    fn an_impossibly_high_or_unknown_fence_is_invalid() {
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let g = grant(&mut book, "j");
        assert_eq!(
            book.check(&job, FenceToken(g.fence.value() + 100)),
            FenceVerdict::Invalid
        );
        assert_eq!(
            book.check(&JobId::new("other"), g.fence),
            FenceVerdict::Invalid
        );
    }

    #[test]
    fn mark_lost_keeps_the_current_fence_authoritative_until_requeue() {
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let g = grant(&mut book, "j");
        assert_eq!(book.is_live(&job), Some(true));
        let lost = book.mark_lost(&job);
        assert!(lost.is_ok());
        assert_eq!(book.is_live(&job), Some(false));
        // Not yet requeued: the current fence still verifies.
        assert_eq!(book.check(&job, g.fence), FenceVerdict::Accept);
    }

    #[test]
    fn checkpoints_carry_forward_across_a_requeue() {
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let first = grant(&mut book, "j");
        let ckpt = CheckpointUri::new("sha256:abc");
        assert!(
            book.record_checkpoint(&job, first.fence, ckpt.clone())
                .is_ok()
        );
        let _second = grant(&mut book, "j");
        assert_eq!(book.lineage_checkpoint(&job), Some(ckpt));
    }

    #[test]
    fn record_checkpoint_rejects_a_non_current_fence() {
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let first = grant(&mut book, "j");
        // The first attempt records a checkpoint under its (then-current) fence.
        let carried = CheckpointUri::new("sha256:from-attempt-1");
        assert!(
            book.record_checkpoint(&job, first.fence, carried.clone())
                .is_ok()
        );

        // A requeue mints a strictly greater fence; the checkpoint carries forward.
        let second = grant(&mut book, "j");
        assert!(second.fence > first.fence);
        assert_eq!(book.lineage_checkpoint(&job), Some(carried.clone()));

        // The superseded attempt, still holding the old fence, tries to overwrite the
        // carried checkpoint. It is fenced off, and the carried checkpoint is unchanged.
        let stale = CheckpointUri::new("sha256:stale-from-attempt-1");
        assert_eq!(
            book.record_checkpoint(&job, first.fence, stale),
            Err(FenceError::StaleFence {
                job: job.clone(),
                presented: first.fence,
                current: second.fence,
            })
        );
        assert_eq!(book.lineage_checkpoint(&job), Some(carried));

        // The live successor, holding the current fence, may still record.
        let next = CheckpointUri::new("sha256:from-attempt-2");
        assert!(
            book.record_checkpoint(&job, second.fence, next.clone())
                .is_ok()
        );
        assert_eq!(book.lineage_checkpoint(&job), Some(next));
    }

    #[test]
    fn record_checkpoint_on_unknown_lineage_is_rejected() {
        let mut book = LeaseBook::new();
        let job = JobId::new("ghost");
        assert_eq!(
            book.record_checkpoint(&job, FenceToken(1), CheckpointUri::new("sha256:x")),
            Err(FenceError::UnknownLineage { job })
        );
    }

    #[test]
    fn record_checkpoint_rejects_an_impossibly_high_fence_as_invalid() {
        // A fence above the current one can only be forged (the book is the sole minter),
        // so `record_checkpoint` classifies it as `InvalidFence` — distinct from a merely
        // superseded (stale, lower) fence — exactly as `check` does, and leaves the
        // lineage's carried checkpoint untouched.
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let g = grant(&mut book, "j");
        let forged = FenceToken(g.fence.value() + 100);
        assert_eq!(book.check(&job, forged), FenceVerdict::Invalid);
        assert_eq!(
            book.record_checkpoint(&job, forged, CheckpointUri::new("sha256:forged")),
            Err(FenceError::InvalidFence {
                job: job.clone(),
                presented: forged,
                current: g.fence,
            })
        );
        assert_eq!(book.lineage_checkpoint(&job), None);
    }

    #[test]
    fn fences_are_globally_unique_across_lineages() {
        let mut book = LeaseBook::new();
        let a = grant(&mut book, "a");
        let b = grant(&mut book, "b");
        let a2 = grant(&mut book, "a");
        assert_ne!(a.fence, b.fence);
        assert_ne!(b.fence, a2.fence);
        assert!(
            a.fence < b.fence && b.fence < a2.fence,
            "global counter is monotone"
        );
    }

    #[test]
    fn complete_retires_the_lineage_and_frees_the_job_id() {
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let g = grant(&mut book, "j");
        // The live attempt retires its own lineage.
        assert!(book.complete(&job, g.fence).is_ok());
        // The bookkeeping is gone: nothing verifies or reports state for the job.
        assert_eq!(book.check(&job, g.fence), FenceVerdict::Invalid);
        assert_eq!(book.current_fence(&job), None);
        assert_eq!(book.attempt_no(&job), None);
        // The id may be granted afresh, and the global minter still hands out a strictly
        // greater fence than the retired lineage ever held — retirement never reuses a fence.
        let Ok(reborn) = book.grant_initial(
            job.clone(),
            AttemptId::new("a2"),
            NodeId::new("n2"),
            t(0),
            t(1),
        ) else {
            panic!("job id must be free after retirement");
        };
        assert!(reborn.fence > g.fence);
    }

    #[test]
    fn complete_is_fence_checked_against_stale_forged_and_unknown() {
        let mut book = LeaseBook::new();
        let job = JobId::new("j");
        let first = grant(&mut book, "j");
        let second = grant(&mut book, "j"); // a requeue mints a strictly greater fence
        assert!(second.fence > first.fence);

        // A superseded attempt cannot retire a lineage that has already moved on.
        assert_eq!(
            book.complete(&job, first.fence),
            Err(FenceError::StaleFence {
                job: job.clone(),
                presented: first.fence,
                current: second.fence,
            })
        );
        // A forged, impossibly-high fence is rejected as invalid — distinct from stale.
        let forged = FenceToken(second.fence.value() + 100);
        assert_eq!(
            book.complete(&job, forged),
            Err(FenceError::InvalidFence {
                job: job.clone(),
                presented: forged,
                current: second.fence,
            })
        );
        // An unknown lineage is rejected.
        assert_eq!(
            book.complete(&JobId::new("ghost"), first.fence),
            Err(FenceError::UnknownLineage {
                job: JobId::new("ghost")
            })
        );
        // Every rejection left the lineage intact; the live successor can still retire it.
        assert_eq!(book.check(&job, second.fence), FenceVerdict::Accept);
        assert!(book.complete(&job, second.fence).is_ok());
        assert_eq!(book.current_fence(&job), None);
    }
}
