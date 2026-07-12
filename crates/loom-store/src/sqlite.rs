// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`SqliteStore`] — the Phase-1 [`Store`] backend, on `sqlx` over a file-backed
//! WAL database.
//!
//! Every method persists and returns the same `loom-core` aggregates the pure
//! logic operates on; the enum/newtype ↔ column mappings live in this module as
//! total, runtime-checked conversions (backend.md §4 uses runtime `query()`, not
//! the compile-time `query!`, so the workspace builds with no live database and
//! the dual-dialect seam stays behind the `Store` trait).
//!
//! The store opens with `journal_mode = WAL`, `foreign_keys = ON`, and a
//! `busy_timeout`, so concurrent writers serialize on the write lock and *wait*
//! rather than erroring — the production configuration the conformance suite is
//! deliberately run against (never `:memory:`, workspace-setup.md §6).
//!
//! [`commit_lease`](SqliteStore::commit_lease) is the load-bearing method: it
//! runs the fencing compare-and-set inside one transaction so a superseded
//! (lower-fence) writer can never claim a node a greater fence already holds.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use loom_core::{
    AccountId, Attempt, AttemptId, AttemptNo, AttemptPhase, Backend, BackendSelector, BackendSet,
    CheckpointUri, FenceToken, GpuId, HostId, IsolationTier, Job, JobId, JobSpec, JobState, Lease,
    LeaseId, LeaseState, MemoryKind, MemoryModel, Node, NodeId, NodeStatus, ResourceClaim,
    Timestamp, UsageRecord, UsageValidation, Version, WorkloadClass,
};
use sqlx::Row;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
    SqliteSynchronous,
};

use crate::error::StoreError;
use crate::records::{
    Account, ApiKey, Gpu, Host, HostStatus, IdempotencyOutcome, IdempotencyRecord, JobQuery,
    LeaseCommit, NewOutboxEvent, OutboxEvent, OutboxId,
};
use crate::store::Store;

/// The SQLite-backed [`Store`]. Cheap to clone — every clone shares one pool.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Opens (creating if absent) the file-backed WAL database at `path`, runs
    /// the embedded migration set, and returns a ready store.
    ///
    /// # Errors
    /// [`StoreError::Backend`] if the database cannot be opened or migrated.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let pool = connect_pool(path, 5).await?;
        crate::migrate::MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| StoreError::Backend(format!("migrate: {e}")))?;
        Ok(Self { pool })
    }

    /// Borrows the underlying pool (for callers that share the connection, e.g.
    /// the outbox relay or `loomd`'s wiring).
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Builds a pool for the WAL database at `path` with the production pragmas.
pub(crate) async fn connect_pool(
    path: &Path,
    max_connections: u32,
) -> Result<SqlitePool, StoreError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
        .map_err(|e| StoreError::Backend(format!("connect {}: {e}", path.display())))
}

/// Maps an `sqlx` error onto the backend-agnostic [`StoreError`], distinguishing
/// the uniqueness/FK conflicts callers match on from opaque backend failures.
fn map_db(err: sqlx::Error) -> StoreError {
    match err {
        sqlx::Error::Database(db) if db.is_unique_violation() || db.is_foreign_key_violation() => {
            StoreError::Conflict(db.message().to_owned())
        }
        other => StoreError::Backend(other.to_string()),
    }
}

/// A `NOT NULL` fence value → i64 for binding.
fn fence_to_i64(fence: FenceToken) -> Result<i64, StoreError> {
    i64::try_from(fence.value()).map_err(|e| StoreError::Backend(format!("fence range: {e}")))
}

/// A stored i64 → a rehydrated [`FenceToken`].
fn fence_from_i64(value: i64) -> Result<FenceToken, StoreError> {
    u64::try_from(value)
        .map(FenceToken::from_persisted)
        .map_err(|e| StoreError::Backend(format!("fence range: {e}")))
}

impl SqliteStore {
    /// `rows_affected == 0` → [`StoreError::NotFound`], for update/delete paths.
    fn require_affected(affected: u64) -> Result<(), StoreError> {
        if affected == 0 {
            Err(StoreError::NotFound)
        } else {
            Ok(())
        }
    }
}

// ================================================================ Store impl

#[async_trait]
impl Store for SqliteStore {
    // ------------------------------------------------------------------ jobs

