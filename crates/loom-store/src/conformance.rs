// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The one shared conformance suite every [`Store`] backend must pass.
//!
//! `loom-store` ships exactly two backends — the in-memory
//! [`FakeStore`](crate::FakeStore) and the file-backed `SqliteStore` — and this
//! suite is the single contract both are held to. A fake that diverges from the
//! real store fails here; that is the whole point (workspace-setup.md §6, the
//! Mirror lesson). The suite is generic over [`Store`] and takes a *factory* so
//! each scenario runs against a pristine store — an empty `FakeStore` or a fresh
//! file-backed WAL database.
//!
//! The load-bearing scenario is [`leases_fencing_cas`]: it drives the
//! persistence-layer split-brain guard — a superseded (lower-fence) writer can
//! never claim a node a greater fence already holds.

// A conformance harness asserts; a failed store contract *is* a panic, and every
// `expect`/`assert` here is that contract. Documenting panics per scenario would
// be noise.
#![allow(clippy::missing_panics_doc)]

use core::future::Future;

use loom_core::{
    AccountId, Attempt, AttemptId, Backend, BackendSet, GpuId, HostId, IsolationTier, Job, JobId,
    JobSpec, JobState, LeaseBook, MemoryKind, MemoryModel, Node, NodeId, NodeStatus, ResourceClaim,
    Timestamp, UsageRecord, UsageValidation, Version, WorkloadClass,
};

use crate::records::{
    Account as AccountRow, ApiKey, Gpu, Host, HostStatus, IdempotencyOutcome, IdempotencyRecord,
    JobQuery, LeaseCommit, NewOutboxEvent,
};
use crate::store::Store;

/// Runs every conformance scenario, each against a freshly made store.
///
/// `make` yields a pristine [`Store`]: an empty [`FakeStore`](crate::FakeStore)
/// or a brand-new file-backed WAL database. Awaiting each scenario in turn keeps
/// list assertions free of cross-scenario contamination.
pub async fn run_all<S, F, Fut>(make: F)
where
    S: Store,
    F: Fn() -> Fut,
    Fut: Future<Output = S>,
{
    jobs_roundtrip(make().await).await;
    jobs_listing(make().await).await;
    attempts_lineage(make().await).await;
    leases_commit_and_read(make().await).await;
    leases_renew(make().await).await;
    leases_fencing_cas(make().await).await;
    usage_idempotent_ingest(make().await).await;
    outbox_drain(make().await).await;
    idempotency_window(make().await).await;
    enrollment_hosts_gpus(make().await).await;
    nodes_scheduling(make().await).await;
    accounts_and_api_keys(make().await).await;
}

// --------------------------------------------------------------- fixtures

