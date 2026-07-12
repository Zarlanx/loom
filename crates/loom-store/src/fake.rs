// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`FakeStore`] — the in-memory [`Store`] used for tests and in-process
//! integration harnesses.
//!
//! It ships inside the crate that defines the trait (workspace-setup.md §6) and
//! is validated by the same [`conformance`](crate::conformance) suite the real
//! `SqliteStore` passes, so it can never silently diverge. It holds everything
//! behind one `Mutex` and clones cheaply (`Arc`), so a whole `loomd` can be
//! booted around it with no sockets, no files, and no root.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use loom_core::{
    AccountId, Attempt, AttemptId, GpuId, HostId, Job, JobId, JobState, Lease, LeaseId, LeaseState,
    Node, NodeId, NodeStatus, Timestamp, UsageRecord,
};

use crate::error::StoreError;
use crate::records::{
    Account, ApiKey, Gpu, Host, HostStatus, IdempotencyOutcome, IdempotencyRecord, JobQuery,
    LeaseCommit, NewOutboxEvent, OutboxEvent, OutboxId,
};
use crate::store::Store;

/// The mutable heart of a [`FakeStore`].
#[derive(Debug, Default)]
struct Inner {
    jobs: HashMap<JobId, Job>,
    attempts: HashMap<AttemptId, Attempt>,
    leases: HashMap<LeaseId, Lease>,
    usage: Vec<UsageRecord>,
    outbox: Vec<OutboxEvent>,
    outbox_next: i64,
    idempotency: HashMap<(AccountId, String), IdempotencyRecord>,
    hosts: HashMap<HostId, Host>,
    gpus: HashMap<GpuId, Gpu>,
    nodes: HashMap<NodeId, Node>,
    accounts: HashMap<AccountId, Account>,
    api_keys: HashMap<String, ApiKey>,
}

/// An in-memory [`Store`]. Cheap to clone; every clone shares one backing map.
#[derive(Debug, Clone, Default)]
pub struct FakeStore {
    inner: Arc<Mutex<Inner>>,
}