    async fn insert_job(&self, job: &Job) -> Result<(), StoreError> {
        let claim = &job.spec.claim;
        sqlx::query(
            "INSERT INTO jobs (\
                id, account_id, image_ref, workload_class, priority, checkpoint_uri, \
                state, submitted_at, terminal_at, \
                claim_min_memory_mb, claim_gpu_model, claim_min_driver, claim_min_cuda, \
                claim_min_isolation, claim_min_reliability_milli, claim_region_pref, \
                claim_max_price_micro_usd, claim_backend_selector, claim_supported_backends\
             ) VALUES (\
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
                ?18, ?19)",
        )
        .bind(job.id.as_str())
        .bind(job.account.as_str())
        .bind(job.spec.image_ref.as_str())
        .bind(workload_class_str(job.spec.workload_class))
        .bind(i64::from(job.spec.priority))
        .bind(job.spec.checkpoint_uri.as_ref().map(CheckpointUri::as_str))
        .bind(job_state_str(job.state))
        .bind(job.submitted_at.as_millis())
        .bind(job.terminal_at.map(Timestamp::as_millis))
        .bind(u64_to_i64(claim.min_memory_mb, "claim_min_memory_mb")?)
        .bind(claim.gpu_model.as_deref())
        .bind(claim.min_driver.map(|v| v.to_string()))
        .bind(claim.min_cuda.map(|v| v.to_string()))
        .bind(isolation_str(claim.min_isolation))
        .bind(i64::from(claim.min_reliability_milli))
        .bind(claim.region_pref.as_deref())
        .bind(claim.max_price_per_sec_micro_usd)
        .bind(backend_selector_str(claim.backend))
        .bind(backend_set_to_csv(claim.supported_backends))
        .execute(&self.pool)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, StoreError> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db)?;
        row.as_ref().map(row_to_job).transpose()
    }

    async fn list_jobs(&self, query: &JobQuery) -> Result<Vec<Job>, StoreError> {
        // Optional filters are applied with NULL-guarded predicates so one
        // prepared statement serves every combination.
        let rows = sqlx::query(
            "SELECT * FROM jobs \
             WHERE (?1 IS NULL OR account_id = ?1) \
               AND (?2 IS NULL OR state = ?2) \
             ORDER BY submitted_at DESC, id ASC",
        )
        .bind(query.account.as_ref().map(AccountId::as_str))
        .bind(query.state.map(job_state_str))
        .fetch_all(&self.pool)
        .await
        .map_err(map_db)?;
        rows.iter().map(row_to_job).collect()
    }

    async fn update_job_state(
        &self,
        id: &JobId,
        state: JobState,
        terminal_at: Option<Timestamp>,
    ) -> Result<(), StoreError> {
        let affected = sqlx::query("UPDATE jobs SET state = ?1, terminal_at = ?2 WHERE id = ?3")
            .bind(job_state_str(state))
            .bind(terminal_at.map(Timestamp::as_millis))
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_db)?
            .rows_affected();
        Self::require_affected(affected)
    }

    // -------------------------------------------------------------- attempts

    async fn insert_attempt(&self, attempt: &Attempt) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO job_attempts (\
                id, job_id, attempt_no, node_id, lease_id, fence, phase, \
                start_checkpoint_uri, end_checkpoint_uri, last_event_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(attempt.id.as_str())
        .bind(attempt.job.as_str())
        .bind(i64::from(attempt.attempt_no.get()))
        .bind(attempt.node.as_ref().map(NodeId::as_str))
        .bind(attempt.lease.as_ref().map(LeaseId::as_str))
        .bind(attempt.fence.map(fence_to_i64).transpose()?)
        .bind(attempt_phase_str(attempt.phase))
        .bind(attempt.start_checkpoint.as_ref().map(CheckpointUri::as_str))
        .bind(attempt.end_checkpoint.as_ref().map(CheckpointUri::as_str))
        .bind(attempt.last_event_at.map(Timestamp::as_millis))
        .execute(&self.pool)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_attempt(&self, id: &AttemptId) -> Result<Option<Attempt>, StoreError> {
        let row = sqlx::query("SELECT * FROM job_attempts WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db)?;
        row.as_ref().map(row_to_attempt).transpose()
    }

    async fn list_attempts_for_job(&self, job: &JobId) -> Result<Vec<Attempt>, StoreError> {
        let rows =
            sqlx::query("SELECT * FROM job_attempts WHERE job_id = ?1 ORDER BY attempt_no ASC")
                .bind(job.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(map_db)?;
        rows.iter().map(row_to_attempt).collect()
    }

    async fn update_attempt(&self, attempt: &Attempt) -> Result<(), StoreError> {
        let affected = sqlx::query(
            "UPDATE job_attempts SET \
                node_id = ?2, lease_id = ?3, fence = ?4, phase = ?5, \
                start_checkpoint_uri = ?6, end_checkpoint_uri = ?7, last_event_at = ?8 \
             WHERE id = ?1",
        )
        .bind(attempt.id.as_str())
        .bind(attempt.node.as_ref().map(NodeId::as_str))
        .bind(attempt.lease.as_ref().map(LeaseId::as_str))
        .bind(attempt.fence.map(fence_to_i64).transpose()?)
        .bind(attempt_phase_str(attempt.phase))
        .bind(attempt.start_checkpoint.as_ref().map(CheckpointUri::as_str))
        .bind(attempt.end_checkpoint.as_ref().map(CheckpointUri::as_str))
        .bind(attempt.last_event_at.map(Timestamp::as_millis))
        .execute(&self.pool)
        .await
        .map_err(map_db)?
        .rows_affected();
        Self::require_affected(affected)
    }

    // ---------------------------------------------------------------- leases

    async fn commit_lease(&self, lease: &Lease) -> Result<LeaseCommit, StoreError> {
        let new_fence = fence_to_i64(lease.fence)?;
        let mut tx = self.pool.begin().await.map_err(map_db)?;

        // The live claim on this node (its greatest active fence), if any.
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT fence FROM leases \
             WHERE node_id = ?1 AND state = 'active' \
             ORDER BY fence DESC LIMIT 1",
        )
        .bind(lease.node.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;

        // The compare-and-set: a live claim with an equal-or-greater fence blocks
        // this commit — the split-brain guard (agent-protocol §5).
        if let Some(current) = current
            && current >= new_fence
        {
            tx.rollback().await.map_err(map_db)?;
            return Ok(LeaseCommit::Superseded {
                current_fence: fence_from_i64(current)?,
            });
        }

        sqlx::query(
            "INSERT INTO leases (id, attempt_id, node_id, fence, granted_at, expires_at, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(lease.id.as_str())
        .bind(lease.attempt.as_str())
        .bind(lease.node.as_str())
        .bind(new_fence)
        .bind(lease.granted_at.as_millis())
        .bind(lease.expires_at.as_millis())
        .bind(lease_state_str(lease.state))
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(LeaseCommit::Committed)
    }

    async fn get_lease(&self, id: &LeaseId) -> Result<Option<Lease>, StoreError> {
        let row = sqlx::query("SELECT * FROM leases WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db)?;
        row.as_ref().map(row_to_lease).transpose()
    }

    async fn active_lease_for_node(&self, node: &NodeId) -> Result<Option<Lease>, StoreError> {
        let row = sqlx::query(
            "SELECT * FROM leases \
             WHERE node_id = ?1 AND state = 'active' \
             ORDER BY fence DESC LIMIT 1",
        )
        .bind(node.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db)?;
        row.as_ref().map(row_to_lease).transpose()
    }

    async fn renew_lease(
        &self,
        id: &LeaseId,
        now: Timestamp,
        new_expiry: Timestamp,
    ) -> Result<bool, StoreError> {
        let Some(mut lease) = self.get_lease(id).await? else {
            return Err(StoreError::NotFound);
        };
        // The renewal rule lives in loom-core; the store persists its verdict.
        if !lease.renew(now, new_expiry) {
            return Ok(false);
        }
        sqlx::query("UPDATE leases SET expires_at = ?1 WHERE id = ?2")
            .bind(new_expiry.as_millis())
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_db)?;
        Ok(true)
    }

    async fn release_lease(&self, id: &LeaseId) -> Result<(), StoreError> {
        let affected = sqlx::query("UPDATE leases SET state = 'released' WHERE id = ?1")
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_db)?
            .rows_affected();
        Self::require_affected(affected)
    }

    // ----------------------------------------------------------------- usage

    async fn insert_usage(&self, record: &UsageRecord) -> Result<bool, StoreError> {
        // Idempotent on the (attempt_id, seq) primary key: a replay is ignored.
        let affected = sqlx::query(
            "INSERT OR IGNORE INTO usage_records (\
                attempt_id, node_id, host_id, fence, seq, window_start, window_end, \
                billable_secs, gpu_util_pct, validation\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(record.attempt.as_str())
        .bind(record.node.as_str())
        .bind(record.host.as_str())
        .bind(fence_to_i64(record.fence)?)
        .bind(u64_to_i64(record.seq, "seq")?)
        .bind(record.window_start.as_millis())
        .bind(record.window_end.as_millis())
        .bind(i64::from(record.billable_secs))
        .bind(record.gpu_util_pct.map(i64::from))
        .bind(usage_validation_str(record.validation))
        .execute(&self.pool)
        .await
        .map_err(map_db)?
        .rows_affected();
        Ok(affected == 1)
    }

    async fn list_usage_for_attempt(
        &self,
        attempt: &AttemptId,
    ) -> Result<Vec<UsageRecord>, StoreError> {
        let rows =
            sqlx::query("SELECT * FROM usage_records WHERE attempt_id = ?1 ORDER BY seq ASC")
                .bind(attempt.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(map_db)?;
        rows.iter().map(row_to_usage).collect()
    }

    // ---------------------------------------------------------------- outbox

    async fn enqueue_outbox(&self, event: &NewOutboxEvent) -> Result<OutboxId, StoreError> {
        let result =
            sqlx::query("INSERT INTO outbox (topic, payload, created_at) VALUES (?1, ?2, ?3)")
                .bind(event.topic.as_str())
                .bind(event.payload.as_str())
                .bind(event.created_at.as_millis())
                .execute(&self.pool)
                .await
                .map_err(map_db)?;
        Ok(OutboxId(result.last_insert_rowid()))
    }

    async fn list_unsent_outbox(&self, limit: u32) -> Result<Vec<OutboxEvent>, StoreError> {
        let rows =
            sqlx::query("SELECT * FROM outbox WHERE sent_at IS NULL ORDER BY id ASC LIMIT ?1")
                .bind(i64::from(limit))
                .fetch_all(&self.pool)
                .await
                .map_err(map_db)?;
        rows.iter().map(row_to_outbox).collect()
    }

    async fn mark_outbox_sent(&self, id: OutboxId, sent_at: Timestamp) -> Result<(), StoreError> {
        let affected = sqlx::query("UPDATE outbox SET sent_at = ?1 WHERE id = ?2")
            .bind(sent_at.as_millis())
            .bind(id.0)
            .execute(&self.pool)
            .await
            .map_err(map_db)?
            .rows_affected();
        Self::require_affected(affected)
    }

    // ----------------------------------------------------------- idempotency

    async fn put_idempotency(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyOutcome, StoreError> {
        // One atomic INSERT OR IGNORE decides stored-vs-existing; an existing key
        // is then classified as a replay or a body mismatch.
        let affected = sqlx::query(
            "INSERT OR IGNORE INTO idempotency_keys (\
                account_id, idempotency_key, request_hash, response_status, response_body, \
                created_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(record.account.as_str())
        .bind(record.key.as_str())
        .bind(record.request_hash.as_str())
        .bind(i64::from(record.response_status))
        .bind(record.response_body.as_str())
        .bind(record.created_at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(map_db)?
        .rows_affected();

        if affected == 1 {
            return Ok(IdempotencyOutcome::Stored);
        }
        let existing = self
            .get_idempotency(&record.account, &record.key)
            .await?
            .ok_or_else(|| StoreError::Backend("idempotency row vanished".to_owned()))?;
        if existing.request_hash == record.request_hash {
            Ok(IdempotencyOutcome::Replayed(existing))
        } else {
            Ok(IdempotencyOutcome::Mismatch)
        }
    }

    async fn get_idempotency(
        &self,
        account: &AccountId,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT * FROM idempotency_keys WHERE account_id = ?1 AND idempotency_key = ?2",
        )
        .bind(account.as_str())
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db)?;
        row.as_ref().map(row_to_idempotency).transpose()
    }

    // --------------------------------------------------------- hosts / gpus

    async fn insert_host(&self, host: &Host) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO hosts (id, account_id, agent_pubkey, status, enrolled_at, last_seen_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(host.id.as_str())
        .bind(host.account.as_str())
        .bind(host.agent_pubkey.as_slice())
        .bind(host_status_str(host.status))
        .bind(host.enrolled_at.as_millis())
        .bind(host.last_seen_at.map(Timestamp::as_millis))
        .execute(&self.pool)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_host(&self, id: &HostId) -> Result<Option<Host>, StoreError> {
        let row = sqlx::query("SELECT * FROM hosts WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db)?;
        row.as_ref().map(row_to_host).transpose()
    }

    async fn set_host_status(&self, id: &HostId, status: HostStatus) -> Result<(), StoreError> {
        let affected = sqlx::query("UPDATE hosts SET status = ?1 WHERE id = ?2")
            .bind(host_status_str(status))
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_db)?
            .rows_affected();
        Self::require_affected(affected)
    }

    async fn insert_gpu(&self, gpu: &Gpu) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO gpus (id, host_id, model, memory_mb, fingerprint) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(gpu.id.as_str())
        .bind(gpu.host.as_str())
        .bind(gpu.model.as_str())
        .bind(u64_to_i64(gpu.memory_mb, "gpu.memory_mb")?)
        .bind(gpu.fingerprint.as_deref())
        .execute(&self.pool)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_gpus_for_host(&self, host: &HostId) -> Result<Vec<Gpu>, StoreError> {
        let rows = sqlx::query("SELECT * FROM gpus WHERE host_id = ?1 ORDER BY id ASC")
            .bind(host.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(map_db)?;
        rows.iter().map(row_to_gpu).collect()
    }

    // ----------------------------------------------------------------- nodes

    async fn upsert_node(&self, node: &Node) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT OR REPLACE INTO nodes (\
                id, host_id, status, gpu_model, memory_kind, memory_mb, backends, \
                driver_version, cuda_version, isolation_tier, region, reliability_milli, \
                price_per_sec_micro_usd, last_heartbeat_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )
        .bind(node.id.as_str())
        .bind(node.host.as_str())
        .bind(node_status_str(node.status))
        .bind(node.gpu_model.as_str())
        .bind(memory_kind_str(node.memory.kind))
        .bind(u64_to_i64(node.memory.size_mb(), "node.memory")?)
        .bind(backend_set_to_csv(node.backends))
        .bind(node.driver.to_string())
        .bind(node.cuda.map(|v| v.to_string()))
        .bind(isolation_str(node.isolation))
        .bind(node.region.as_str())
        .bind(i64::from(node.reliability_milli))
        .bind(node.price_per_sec_micro_usd)
        .bind(node.last_heartbeat_at.map(Timestamp::as_millis))
        .execute(&self.pool)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_node(&self, id: &NodeId) -> Result<Option<Node>, StoreError> {
        let row = sqlx::query("SELECT * FROM nodes WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db)?;
        row.as_ref().map(row_to_node).transpose()
    }

    async fn list_schedulable_nodes(&self) -> Result<Vec<Node>, StoreError> {
        let rows = sqlx::query("SELECT * FROM nodes WHERE status = 'available' ORDER BY id ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(map_db)?;
        rows.iter().map(row_to_node).collect()
    }

    async fn set_node_status(&self, id: &NodeId, status: NodeStatus) -> Result<(), StoreError> {
        let affected = sqlx::query("UPDATE nodes SET status = ?1 WHERE id = ?2")
            .bind(node_status_str(status))
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_db)?
            .rows_affected();
        Self::require_affected(affected)
    }

    async fn record_node_heartbeat(&self, id: &NodeId, at: Timestamp) -> Result<(), StoreError> {
        let affected = sqlx::query("UPDATE nodes SET last_heartbeat_at = ?1 WHERE id = ?2")
            .bind(at.as_millis())
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(map_db)?
            .rows_affected();
        Self::require_affected(affected)
    }

    // ------------------------------------------------------ accounts / keys

    async fn insert_account(&self, account: &Account) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO accounts (id, name, created_at) VALUES (?1, ?2, ?3)")
            .bind(account.id.as_str())
            .bind(account.name.as_str())
            .bind(account.created_at.as_millis())
            .execute(&self.pool)
            .await
            .map_err(map_db)?;
        Ok(())
    }

    async fn get_account(&self, id: &AccountId) -> Result<Option<Account>, StoreError> {
        let row = sqlx::query("SELECT * FROM accounts WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db)?;
        row.as_ref().map(row_to_account).transpose()
    }

    async fn insert_api_key(&self, key: &ApiKey) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO api_keys (id, account_id, key_hash, label, created_at, revoked) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(key.id.as_str())
        .bind(key.account.as_str())
        .bind(key.key_hash.as_str())
        .bind(key.label.as_str())
        .bind(key.created_at.as_millis())
        .bind(i64::from(key.revoked))
        .execute(&self.pool)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn api_key_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, StoreError> {
        let row = sqlx::query("SELECT * FROM api_keys WHERE key_hash = ?1 AND revoked = 0")
            .bind(key_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db)?;
        row.as_ref().map(row_to_api_key).transpose()
    }

    async fn revoke_api_key(&self, id: &str) -> Result<(), StoreError> {
        let affected = sqlx::query("UPDATE api_keys SET revoked = 1 WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_db)?
            .rows_affected();
        Self::require_affected(affected)
    }
}

// ========================================================= row → aggregate

fn row_to_job(row: &SqliteRow) -> Result<Job, StoreError> {
    let claim = ResourceClaim {
        min_memory_mb: i64_to_u64(row.try_get("claim_min_memory_mb").map_err(map_db)?)?,
        gpu_model: row
            .try_get::<Option<String>, _>("claim_gpu_model")
            .map_err(map_db)?,
        min_driver: opt_version(row, "claim_min_driver")?,
        min_cuda: opt_version(row, "claim_min_cuda")?,
        min_isolation: isolation_from_str(&col_str(row, "claim_min_isolation")?)?,
        min_reliability_milli: i64_to_u16(
            row.try_get("claim_min_reliability_milli").map_err(map_db)?,
        )?,
        region_pref: row
            .try_get::<Option<String>, _>("claim_region_pref")
            .map_err(map_db)?,
        max_price_per_sec_micro_usd: row.try_get("claim_max_price_micro_usd").map_err(map_db)?,
        backend: backend_selector_from_str(&col_str(row, "claim_backend_selector")?)?,
        supported_backends: backend_set_from_csv(&col_str(row, "claim_supported_backends")?)?,
    };
    let spec = JobSpec {
        image_ref: col_str(row, "image_ref")?,
        claim,
        workload_class: workload_class_from_str(&col_str(row, "workload_class")?)?,
        priority: i64_to_i32(row.try_get("priority").map_err(map_db)?)?,
        checkpoint_uri: row
            .try_get::<Option<String>, _>("checkpoint_uri")
            .map_err(map_db)?
            .map(CheckpointUri::new),
    };
    Ok(Job {
        id: JobId::new(col_str(row, "id")?),
        account: AccountId::new(col_str(row, "account_id")?),
        spec,
        state: job_state_from_str(&col_str(row, "state")?)?,
        submitted_at: Timestamp::from_millis(row.try_get("submitted_at").map_err(map_db)?),
        terminal_at: opt_ts(row, "terminal_at")?,
    })
}

fn row_to_attempt(row: &SqliteRow) -> Result<Attempt, StoreError> {
    Ok(Attempt {
        id: AttemptId::new(col_str(row, "id")?),
        job: JobId::new(col_str(row, "job_id")?),
        attempt_no: AttemptNo::from_persisted(i64_to_u32(
            row.try_get("attempt_no").map_err(map_db)?,
        )?),
        node: opt_str(row, "node_id")?.map(NodeId::new),
        lease: opt_str(row, "lease_id")?.map(LeaseId::new),
        fence: row
            .try_get::<Option<i64>, _>("fence")
            .map_err(map_db)?
            .map(fence_from_i64)
            .transpose()?,
        phase: attempt_phase_from_str(&col_str(row, "phase")?)?,
        start_checkpoint: opt_str(row, "start_checkpoint_uri")?.map(CheckpointUri::new),
        end_checkpoint: opt_str(row, "end_checkpoint_uri")?.map(CheckpointUri::new),
        last_event_at: opt_ts(row, "last_event_at")?,
    })
}

fn row_to_lease(row: &SqliteRow) -> Result<Lease, StoreError> {
    Ok(Lease {
        id: LeaseId::new(col_str(row, "id")?),
        attempt: AttemptId::new(col_str(row, "attempt_id")?),
        node: NodeId::new(col_str(row, "node_id")?),
        fence: fence_from_i64(row.try_get("fence").map_err(map_db)?)?,
        granted_at: Timestamp::from_millis(row.try_get("granted_at").map_err(map_db)?),
        expires_at: Timestamp::from_millis(row.try_get("expires_at").map_err(map_db)?),
        state: lease_state_from_str(&col_str(row, "state")?)?,
    })
}

fn row_to_usage(row: &SqliteRow) -> Result<UsageRecord, StoreError> {
    Ok(UsageRecord {
        attempt: AttemptId::new(col_str(row, "attempt_id")?),
        node: NodeId::new(col_str(row, "node_id")?),
        host: HostId::new(col_str(row, "host_id")?),
        fence: fence_from_i64(row.try_get("fence").map_err(map_db)?)?,
        seq: i64_to_u64(row.try_get("seq").map_err(map_db)?)?,
        window_start: Timestamp::from_millis(row.try_get("window_start").map_err(map_db)?),
        window_end: Timestamp::from_millis(row.try_get("window_end").map_err(map_db)?),
        billable_secs: i64_to_u32(row.try_get("billable_secs").map_err(map_db)?)?,
        gpu_util_pct: row
            .try_get::<Option<i64>, _>("gpu_util_pct")
            .map_err(map_db)?
            .map(i64_to_u8)
            .transpose()?,
        validation: usage_validation_from_str(&col_str(row, "validation")?)?,
    })
}

fn row_to_outbox(row: &SqliteRow) -> Result<OutboxEvent, StoreError> {
    Ok(OutboxEvent {
        id: OutboxId(row.try_get("id").map_err(map_db)?),
        topic: col_str(row, "topic")?,
        payload: col_str(row, "payload")?,
        created_at: Timestamp::from_millis(row.try_get("created_at").map_err(map_db)?),
        sent_at: opt_ts(row, "sent_at")?,
    })
}

fn row_to_idempotency(row: &SqliteRow) -> Result<IdempotencyRecord, StoreError> {
    Ok(IdempotencyRecord {
        account: AccountId::new(col_str(row, "account_id")?),
        key: col_str(row, "idempotency_key")?,
        request_hash: col_str(row, "request_hash")?,
        response_status: i64_to_u16(row.try_get("response_status").map_err(map_db)?)?,
        response_body: col_str(row, "response_body")?,
        created_at: Timestamp::from_millis(row.try_get("created_at").map_err(map_db)?),
    })
}

fn row_to_host(row: &SqliteRow) -> Result<Host, StoreError> {
    Ok(Host {
        id: HostId::new(col_str(row, "id")?),
        account: AccountId::new(col_str(row, "account_id")?),
        agent_pubkey: row.try_get("agent_pubkey").map_err(map_db)?,
        status: host_status_from_str(&col_str(row, "status")?)?,
        enrolled_at: Timestamp::from_millis(row.try_get("enrolled_at").map_err(map_db)?),
        last_seen_at: opt_ts(row, "last_seen_at")?,
    })
}

fn row_to_gpu(row: &SqliteRow) -> Result<Gpu, StoreError> {
    Ok(Gpu {
        id: GpuId::new(col_str(row, "id")?),
        host: HostId::new(col_str(row, "host_id")?),
        model: col_str(row, "model")?,
        memory_mb: i64_to_u64(row.try_get("memory_mb").map_err(map_db)?)?,
        fingerprint: opt_str(row, "fingerprint")?,
    })
}

fn row_to_node(row: &SqliteRow) -> Result<Node, StoreError> {
    Ok(Node {
        id: NodeId::new(col_str(row, "id")?),
        host: HostId::new(col_str(row, "host_id")?),
        status: node_status_from_str(&col_str(row, "status")?)?,
        gpu_model: col_str(row, "gpu_model")?,
        memory: MemoryModel::new(
            memory_kind_from_str(&col_str(row, "memory_kind")?)?,
            i64_to_u64(row.try_get("memory_mb").map_err(map_db)?)?,
        ),
        backends: backend_set_from_csv(&col_str(row, "backends")?)?,
        driver: version_from_str(&col_str(row, "driver_version")?)?,
        cuda: opt_version(row, "cuda_version")?,
        isolation: isolation_from_str(&col_str(row, "isolation_tier")?)?,
        region: col_str(row, "region")?,
        reliability_milli: i64_to_u16(row.try_get("reliability_milli").map_err(map_db)?)?,
        price_per_sec_micro_usd: row.try_get("price_per_sec_micro_usd").map_err(map_db)?,
        last_heartbeat_at: opt_ts(row, "last_heartbeat_at")?,
    })
}

fn row_to_account(row: &SqliteRow) -> Result<Account, StoreError> {
    Ok(Account {
        id: AccountId::new(col_str(row, "id")?),
        name: col_str(row, "name")?,
        created_at: Timestamp::from_millis(row.try_get("created_at").map_err(map_db)?),
    })
}

fn row_to_api_key(row: &SqliteRow) -> Result<ApiKey, StoreError> {
    Ok(ApiKey {
        id: col_str(row, "id")?,
        account: AccountId::new(col_str(row, "account_id")?),
        key_hash: col_str(row, "key_hash")?,
        label: col_str(row, "label")?,
        created_at: Timestamp::from_millis(row.try_get("created_at").map_err(map_db)?),
        revoked: row.try_get::<i64, _>("revoked").map_err(map_db)? != 0,
    })
}

// ============================================================ column helpers

fn col_str(row: &SqliteRow, col: &str) -> Result<String, StoreError> {
    row.try_get::<String, _>(col).map_err(map_db)
}

fn opt_str(row: &SqliteRow, col: &str) -> Result<Option<String>, StoreError> {
    row.try_get::<Option<String>, _>(col).map_err(map_db)
}

fn opt_ts(row: &SqliteRow, col: &str) -> Result<Option<Timestamp>, StoreError> {
    Ok(row
        .try_get::<Option<i64>, _>(col)
        .map_err(map_db)?
        .map(Timestamp::from_millis))
}

fn opt_version(row: &SqliteRow, col: &str) -> Result<Option<Version>, StoreError> {
    opt_str(row, col)?.map(|s| version_from_str(&s)).transpose()
}

// ============================================================ numeric casts

fn u64_to_i64(value: u64, what: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|e| StoreError::Backend(format!("{what} range: {e}")))
}

fn i64_to_u64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|e| StoreError::Backend(format!("u64 range: {e}")))
}

fn i64_to_u32(value: i64) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|e| StoreError::Backend(format!("u32 range: {e}")))
}

fn i64_to_u16(value: i64) -> Result<u16, StoreError> {
    u16::try_from(value).map_err(|e| StoreError::Backend(format!("u16 range: {e}")))
}

fn i64_to_u8(value: i64) -> Result<u8, StoreError> {
    u8::try_from(value).map_err(|e| StoreError::Backend(format!("u8 range: {e}")))
}

fn i64_to_i32(value: i64) -> Result<i32, StoreError> {
    i32::try_from(value).map_err(|e| StoreError::Backend(format!("i32 range: {e}")))
}

// ============================================================ enum ↔ text

fn job_state_str(state: JobState) -> &'static str {
    match state {
        JobState::Submitted => "submitted",
        JobState::Queued => "queued",
        JobState::Scheduled => "scheduled",
        JobState::Dispatched => "dispatched",
        JobState::Preparing => "preparing",
        JobState::Running => "running",
        JobState::Checkpointing => "checkpointing",
        JobState::Requeued => "requeued",
        JobState::Preempted => "preempted",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn job_state_from_str(s: &str) -> Result<JobState, StoreError> {
    Ok(match s {
        "submitted" => JobState::Submitted,
        "queued" => JobState::Queued,
        "scheduled" => JobState::Scheduled,
        "dispatched" => JobState::Dispatched,
        "preparing" => JobState::Preparing,
        "running" => JobState::Running,
        "checkpointing" => JobState::Checkpointing,
        "requeued" => JobState::Requeued,
        "preempted" => JobState::Preempted,
        "succeeded" => JobState::Succeeded,
        "failed" => JobState::Failed,
        "cancelled" => JobState::Cancelled,
        other => return Err(unknown("job state", other)),
    })
}

fn attempt_phase_str(phase: AttemptPhase) -> &'static str {
    match phase {
        AttemptPhase::Scheduled => "scheduled",
        AttemptPhase::Dispatched => "dispatched",
        AttemptPhase::Preparing => "preparing",
        AttemptPhase::Running => "running",
        AttemptPhase::Checkpointing => "checkpointing",
        AttemptPhase::Succeeded => "succeeded",
        AttemptPhase::Failed => "failed",
        AttemptPhase::Lost => "lost",
        AttemptPhase::Preempted => "preempted",
        AttemptPhase::Cancelled => "cancelled",
    }
}

fn attempt_phase_from_str(s: &str) -> Result<AttemptPhase, StoreError> {
    Ok(match s {
        "scheduled" => AttemptPhase::Scheduled,
        "dispatched" => AttemptPhase::Dispatched,
        "preparing" => AttemptPhase::Preparing,
        "running" => AttemptPhase::Running,
        "checkpointing" => AttemptPhase::Checkpointing,
        "succeeded" => AttemptPhase::Succeeded,
        "failed" => AttemptPhase::Failed,
        "lost" => AttemptPhase::Lost,
        "preempted" => AttemptPhase::Preempted,
        "cancelled" => AttemptPhase::Cancelled,
        other => return Err(unknown("attempt phase", other)),
    })
}

fn lease_state_str(state: LeaseState) -> &'static str {
    match state {
        LeaseState::Active => "active",
        LeaseState::Expired => "expired",
        LeaseState::Released => "released",
    }
}

fn lease_state_from_str(s: &str) -> Result<LeaseState, StoreError> {
    Ok(match s {
        "active" => LeaseState::Active,
        "expired" => LeaseState::Expired,
        "released" => LeaseState::Released,
        other => return Err(unknown("lease state", other)),
    })
}

fn node_status_str(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Offline => "offline",
        NodeStatus::Available => "available",
        NodeStatus::Leased => "leased",
        NodeStatus::Draining => "draining",
        NodeStatus::OwnerEjected => "owner_ejected",
    }
}

fn node_status_from_str(s: &str) -> Result<NodeStatus, StoreError> {
    Ok(match s {
        "offline" => NodeStatus::Offline,
        "available" => NodeStatus::Available,
        "leased" => NodeStatus::Leased,
        "draining" => NodeStatus::Draining,
        "owner_ejected" => NodeStatus::OwnerEjected,
        other => return Err(unknown("node status", other)),
    })
}

fn host_status_str(status: HostStatus) -> &'static str {
    match status {
        HostStatus::Pending => "pending",
        HostStatus::Enrolled => "enrolled",
        HostStatus::Revoked => "revoked",
    }
}

fn host_status_from_str(s: &str) -> Result<HostStatus, StoreError> {
    Ok(match s {
        "pending" => HostStatus::Pending,
        "enrolled" => HostStatus::Enrolled,
        "revoked" => HostStatus::Revoked,
        other => return Err(unknown("host status", other)),
    })
}

fn usage_validation_str(validation: UsageValidation) -> &'static str {
    match validation {
        UsageValidation::Pending => "pending",
        UsageValidation::Valid => "valid",
        UsageValidation::Implausible => "implausible",
        UsageValidation::Disputed => "disputed",
    }
}