fn ts(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn job_fixture(id: &str, account: &str, state: JobState) -> Job {
    Job {
        id: JobId::new(id),
        account: AccountId::new(account),
        spec: JobSpec {
            image_ref: "sha256:image".to_owned(),
            claim: ResourceClaim::default(),
            workload_class: WorkloadClass::Batch,
            priority: 0,
            checkpoint_uri: None,
        },
        state,
        submitted_at: ts(1_000),
        terminal_at: None,
    }
}

fn node_fixture(id: &str, host: &str, status: NodeStatus) -> Node {
    Node {
        id: NodeId::new(id),
        host: HostId::new(host),
        status,
        gpu_model: "M3 Max".to_owned(),
        memory: MemoryModel::new(MemoryKind::Unified, 48_000),
        backends: BackendSet::from_backends(&[Backend::Mlx, Backend::Cpu]),
        driver: Version::new(1, 0, 0),
        cuda: None,
        isolation: IsolationTier::B,
        region: "local".to_owned(),
        reliability_milli: 900,
        price_per_sec_micro_usd: 0,
        last_heartbeat_at: None,
    }
}

// ------------------------------------------------------------------- jobs

async fn jobs_roundtrip<S: Store>(store: S) {
    let job = job_fixture("job-1", "acct-1", JobState::Submitted);
    store.insert_job(&job).await.expect("insert job");
    assert_eq!(
        store.get_job(&job.id).await.expect("get job"),
        Some(job.clone()),
        "a stored job reads back identical"
    );
    assert_eq!(
        store
            .get_job(&JobId::new("missing"))
            .await
            .expect("get miss"),
        None,
        "a missing job reads back as None"
    );

    // Re-inserting the same id is a conflict, not a silent overwrite.
    let dup = store.insert_job(&job).await;
    assert!(matches!(dup, Err(crate::StoreError::Conflict(_))));

    // State advances, and a terminal transition stamps `terminal_at`.
    store
        .update_job_state(&job.id, JobState::Succeeded, Some(ts(9_000)))
        .await
        .expect("update state");
    let stored = store.get_job(&job.id).await.expect("get").expect("present");
    assert_eq!(stored.state, JobState::Succeeded);
    assert_eq!(stored.terminal_at, Some(ts(9_000)));

    // Updating a missing job is a not-found, never a silent insert.
    assert!(matches!(
        store
            .update_job_state(&JobId::new("ghost"), JobState::Failed, None)
            .await,
        Err(crate::StoreError::NotFound)
    ));
}

async fn jobs_listing<S: Store>(store: S) {
    store
        .insert_job(&job_fixture("job-a", "acct-1", JobState::Queued))
        .await
        .expect("a");
    store
        .insert_job(&job_fixture("job-b", "acct-1", JobState::Running))
        .await
        .expect("b");
    store
        .insert_job(&job_fixture("job-c", "acct-2", JobState::Queued))
        .await
        .expect("c");

    let all = store.list_jobs(&JobQuery::default()).await.expect("all");
    assert_eq!(all.len(), 3, "an empty query lists every job");

    let by_account = store
        .list_jobs(&JobQuery {
            account: Some(AccountId::new("acct-1")),
            state: None,
        })
        .await
        .expect("by account");
    assert_eq!(by_account.len(), 2, "account filter narrows the set");
    assert!(
        by_account
            .iter()
            .all(|j| j.account == AccountId::new("acct-1"))
    );

    let queued_acct1 = store
        .list_jobs(&JobQuery {
            account: Some(AccountId::new("acct-1")),
            state: Some(JobState::Queued),
        })
        .await
        .expect("by both");
    assert_eq!(queued_acct1.len(), 1);
    assert_eq!(queued_acct1[0].id, JobId::new("job-a"));
}

// --------------------------------------------------------------- attempts

async fn attempts_lineage<S: Store>(store: S) {
    let job = JobId::new("job-1");
    let mut book = LeaseBook::new();
    let g1 = book
        .grant_initial(
            job.clone(),
            AttemptId::new("at-1"),
            NodeId::new("node-1"),
            ts(0),
            ts(30_000),
        )
        .expect("grant 1");
    let a1 = Attempt::scheduled(
        AttemptId::new("at-1"),
        job.clone(),
        g1.attempt_no,
        NodeId::new("node-1"),
        g1.lease.id.clone(),
        g1.fence,
        None,
    );
    store.insert_attempt(&a1).await.expect("insert a1");
    assert_eq!(
        store.get_attempt(&a1.id).await.expect("get a1"),
        Some(a1.clone())
    );

    // A second attempt with the same `(job, attempt_no)` is rejected even though
    // its id differs — the lineage counter is unique per job.
    let dup = Attempt::scheduled(
        AttemptId::new("at-1-dup"),
        job.clone(),
        g1.attempt_no,
        NodeId::new("node-1"),
        g1.lease.id.clone(),
        g1.fence,
        None,
    );
    assert!(matches!(
        store.insert_attempt(&dup).await,
        Err(crate::StoreError::Conflict(_))
    ));

    // The requeued successor carries the next attempt number and a checkpoint.
    let g2 = book
        .requeue(
            job.clone(),
            AttemptId::new("at-2"),
            NodeId::new("node-2"),
            ts(0),
            ts(30_000),
            None,
        )
        .expect("requeue");
    let a2 = Attempt::scheduled(
        AttemptId::new("at-2"),
        job.clone(),
        g2.attempt_no,
        NodeId::new("node-2"),
        g2.lease.id.clone(),
        g2.fence,
        None,
    );
    store.insert_attempt(&a2).await.expect("insert a2");

    let lineage = store.list_attempts_for_job(&job).await.expect("lineage");
    assert_eq!(lineage.len(), 2, "both attempts of the lineage are listed");
    assert!(
        lineage[0].attempt_no < lineage[1].attempt_no,
        "attempts list in lineage order"
    );

    // Mutating an attempt in place (a phase advance) is persisted.
    let mut running = a1.clone();
    running.phase = loom_core::AttemptPhase::Running;
    store.update_attempt(&running).await.expect("update");
    let read = store.get_attempt(&a1.id).await.expect("get").expect("some");
    assert_eq!(read.phase, loom_core::AttemptPhase::Running);

    // Updating a never-inserted attempt is a not-found.
    let ghost = Attempt::scheduled(
        AttemptId::new("ghost"),
        job,
        g1.attempt_no,
        NodeId::new("node-1"),
        g1.lease.id.clone(),
        g1.fence,
        None,
    );
    assert!(matches!(
        store.update_attempt(&ghost).await,
        Err(crate::StoreError::NotFound)
    ));
}

// ----------------------------------------------------------------- leases

/// Grants an initial lease for `job` on `node` from a fresh book, returning the
/// book (for further requeues) and the grant.
fn grant_on(book: &mut LeaseBook, job: &str, attempt: &str, node: &str) -> loom_core::LeaseGrant {
    book.grant_initial(
        JobId::new(job),
        AttemptId::new(attempt),
        NodeId::new(node),
        ts(0),
        ts(30_000),
    )
    .expect("grant_initial")
}

async fn leases_commit_and_read<S: Store>(store: S) {
    let mut book = LeaseBook::new();
    let g = grant_on(&mut book, "job-1", "at-1", "node-1");
    assert_eq!(
        store.commit_lease(&g.lease).await.expect("commit"),
        LeaseCommit::Committed
    );
    assert_eq!(
        store.get_lease(&g.lease.id).await.expect("get"),
        Some(g.lease.clone())
    );
    assert_eq!(
        store
            .active_lease_for_node(&NodeId::new("node-1"))
            .await
            .expect("active"),
        Some(g.lease.clone()),
        "the committed lease holds its node"
    );
    assert_eq!(
        store
            .active_lease_for_node(&NodeId::new("node-2"))
            .await
            .expect("active other"),
        None
    );

    // Releasing frees the node: a fresh (greater-fence) lease may then claim it.
    store.release_lease(&g.lease.id).await.expect("release");
    assert_eq!(
        store
            .active_lease_for_node(&NodeId::new("node-1"))
            .await
            .expect("active after release"),
        None
    );
    let g2 = book
        .requeue(
            JobId::new("job-1"),
            AttemptId::new("at-2"),
            NodeId::new("node-1"),
            ts(0),
            ts(30_000),
            None,
        )
        .expect("requeue");
    assert!(g2.fence > g.fence);
    assert_eq!(
        store.commit_lease(&g2.lease).await.expect("commit 2"),
        LeaseCommit::Committed,
        "a freed node accepts a fresh lease"
    );

    // Releasing a missing lease is a not-found.
    assert!(matches!(
        store.release_lease(&loom_core::LeaseId::new("ghost")).await,
        Err(crate::StoreError::NotFound)
    ));
}

async fn leases_renew<S: Store>(store: S) {
    let mut book = LeaseBook::new();
    let g = grant_on(&mut book, "job-1", "at-1", "node-1");
    store.commit_lease(&g.lease).await.expect("commit");

    // A strictly-extending renewal on an active, unexpired lease succeeds.
    assert!(
        store
            .renew_lease(&g.lease.id, ts(10_000), ts(60_000))
            .await
            .expect("renew"),
        "a live lease extends"
    );
    let renewed = store
        .get_lease(&g.lease.id)
        .await
        .expect("get")
        .expect("some");
    assert_eq!(renewed.expires_at, ts(60_000));

    // A non-extending (shrinking) renewal is rejected, leaving the lease intact.
    assert!(
        !store
            .renew_lease(&g.lease.id, ts(10_000), ts(20_000))
            .await
            .expect("shrink"),
        "a shrinking renewal is refused"
    );

    // A renewal past expiry cannot revive a lapsed lease.
    assert!(
        !store
            .renew_lease(&g.lease.id, ts(60_000), ts(90_000))
            .await
            .expect("lapsed"),
        "a lapsed lease is not revived"
    );

    // Renewing a missing lease is a not-found.
    assert!(matches!(
        store
            .renew_lease(&loom_core::LeaseId::new("ghost"), ts(0), ts(1))
            .await,
        Err(crate::StoreError::NotFound)
    ));
}

/// The split-brain guard at the persistence layer (agent-protocol §5).
async fn leases_fencing_cas<S: Store>(store: S) {
    // One lineage, two grants on the same node: `g1` (fence f1) then a requeued
    // `g2` (fence f2 > f1). Neither is committed yet.
    let mut book = LeaseBook::new();
    let g1 = grant_on(&mut book, "job-1", "at-1", "node-1");
    let g2 = book
        .requeue(
            JobId::new("job-1"),
            AttemptId::new("at-2"),
            NodeId::new("node-1"),
            ts(0),
            ts(30_000),
            None,
        )
        .expect("requeue");
    assert!(
        g2.fence > g1.fence,
        "a requeue mints a strictly greater fence"
    );

    // The current authority (greater fence) claims the node.
    assert_eq!(
        store.commit_lease(&g2.lease).await.expect("commit g2"),
        LeaseCommit::Committed
    );

    // The superseded writer (lower fence) races in late and is fenced off: it
    // cannot claim a node the greater fence already holds. This is the CAS.
    let verdict = store.commit_lease(&g1.lease).await.expect("commit g1");
    assert_eq!(
        verdict,
        LeaseCommit::Superseded {
            current_fence: g2.fence,
        },
        "a stale (lower) fence is rejected with the current fence reported"
    );
    assert!(!verdict.is_committed());

    // The node is still held by the greater fence; the stale lease never landed.
    assert_eq!(
        store
            .active_lease_for_node(&NodeId::new("node-1"))
            .await
            .expect("active"),
        Some(g2.lease.clone())
    );
    assert_eq!(
        store.get_lease(&g1.lease.id).await.expect("get g1"),
        None,
        "the fenced-off lease was never written"
    );
}

// ----------------------------------------------------------------- usage

fn usage_fixture(attempt: &str, seq: u64, billable_secs: u32) -> UsageRecord {
    let mut book = LeaseBook::new();
    let g = book
        .grant_initial(
            JobId::new("job-usage"),
            AttemptId::new(attempt),
            NodeId::new("node-1"),
            ts(0),
            ts(30_000),
        )
        .expect("grant for usage fence");
    UsageRecord {
        attempt: AttemptId::new(attempt),
        node: NodeId::new("node-1"),
        host: HostId::new("host-1"),
        fence: g.fence,
        seq,
        window_start: ts(0),
        window_end: ts(10_000),
        billable_secs,
        gpu_util_pct: Some(85),
        validation: UsageValidation::Pending,
    }
}

async fn usage_idempotent_ingest<S: Store>(store: S) {
    let attempt = AttemptId::new("at-1");
    let r0 = usage_fixture("at-1", 0, 10);
    let r1 = usage_fixture("at-1", 1, 10);

    assert!(
        store.insert_usage(&r0).await.expect("ingest 0"),
        "first is new"
    );
    assert!(
        store.insert_usage(&r1).await.expect("ingest 1"),
        "second is new"
    );

    // A replay of `(attempt, seq)` is idempotent — stored once, reported as a
    // duplicate, never a second row.
    assert!(
        !store.insert_usage(&r0).await.expect("replay 0"),
        "a duplicate (attempt, seq) is a harmless no-op"
    );

    let records = store
        .list_usage_for_attempt(&attempt)
        .await
        .expect("list usage");
    assert_eq!(records.len(), 2, "the replay did not add a row");
    assert_eq!(records[0].seq, 0, "records list in seq order");
    assert_eq!(records[1].seq, 1);
}

// ---------------------------------------------------------------- outbox

async fn outbox_drain<S: Store>(store: S) {
    let e1 = NewOutboxEvent {
        topic: "job.scheduled".to_owned(),
        payload: r#"{"job":"job-1"}"#.to_owned(),
        created_at: ts(1_000),
    };
    let e2 = NewOutboxEvent {
        topic: "job.dispatched".to_owned(),
        payload: r#"{"job":"job-1"}"#.to_owned(),
        created_at: ts(2_000),
    };
    let id1 = store.enqueue_outbox(&e1).await.expect("enqueue 1");
    let id2 = store.enqueue_outbox(&e2).await.expect("enqueue 2");
    assert!(id1 < id2, "outbox ids are monotonic");

    let unsent = store.list_unsent_outbox(10).await.expect("unsent");
    assert_eq!(unsent.len(), 2, "both rows are pending");
    assert_eq!(unsent[0].id, id1, "unsent lists oldest first");
    assert_eq!(unsent[1].id, id2);

    // The limit bounds the batch.
    assert_eq!(
        store.list_unsent_outbox(1).await.expect("limited").len(),
        1,
        "the limit caps the drained batch"
    );

    // Marking one sent removes it from the pending set.
    store
        .mark_outbox_sent(id1, ts(3_000))
        .await
        .expect("mark sent");
    let remaining = store.list_unsent_outbox(10).await.expect("remaining");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, id2, "only the unsent row remains");

    // Marking a missing row sent is a not-found.
    assert!(matches!(
        store
            .mark_outbox_sent(crate::OutboxId(9_999), ts(4_000))
            .await,
        Err(crate::StoreError::NotFound)
    ));
}

