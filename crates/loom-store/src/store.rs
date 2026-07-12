// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The [`Store`] trait — every durable surface the control plane touches.
//!
//! It is one trait, deliberately, so the whole persistence contract has a single
//! conformance suite and a single seam that `loomd` boots either backend behind.
//! The methods are `async` (via [`async_trait`]) and the trait is object-safe,
//! so callers hold an `Arc<dyn Store>` without caring which backend answers.
//!
//! The fencing methods ([`commit_lease`](Store::commit_lease),
//! [`renew_lease`](Store::renew_lease)) carry the split-brain guard down into
//! persistence: a lease is written only if it strictly supersedes any live claim
//! on its node, and a renewal only extends a still-active lease. That guard is
//! the reason this crate is invariant-core.

use async_trait::async_trait;
use loom_core::{
    AccountId, Attempt, AttemptId, HostId, Job, JobId, JobState, Lease, LeaseId, Node, NodeId,
    NodeStatus, Timestamp, UsageRecord,
};

use crate::error::StoreError;
use crate::records::{
    Account, ApiKey, Gpu, Host, HostStatus, IdempotencyOutcome, IdempotencyRecord, JobQuery,
    LeaseCommit, NewOutboxEvent, OutboxEvent, OutboxId,
};

/// The durable persistence contract shared by every backend.
///
/// `Send + Sync` so an `Arc<dyn Store>` can cross task boundaries on a
/// multi-threaded runtime.
#[async_trait]
pub trait Store: Send + Sync {
    // ------------------------------------------------------------------ jobs

    /// Inserts a newly submitted job.
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if a job with the same id already exists.
    async fn insert_job(&self, job: &Job) -> Result<(), StoreError>;

    /// Fetches a job by id, or `None` if it does not exist.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, StoreError>;

    /// Lists jobs matching `query`, newest submission first.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn list_jobs(&self, query: &JobQuery) -> Result<Vec<Job>, StoreError>;

    /// Updates a job's lifecycle state, stamping `terminal_at` on a terminal
    /// transition (`None` otherwise).
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such job exists.
    async fn update_job_state(
        &self,
        id: &JobId,
        state: JobState,
        terminal_at: Option<Timestamp>,
    ) -> Result<(), StoreError>;

    // -------------------------------------------------------------- attempts

    /// Inserts a new attempt.
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if the id or the `(job, attempt_no)` pair is
    /// already taken.
    async fn insert_attempt(&self, attempt: &Attempt) -> Result<(), StoreError>;

    /// Fetches an attempt by id, or `None` if it does not exist.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn get_attempt(&self, id: &AttemptId) -> Result<Option<Attempt>, StoreError>;

    /// Lists a job's attempts in `attempt_no` order (the requeue lineage).
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn list_attempts_for_job(&self, job: &JobId) -> Result<Vec<Attempt>, StoreError>;

    /// Overwrites an attempt's mutable fields (phase, placement, checkpoints,
    /// last-event instant), keyed by its id.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such attempt exists.
    async fn update_attempt(&self, attempt: &Attempt) -> Result<(), StoreError>;

    // ---------------------------------------------------------------- leases

    /// Commits a lease under the fencing compare-and-set: it is written only if
    /// no active lease on `lease.node` already holds an equal-or-greater fence.
    ///
    /// Returns [`LeaseCommit::Committed`] on success, or
    /// [`LeaseCommit::Superseded`] carrying the blocking fence — the
    /// persistence-layer split-brain guard (agent-protocol §5).
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if the lease id already exists;
    /// [`StoreError::Backend`] on a backend failure.
    async fn commit_lease(&self, lease: &Lease) -> Result<LeaseCommit, StoreError>;

    /// Fetches a lease by id, or `None` if it does not exist.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn get_lease(&self, id: &LeaseId) -> Result<Option<Lease>, StoreError>;

    /// The active lease currently holding `node`, if any.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn active_lease_for_node(&self, node: &NodeId) -> Result<Option<Lease>, StoreError>;

    /// Renews a lease, extending its expiry. Rejected (returning `false`) unless
    /// the lease is active, not yet expired by `now`, and `new_expiry` strictly
    /// extends it — the same rule as [`Lease::renew`].
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such lease exists.
    async fn renew_lease(
        &self,
        id: &LeaseId,
        now: Timestamp,
        new_expiry: Timestamp,
    ) -> Result<bool, StoreError>;