fn usage_validation_from_str(s: &str) -> Result<UsageValidation, StoreError> {
    Ok(match s {
        "pending" => UsageValidation::Pending,
        "valid" => UsageValidation::Valid,
        "implausible" => UsageValidation::Implausible,
        "disputed" => UsageValidation::Disputed,
        other => return Err(unknown("usage validation", other)),
    })
}

fn workload_class_str(class: WorkloadClass) -> &'static str {
    match class {
        WorkloadClass::Batch => "batch",
        WorkloadClass::Serving => "serving",
    }
}

fn workload_class_from_str(s: &str) -> Result<WorkloadClass, StoreError> {
    Ok(match s {
        "batch" => WorkloadClass::Batch,
        "serving" => WorkloadClass::Serving,
        other => return Err(unknown("workload class", other)),
    })
}

fn memory_kind_str(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Unified => "unified",
        MemoryKind::Discrete => "discrete",
    }
}

fn memory_kind_from_str(s: &str) -> Result<MemoryKind, StoreError> {
    Ok(match s {
        "unified" => MemoryKind::Unified,
        "discrete" => MemoryKind::Discrete,
        other => return Err(unknown("memory kind", other)),
    })
}

fn isolation_str(tier: IsolationTier) -> &'static str {
    match tier {
        IsolationTier::B => "B",
        IsolationTier::A => "A",
        IsolationTier::C => "C",
    }
}