// ----------------------------------------------------------- idempotency

async fn idempotency_window<S: Store>(store: S) {
    let account = AccountId::new("acct-1");
    let record = IdempotencyRecord {
        account: account.clone(),
        key: "key-1".to_owned(),
        request_hash: "hash-a".to_owned(),
        response_status: 201,
        response_body: r#"{"id":"job-1"}"#.to_owned(),
        created_at: ts(1_000),
    };

    // First use stores it and signals "execute".
    assert_eq!(
        store.put_idempotency(&record).await.expect("first put"),
        IdempotencyOutcome::Stored
    );
    assert_eq!(
        store.get_idempotency(&account, "key-1").await.expect("get"),
        Some(record.clone())
    );

    // Same key, same request → replay the stored response.
    match store.put_idempotency(&record).await.expect("replay") {
        IdempotencyOutcome::Replayed(stored) => assert_eq!(stored, record),
        other => panic!("expected replay, got {other:?}"),
    }

    // Same key, *different* request body → reuse mismatch (renter-api §1.3).
    let conflicting = IdempotencyRecord {
        request_hash: "hash-b".to_owned(),
        ..record.clone()
    };
    assert_eq!(
        store.put_idempotency(&conflicting).await.expect("mismatch"),
        IdempotencyOutcome::Mismatch
    );

    // An unknown key reads back as absent.
    assert_eq!(
        store
            .get_idempotency(&account, "missing")
            .await
            .expect("miss"),
        None
    );
}

