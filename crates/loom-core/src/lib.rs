// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-core` — the invariant heart of Loom: domain types and pure state
//! machines with **zero I/O**.
//!
//! This crate carries the identifiers, the job/attempt/node/lease/usage types,
//! and the deterministic logic that must never be wrong: the job-lifecycle FSM
//! ([`job`]), the lease/fencing rules ([`fencing`]), and the scheduler's
//! `filter` → `score` → `commit` decision logic ([`scheduler`]). It reads no
//! clock (`SystemTime::now` is a denied method — inject a [`time::Timestamp`])
//! and touches no socket or database. If a thing does I/O, it does not live here.
//!
//! The split-brain guard runs through the whole crate: leases carry a monotonic
//! [`ids::FenceToken`], a requeued attempt always gets a strictly greater fence,
//! and a stale fence is rejected — so two nodes can never both *effect* work on
//! the same attempt lineage.
//!
//! Design docs: control-plane.md (lifecycle, scheduler), agent-protocol.md §5
//! (fencing), compute-backends.md / ADR-0015 (backends, capability).

// Domain type names deliberately carry their prefix (`JobState`, `LeaseState`,
// `NodeStatus`) so they read unambiguously at call sites across crate
// boundaries; the module-name echo is intentional.
#![allow(clippy::module_name_repetitions)]

pub mod attempt;
pub mod capability;
pub mod fencing;
pub mod ids;
pub mod job;
pub mod lease;
pub mod node;
pub mod scheduler;
pub mod time;
pub mod usage;

pub use attempt::{Attempt, AttemptPhase};
pub use capability::{Backend, BackendSet, IsolationTier, MemoryKind, MemoryModel, Version};
pub use fencing::{FenceError, FenceVerdict, LeaseBook, LeaseGrant};
pub use ids::{
    AccountId, AttemptId, AttemptNo, CheckpointUri, DeploymentId, FenceToken, GpuId, HostId, JobId,
    LeaseId, NodeId, ReplicaId,
};
pub use job::{
    BackendSelector, IllegalTransition, Job, JobEvent, JobSpec, JobState, ResourceClaim,
    WorkloadClass,
};
pub use lease::{Lease, LeaseState};
pub use node::{Node, NodeStatus};
pub use scheduler::{
    NodeOffer, Placement, PlacementSignals, Score, ScoreWeights, is_feasible, plan_placement,
    resolve_backend, score,
};
pub use time::Timestamp;
pub use usage::{UsageRecord, UsageValidation};