fn isolation_from_str(s: &str) -> Result<IsolationTier, StoreError> {
    Ok(match s {
        "B" => IsolationTier::B,
        "A" => IsolationTier::A,
        "C" => IsolationTier::C,
        other => return Err(unknown("isolation tier", other)),
    })
}

fn backend_str(backend: Backend) -> &'static str {
    match backend {
        Backend::Mlx => "mlx",
        Backend::Cuda => "cuda",
        Backend::Cpu => "cpu",
        Backend::Rocm => "rocm",
    }
}

fn backend_from_str(s: &str) -> Result<Backend, StoreError> {
    Ok(match s {
        "mlx" => Backend::Mlx,
        "cuda" => Backend::Cuda,
        "cpu" => Backend::Cpu,
        "rocm" => Backend::Rocm,
        other => return Err(unknown("backend", other)),
    })
}

fn backend_selector_str(selector: BackendSelector) -> String {
    match selector {
        BackendSelector::Auto => "auto".to_owned(),
        BackendSelector::Only(backend) => format!("only:{}", backend_str(backend)),
    }
}

fn backend_selector_from_str(s: &str) -> Result<BackendSelector, StoreError> {
    if s == "auto" {
        return Ok(BackendSelector::Auto);
    }
    if let Some(rest) = s.strip_prefix("only:") {
        return Ok(BackendSelector::Only(backend_from_str(rest)?));
    }
    Err(unknown("backend selector", s))
}

fn backend_set_to_csv(set: BackendSet) -> String {
    set.iter().map(backend_str).collect::<Vec<_>>().join(",")
}

fn backend_set_from_csv(csv: &str) -> Result<BackendSet, StoreError> {
    csv.split(',')
        .filter(|part| !part.is_empty())
        .map(backend_from_str)
        .collect()
}

fn version_from_str(s: &str) -> Result<Version, StoreError> {
    let mut parts = s.split('.');
    let major = parse_u32_part(parts.next(), s)?;
    let minor = parse_u32_part(parts.next(), s)?;
    let patch = parse_u32_part(parts.next(), s)?;
    if parts.next().is_some() {
        return Err(unknown("version", s));
    }
    Ok(Version::new(major, minor, patch))
}

fn parse_u32_part(part: Option<&str>, whole: &str) -> Result<u32, StoreError> {
    part.ok_or_else(|| unknown("version", whole))?
        .parse::<u32>()
        .map_err(|e| StoreError::Backend(format!("bad version {whole:?}: {e}")))
}

fn unknown(kind: &str, value: &str) -> StoreError {
    StoreError::Backend(format!("unknown {kind} in database: {value:?}"))
}