// --------------------------------------------------------- hosts / gpus

async fn enrollment_hosts_gpus<S: Store>(store: S) {
    let account = AccountRow {
        id: AccountId::new("acct-1"),
        name: "founder".to_owned(),
        created_at: ts(0),
    };
    store.insert_account(&account).await.expect("account");

    let host = Host {
        id: HostId::new("host-1"),
        account: AccountId::new("acct-1"),
        agent_pubkey: vec![1, 2, 3, 4],
        status: HostStatus::Pending,
        enrolled_at: ts(1_000),
        last_seen_at: None,
    };
    store.insert_host(&host).await.expect("host");
    assert_eq!(
        store.get_host(&host.id).await.expect("get host"),
        Some(host.clone())
    );

    // Enrollment status advances.
    store
        .set_host_status(&host.id, HostStatus::Enrolled)
        .await
        .expect("enroll");
    assert_eq!(
        store
            .get_host(&host.id)
            .await
            .expect("get")
            .expect("some")
            .status,
        HostStatus::Enrolled
    );

    // Two GPUs under the host, listed deterministically.
    for (gid, model) in [("gpu-1", "M3 Max"), ("gpu-2", "M3 Max")] {
        store
            .insert_gpu(&Gpu {
                id: GpuId::new(gid),
                host: HostId::new("host-1"),
                model: model.to_owned(),
                memory_mb: 48_000,
                fingerprint: Some("fp".to_owned()),
            })
            .await
            .expect("gpu");
    }
    let gpus = store
        .list_gpus_for_host(&HostId::new("host-1"))
        .await
        .expect("list gpus");
    assert_eq!(gpus.len(), 2);
    assert_eq!(
        gpus[0].id,
        GpuId::new("gpu-1"),
        "gpus list deterministically"
    );

    // A duplicate host id is a conflict.
    assert!(matches!(
        store.insert_host(&host).await,
        Err(crate::StoreError::Conflict(_))
    ));

    // Setting status on a missing host is a not-found.
    assert!(matches!(
        store
            .set_host_status(&HostId::new("ghost"), HostStatus::Revoked)
            .await,
        Err(crate::StoreError::NotFound)
    ));
}