    /// Marks a lease released, freeing its node for a fresh commit.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such lease exists.
    async fn release_lease(&self, id: &LeaseId) -> Result<(), StoreError>;

    // ----------------------------------------------------------------- usage

    /// Ingests a usage record, idempotently on `(attempt, seq)`. Returns `true`
    /// if newly stored, `false` if a record with that key already existed (a
    /// harmless replay).
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn insert_usage(&self, record: &UsageRecord) -> Result<bool, StoreError>;

    /// Lists an attempt's usage records in `seq` order.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn list_usage_for_attempt(
        &self,
        attempt: &AttemptId,
    ) -> Result<Vec<UsageRecord>, StoreError>;

    // ---------------------------------------------------------------- outbox

    /// Enqueues an outbox row, returning its assigned [`OutboxId`].
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn enqueue_outbox(&self, event: &NewOutboxEvent) -> Result<OutboxId, StoreError>;

    /// Lists up to `limit` unsent outbox rows in id order (oldest first).
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn list_unsent_outbox(&self, limit: u32) -> Result<Vec<OutboxEvent>, StoreError>;

    /// Marks an outbox row published at `sent_at`.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such row exists.
    async fn mark_outbox_sent(&self, id: OutboxId, sent_at: Timestamp) -> Result<(), StoreError>;

    // ----------------------------------------------------------- idempotency

    /// Records an idempotency mapping if absent. See [`IdempotencyOutcome`] for
    /// the three cases (stored / replay / reuse-mismatch).
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn put_idempotency(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyOutcome, StoreError>;

    /// Fetches an idempotency mapping by `(account, key)`, or `None`.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn get_idempotency(
        &self,
        account: &AccountId,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, StoreError>;

    // --------------------------------------------------------- hosts / gpus

    /// Inserts an enrolled host.
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if the host id already exists.
    async fn insert_host(&self, host: &Host) -> Result<(), StoreError>;

    /// Fetches a host by id, or `None`.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn get_host(&self, id: &HostId) -> Result<Option<Host>, StoreError>;

    /// Sets a host's enrollment status.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such host exists.
    async fn set_host_status(&self, id: &HostId, status: HostStatus) -> Result<(), StoreError>;

    /// Inserts a GPU advertised by a host.
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if the GPU id already exists.
    async fn insert_gpu(&self, gpu: &Gpu) -> Result<(), StoreError>;

    /// Lists the GPUs advertised by a host.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn list_gpus_for_host(&self, host: &HostId) -> Result<Vec<Gpu>, StoreError>;

    // ----------------------------------------------------------------- nodes

    /// Inserts or replaces a schedulable node offer, keyed by id.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn upsert_node(&self, node: &Node) -> Result<(), StoreError>;

    /// Fetches a node by id, or `None`.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn get_node(&self, id: &NodeId) -> Result<Option<Node>, StoreError>;

    /// Lists nodes the scheduler may offer work to (status `Available`).
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn list_schedulable_nodes(&self) -> Result<Vec<Node>, StoreError>;

    /// Sets a node's schedulable status.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such node exists.
    async fn set_node_status(&self, id: &NodeId, status: NodeStatus) -> Result<(), StoreError>;

    /// Records a node heartbeat at `at`.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such node exists.
    async fn record_node_heartbeat(&self, id: &NodeId, at: Timestamp) -> Result<(), StoreError>;

    // ------------------------------------------------------ accounts / keys

    /// Inserts an account.
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if the account id already exists.
    async fn insert_account(&self, account: &Account) -> Result<(), StoreError>;

    /// Fetches an account by id, or `None`.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn get_account(&self, id: &AccountId) -> Result<Option<Account>, StoreError>;

    /// Inserts an API key.
    ///
    /// # Errors
    /// [`StoreError::Conflict`] if the key id or key hash already exists.
    async fn insert_api_key(&self, key: &ApiKey) -> Result<(), StoreError>;

    /// Looks up a non-revoked API key by its stored hash, or `None`. A revoked
    /// key never resolves — the auth path treats it as absent.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a backend failure.
    async fn api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, StoreError>;

    /// Revokes an API key by id (idempotent — revoking a revoked key is fine).
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such key exists.
    async fn revoke_api_key(&self, id: &str) -> Result<(), StoreError>;
}
