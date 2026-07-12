// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Row types the store owns that the pure domain model does not.
//!
//! `loom-core` owns `Job`/`Attempt`/`Lease`/`UsageRecord`/`Node`; those are
//! persisted and returned as-is. This module carries the remaining durable
//! surfaces — the transactional [`OutboxEvent`], the [`IdempotencyRecord`]
//! window, and the enrollment/auth rows ([`Host`], [`Gpu`], [`Account`],
//! [`ApiKey`]) — plus the small query/outcome enums the [`Store`](crate::Store)
//! trait needs at its edges.

use loom_core::{AccountId, FenceToken, GpuId, HostId, JobState, Timestamp};

/// The outcome of a fencing-guarded [`commit_lease`](crate::Store::commit_lease).
///
/// The lease is written only if it strictly supersedes any live claim already
/// on the node; otherwise the caller learns which fence currently holds it. This
/// is the persistence-layer half of the split-brain guard (agent-protocol §5):
/// a superseded (lower-fence) writer can never claim a node a greater fence
/// already owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseCommit {
    /// The lease was written — it holds the node now.
    Committed,
    /// Rejected: an active lease with an equal-or-greater fence already holds
    /// the node. The current high-water fence is reported for diagnostics.
    Superseded {
        /// The fence of the live claim that blocked this commit.
        current_fence: FenceToken,
    },
}

impl LeaseCommit {
    /// Whether the lease was committed.
    #[must_use]
    pub const fn is_committed(self) -> bool {
        matches!(self, Self::Committed)
    }
}

/// A filter for [`list_jobs`](crate::Store::list_jobs). An unset field matches
/// every value; the default matches every job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobQuery {
    /// Restrict to a single owning account.
    pub account: Option<AccountId>,
    /// Restrict to a single lifecycle state.
    pub state: Option<JobState>,
}

/// The store-assigned identifier of an [`OutboxEvent`] — a monotonic rowid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutboxId(pub i64);

/// An outbox row to enqueue: a state-change *nudge* written in the same
/// transaction as the change it announces (control-plane §3). The store assigns
/// the [`OutboxId`] and stamps the sent marker; the caller supplies the content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboxEvent {
    /// The event kind — the relay's routing key.
    pub topic: String,
    /// The serialized payload (JSON text on both dialects).
    pub payload: String,
    /// When the announcing transaction was written.
    pub created_at: Timestamp,
}

/// A persisted outbox row, as read back by the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEvent {
    /// The store-assigned id.
    pub id: OutboxId,
    /// The event kind.
    pub topic: String,
    /// The serialized payload.
    pub payload: String,
    /// When the row was written.
    pub created_at: Timestamp,
    /// When the relay marked it published, if it has.
    pub sent_at: Option<Timestamp>,
}

/// A stored idempotency-key mapping (renter-api §1.3): `(account, key)` → the
/// original response, so a retried `POST` replays rather than re-executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    /// The account the key is scoped to.
    pub account: AccountId,
    /// The client-supplied key.
    pub key: String,
    /// A hash of the request body — a reuse with a different body is a conflict.
    pub request_hash: String,
    /// The original response status code.
    pub response_status: u16,
    /// The original response body.
    pub response_body: String,
    /// When the mapping was first stored (drives the 24 h window).
    pub created_at: Timestamp,
}

/// The outcome of [`put_idempotency`](crate::Store::put_idempotency).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyOutcome {
    /// No prior key — the record was stored; execute the request.
    Stored,
    /// The key exists with the *same* request — replay the stored response.
    Replayed(IdempotencyRecord),
    /// The key exists with a *different* request body (renter-api §1.3 reuse).
    Mismatch,
}

/// Enrollment/identity lifecycle of a host machine (control-plane §2 `hosts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HostStatus {
    /// Presented a CSR, not yet approved.
    #[default]
    Pending,
    /// Approved and holding a signed node cert.
    Enrolled,
    /// Revoked — its cert no longer trusted.
    Revoked,
}

/// A host machine: the enrolled owner of one or more [`Gpu`]s and the nodes they
/// back (control-plane §2 `hosts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// Stable host identifier.
    pub id: HostId,
    /// The account that owns the machine.
    pub account: AccountId,
    /// The agent's identity public key (used to verify its signed messages).
    pub agent_pubkey: Vec<u8>,
    /// Current enrollment status.
    pub status: HostStatus,
    /// When enrollment was first recorded.
    pub enrolled_at: Timestamp,
    /// Last time the host was seen, if ever.
    pub last_seen_at: Option<Timestamp>,
}

/// One physical GPU advertised by a host (control-plane §2 `gpus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    /// Stable GPU identifier.
    pub id: GpuId,
    /// The host advertising it.
    pub host: HostId,
    /// Model string, e.g. `RTX 4090` or `M3 Max`.
    pub model: String,
    /// Advertised memory in megabytes.
    pub memory_mb: u64,
    /// Benchmark fingerprint used to cross-check reported utilization, if taken.
    pub fingerprint: Option<String>,
}

/// A billing/ownership account (control-plane §2 `accounts`). Balances and
/// transactions are deferred with the money path; Phase 1 stores identity only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Stable account identifier.
    pub id: AccountId,
    /// Human-facing display name.
    pub name: String,
    /// When the account was created.
    pub created_at: Timestamp,
}

/// An API key granting an account access to the renter API (control-plane §2
/// `api_keys`). Only a hash of the token is stored — never the token itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKey {
    /// Stable key identifier (safe to log; not the secret).
    pub id: String,
    /// The account the key authenticates as.
    pub account: AccountId,
    /// A hash of the presented token — the auth lookup key.
    pub key_hash: String,
    /// Human-facing label for the key.
    pub label: String,
    /// When the key was issued.
    pub created_at: Timestamp,
    /// Whether the key has been revoked (a revoked key never authenticates).
    pub revoked: bool,
}