// ----------------------------------------------------------------- nodes

async fn nodes_scheduling<S: Store>(store: S) {
    let account = AccountRow {
        id: AccountId::new("acct-1"),
        name: "founder".to_owned(),
        created_at: ts(0),
    };
    store.insert_account(&account).await.expect("account");
    store
        .insert_host(&Host {
            id: HostId::new("host-1"),
            account: AccountId::new("acct-1"),
            agent_pubkey: vec![9],
            status: HostStatus::Enrolled,
            enrolled_at: ts(0),
            last_seen_at: None,
        })
        .await
        .expect("host");

    // Upsert is insert-or-replace: the second write wins.
    let mut node = node_fixture("node-1", "host-1", NodeStatus::Offline);
    store.upsert_node(&node).await.expect("insert node");
    node.status = NodeStatus::Available;
    store.upsert_node(&node).await.expect("replace node");
    assert_eq!(
        store
            .get_node(&node.id)
            .await
            .expect("get")
            .expect("some")
            .status,
        NodeStatus::Available,
        "upsert replaces the prior row"
    );

    // An offline node is added; only the available one is schedulable.
    store
        .upsert_node(&node_fixture("node-2", "host-1", NodeStatus::Offline))
        .await
        .expect("node-2");
    let schedulable = store.list_schedulable_nodes().await.expect("schedulable");
    assert_eq!(schedulable.len(), 1, "only Available nodes are schedulable");
    assert_eq!(schedulable[0].id, NodeId::new("node-1"));

    // Draining a node removes it from the schedulable set.
    store
        .set_node_status(&NodeId::new("node-1"), NodeStatus::Draining)
        .await
        .expect("drain");
    assert!(
        store
            .list_schedulable_nodes()
            .await
            .expect("after drain")
            .is_empty(),
        "a draining node is not schedulable"
    );

    // Heartbeats are recorded.
    store
        .record_node_heartbeat(&NodeId::new("node-1"), ts(5_000))
        .await
        .expect("heartbeat");
    assert_eq!(
        store
            .get_node(&NodeId::new("node-1"))
            .await
            .expect("get")
            .expect("some")
            .last_heartbeat_at,
        Some(ts(5_000))
    );

    // Status/heartbeat on a missing node is a not-found.
    assert!(matches!(
        store
            .set_node_status(&NodeId::new("ghost"), NodeStatus::Available)
            .await,
        Err(crate::StoreError::NotFound)
    ));
}