impl FakeStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the backing map, recovering from a poisoned lock rather than
    /// panicking — a poisoned `FakeStore` mutex still holds valid data, and
    /// unwinding here would violate the crate's no-`unwrap` discipline.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl Store for FakeStore {
    // ------------------------------------------------------------------ jobs

    async fn insert_job(&self, job: &Job) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.jobs.contains_key(&job.id) {
            return Err(StoreError::Conflict(format!("job {} exists", job.id)));
        }
        inner.jobs.insert(job.id.clone(), job.clone());
        Ok(())
    }

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, StoreError> {
        Ok(self.lock().jobs.get(id).cloned())
    }

    async fn list_jobs(&self, query: &JobQuery) -> Result<Vec<Job>, StoreError> {
        let inner = self.lock();
        let mut jobs: Vec<Job> = inner
            .jobs
            .values()
            .filter(|job| query.account.as_ref().is_none_or(|a| *a == job.account))
            .filter(|job| query.state.is_none_or(|s| s == job.state))
            .cloned()
            .collect();
        // Newest submission first; id breaks ties for a deterministic order.
        jobs.sort_by(|a, b| {
            b.submitted_at
                .cmp(&a.submitted_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(jobs)
    }

    async fn update_job_state(
        &self,
        id: &JobId,
        state: JobState,
        terminal_at: Option<Timestamp>,
    ) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let job = inner.jobs.get_mut(id).ok_or(StoreError::NotFound)?;
        job.state = state;
        job.terminal_at = terminal_at;
        Ok(())
    }

    // -------------------------------------------------------------- attempts

    async fn insert_attempt(&self, attempt: &Attempt) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.attempts.contains_key(&attempt.id) {
            return Err(StoreError::Conflict(format!(
                "attempt {} exists",
                attempt.id
            )));
        }
        if inner
            .attempts
            .values()
            .any(|a| a.job == attempt.job && a.attempt_no == attempt.attempt_no)
        {
            return Err(StoreError::Conflict(format!(
                "attempt_no {} already used for job {}",
                attempt.attempt_no, attempt.job
            )));
        }
        inner.attempts.insert(attempt.id.clone(), attempt.clone());
        Ok(())
    }

    async fn get_attempt(&self, id: &AttemptId) -> Result<Option<Attempt>, StoreError> {
        Ok(self.lock().attempts.get(id).cloned())
    }

    async fn list_attempts_for_job(&self, job: &JobId) -> Result<Vec<Attempt>, StoreError> {
        let inner = self.lock();
        let mut attempts: Vec<Attempt> = inner
            .attempts
            .values()
            .filter(|a| a.job == *job)
            .cloned()
            .collect();
        attempts.sort_by_key(|a| a.attempt_no);
        Ok(attempts)
    }

    async fn update_attempt(&self, attempt: &Attempt) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if !inner.attempts.contains_key(&attempt.id) {
            return Err(StoreError::NotFound);
        }
        inner.attempts.insert(attempt.id.clone(), attempt.clone());
        Ok(())
    }

    // ---------------------------------------------------------------- leases

    async fn commit_lease(&self, lease: &Lease) -> Result<LeaseCommit, StoreError> {
        let mut inner = self.lock();
        if inner.leases.contains_key(&lease.id) {
            return Err(StoreError::Conflict(format!("lease {} exists", lease.id)));
        }
        // The fencing compare-and-set: an active lease on the same node whose
        // fence is >= ours blocks the commit (a superseded writer cannot claim a
        // node a greater fence already owns).
        if let Some(blocker) = inner
            .leases
            .values()
            .filter(|l| l.node == lease.node && l.state == LeaseState::Active)
            .max_by_key(|l| l.fence)
            && blocker.fence >= lease.fence
        {
            return Ok(LeaseCommit::Superseded {
                current_fence: blocker.fence,
            });
        }
        inner.leases.insert(lease.id.clone(), lease.clone());
        Ok(LeaseCommit::Committed)
    }

    async fn get_lease(&self, id: &LeaseId) -> Result<Option<Lease>, StoreError> {
        Ok(self.lock().leases.get(id).cloned())
    }

    async fn active_lease_for_node(&self, node: &NodeId) -> Result<Option<Lease>, StoreError> {
        let inner = self.lock();
        Ok(inner
            .leases
            .values()
            .filter(|l| l.node == *node && l.state == LeaseState::Active)
            .max_by_key(|l| l.fence)
            .cloned())
    }

    async fn renew_lease(
        &self,
        id: &LeaseId,
        now: Timestamp,
        new_expiry: Timestamp,
    ) -> Result<bool, StoreError> {
        let mut inner = self.lock();
        let lease = inner.leases.get_mut(id).ok_or(StoreError::NotFound)?;
        Ok(lease.renew(now, new_expiry))
    }

    async fn release_lease(&self, id: &LeaseId) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let lease = inner.leases.get_mut(id).ok_or(StoreError::NotFound)?;
        lease.state = LeaseState::Released;
        Ok(())
    }

    // ----------------------------------------------------------------- usage

    async fn insert_usage(&self, record: &UsageRecord) -> Result<bool, StoreError> {
        let mut inner = self.lock();
        if inner
            .usage
            .iter()
            .any(|r| r.attempt == record.attempt && r.seq == record.seq)
        {
            return Ok(false);
        }
        inner.usage.push(record.clone());
        Ok(true)
    }

    async fn list_usage_for_attempt(
        &self,
        attempt: &AttemptId,
    ) -> Result<Vec<UsageRecord>, StoreError> {
        let inner = self.lock();
        let mut records: Vec<UsageRecord> = inner
            .usage
            .iter()
            .filter(|r| r.attempt == *attempt)
            .cloned()
            .collect();
        records.sort_by_key(|r| r.seq);
        Ok(records)
    }

    // ---------------------------------------------------------------- outbox

    async fn enqueue_outbox(&self, event: &NewOutboxEvent) -> Result<OutboxId, StoreError> {
        let mut inner = self.lock();
        let id = OutboxId(inner.outbox_next);
        inner.outbox_next += 1;
        inner.outbox.push(OutboxEvent {
            id,
            topic: event.topic.clone(),
            payload: event.payload.clone(),
            created_at: event.created_at,
            sent_at: None,
        });
        Ok(id)
    }

    async fn list_unsent_outbox(&self, limit: u32) -> Result<Vec<OutboxEvent>, StoreError> {
        let inner = self.lock();
        let mut rows: Vec<OutboxEvent> = inner
            .outbox
            .iter()
            .filter(|e| e.sent_at.is_none())
            .cloned()
            .collect();
        rows.sort_by_key(|e| e.id);
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn mark_outbox_sent(&self, id: OutboxId, sent_at: Timestamp) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let row = inner
            .outbox
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or(StoreError::NotFound)?;
        row.sent_at = Some(sent_at);
        Ok(())
    }

    // ----------------------------------------------------------- idempotency

    async fn put_idempotency(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyOutcome, StoreError> {
        let mut inner = self.lock();
        let key = (record.account.clone(), record.key.clone());
        if let Some(existing) = inner.idempotency.get(&key) {
            return Ok(if existing.request_hash == record.request_hash {
                IdempotencyOutcome::Replayed(existing.clone())
            } else {
                IdempotencyOutcome::Mismatch
            });
        }
        inner.idempotency.insert(key, record.clone());
        Ok(IdempotencyOutcome::Stored)
    }

    async fn get_idempotency(
        &self,
        account: &AccountId,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, StoreError> {
        let inner = self.lock();
        Ok(inner
            .idempotency
            .get(&(account.clone(), key.to_owned()))
            .cloned())
    }

    // --------------------------------------------------------- hosts / gpus

    async fn insert_host(&self, host: &Host) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.hosts.contains_key(&host.id) {
            return Err(StoreError::Conflict(format!("host {} exists", host.id)));
        }
        inner.hosts.insert(host.id.clone(), host.clone());
        Ok(())
    }

    async fn get_host(&self, id: &HostId) -> Result<Option<Host>, StoreError> {
        Ok(self.lock().hosts.get(id).cloned())
    }

    async fn set_host_status(&self, id: &HostId, status: HostStatus) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let host = inner.hosts.get_mut(id).ok_or(StoreError::NotFound)?;
        host.status = status;
        Ok(())
    }

    async fn insert_gpu(&self, gpu: &Gpu) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.gpus.contains_key(&gpu.id) {
            return Err(StoreError::Conflict(format!("gpu {} exists", gpu.id)));
        }
        inner.gpus.insert(gpu.id.clone(), gpu.clone());
        Ok(())
    }

    async fn list_gpus_for_host(&self, host: &HostId) -> Result<Vec<Gpu>, StoreError> {
        let inner = self.lock();
        let mut gpus: Vec<Gpu> = inner
            .gpus
            .values()
            .filter(|g| g.host == *host)
            .cloned()
            .collect();
        gpus.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(gpus)
    }

    // ----------------------------------------------------------------- nodes

    async fn upsert_node(&self, node: &Node) -> Result<(), StoreError> {
        self.lock().nodes.insert(node.id.clone(), node.clone());
        Ok(())
    }

    async fn get_node(&self, id: &NodeId) -> Result<Option<Node>, StoreError> {
        Ok(self.lock().nodes.get(id).cloned())
    }

    async fn list_schedulable_nodes(&self) -> Result<Vec<Node>, StoreError> {
        let inner = self.lock();
        let mut nodes: Vec<Node> = inner
            .nodes
            .values()
            .filter(|n| n.status == NodeStatus::Available)
            .cloned()
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(nodes)
    }

    async fn set_node_status(&self, id: &NodeId, status: NodeStatus) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let node = inner.nodes.get_mut(id).ok_or(StoreError::NotFound)?;
        node.status = status;
        Ok(())
    }

    async fn record_node_heartbeat(&self, id: &NodeId, at: Timestamp) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let node = inner.nodes.get_mut(id).ok_or(StoreError::NotFound)?;
        node.last_heartbeat_at = Some(at);
        Ok(())
    }

    // ------------------------------------------------------ accounts / keys

    async fn insert_account(&self, account: &Account) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.accounts.contains_key(&account.id) {
            return Err(StoreError::Conflict(format!(
                "account {} exists",
                account.id
            )));
        }
        inner.accounts.insert(account.id.clone(), account.clone());
        Ok(())
    }

    async fn get_account(&self, id: &AccountId) -> Result<Option<Account>, StoreError> {
        Ok(self.lock().accounts.get(id).cloned())
    }

    async fn insert_api_key(&self, key: &ApiKey) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.api_keys.contains_key(&key.id) {
            return Err(StoreError::Conflict(format!("api key {} exists", key.id)));
        }
        if inner.api_keys.values().any(|k| k.key_hash == key.key_hash) {
            return Err(StoreError::Conflict("api key hash exists".to_owned()));
        }
        inner.api_keys.insert(key.id.clone(), key.clone());
        Ok(())
    }

    async fn api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, StoreError> {
        let inner = self.lock();
        Ok(inner
            .api_keys
            .values()
            .find(|k| k.key_hash == key_hash && !k.revoked)
            .cloned())
    }

    async fn revoke_api_key(&self, id: &str) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let key = inner.api_keys.get_mut(id).ok_or(StoreError::NotFound)?;
        key.revoked = true;
        Ok(())
    }
}