// ------------------------------------------------------ accounts / keys

async fn accounts_and_api_keys<S: Store>(store: S) {
    let account = AccountRow {
        id: AccountId::new("acct-1"),
        name: "founder".to_owned(),
        created_at: ts(0),
    };
    store.insert_account(&account).await.expect("account");
    assert_eq!(
        store.get_account(&account.id).await.expect("get account"),
        Some(account.clone())
    );
    assert!(
        matches!(
            store.insert_account(&account).await,
            Err(crate::StoreError::Conflict(_))
        ),
        "a duplicate account id is a conflict"
    );

    let key = ApiKey {
        id: "key-1".to_owned(),
        account: AccountId::new("acct-1"),
        key_hash: "hash-of-token".to_owned(),
        label: "laptop".to_owned(),
        created_at: ts(1_000),
        revoked: false,
    };
    store.insert_api_key(&key).await.expect("api key");

    // Auth resolves a live key by its hash.
    assert_eq!(
        store
            .api_key_by_hash("hash-of-token")
            .await
            .expect("by hash"),
        Some(key.clone())
    );

    // A revoked key never authenticates — the auth path sees it as absent.
    store.revoke_api_key(&key.id).await.expect("revoke");
    assert_eq!(
        store
            .api_key_by_hash("hash-of-token")
            .await
            .expect("revoked lookup"),
        None,
        "a revoked key does not resolve"
    );

    // Revoking a missing key is a not-found.
    assert!(matches!(
        store.revoke_api_key("ghost").await,
        Err(crate::StoreError::NotFound)
    ));
}
