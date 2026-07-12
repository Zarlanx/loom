// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Reconcile-from-store: the outbox relay drains `loom-store`'s `outbox` rows
//! onto the bus exactly-once-in-effect, and redelivers a row whose ack was missed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use loom_bus::{
    Bus, Clock, DrainSummary, InProcBus, OutboxRelay, OutboxSource, RelayConfig, StoreOutbox, Topic,
};
use loom_core::Timestamp;
use loom_store::{FakeStore, NewOutboxEvent, OutboxEvent, OutboxId, Store, StoreError};
use tokio::sync::Notify;

// ----------------------------------------------------------------- fixtures

async fn enqueue(store: &FakeStore, topic: &str, ms: i64) {
    store
        .enqueue_outbox(&NewOutboxEvent {
            topic: topic.to_owned(),
            payload: format!(r#"{{"at":{ms}}}"#),
            created_at: Timestamp::from_millis(ms),
        })
        .await
        .expect("enqueue");
}

/// A [`Clock`] frozen at one instant — the relay's timestamps are irrelevant to
/// these assertions, so freezing them keeps the tests deterministic.
#[derive(Debug)]
struct FixedClock(Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// An [`OutboxSource`] that fails the first `fail_marks` acks, simulating a crash
/// between publish and ack — the "missed ack" that must trigger redelivery.
#[derive(Debug)]
struct FlakyOutbox {
    inner: Arc<FakeStore>,
    fail_marks: AtomicUsize,
}

impl FlakyOutbox {
    fn new(inner: Arc<FakeStore>, fail_marks: usize) -> Self {
        Self {
            inner,
            fail_marks: AtomicUsize::new(fail_marks),
        }
    }
}

#[async_trait]
impl OutboxSource for FlakyOutbox {
    async fn list_unsent(&self, limit: u32) -> Result<Vec<OutboxEvent>, StoreError> {
        self.inner.list_unsent_outbox(limit).await
    }

    async fn mark_sent(&self, id: OutboxId, sent_at: Timestamp) -> Result<(), StoreError> {
        // Consume one "failure credit" if any remain; while credits last, the ack
        // fails and the row is left unsent (it must redeliver).
        let failed = self
            .fail_marks
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failed {
            return Err(StoreError::Backend("injected ack failure".to_owned()));
        }
        self.inner.mark_outbox_sent(id, sent_at).await
    }
}

// -------------------------------------------------------------------- tests

/// The headline: a drain publishes every unsent row to the right family and marks
/// it sent, and a second drain publishes nothing — a sent row is never
/// republished (exactly-once in effect).
#[tokio::test]
async fn relay_drains_the_outbox_exactly_once_in_effect() {
    let store = Arc::new(FakeStore::new());
    enqueue(&store, "job.scheduled", 1_000).await;
    enqueue(&store, "job.dispatched", 2_000).await;
    enqueue(&store, "agent.heartbeat", 3_000).await;

    let bus = InProcBus::new();
    let mut jobs = bus
        .subscribe(Topic::job_lifecycle())
        .await
        .expect("sub jobs");
    let mut agents = bus
        .subscribe(Topic::agent_events())
        .await
        .expect("sub agents");

    let relay = OutboxRelay::new(
        Arc::new(StoreOutbox::new(store.clone())),
        Arc::new(bus.clone()),
        RelayConfig {
            batch_limit: 16,
            ..RelayConfig::default()
        },
    );

    let summary = relay
        .drain_once(Timestamp::from_millis(5_000))
        .await
        .expect("drain");
    assert_eq!(
        summary,
        DrainSummary {
            published: 3,
            acked: 3
        },
    );

    // The job family sees both job.* nudges, oldest first; the agent family sees
    // only the heartbeat.
    assert_eq!(
        jobs.recv().await.expect("job 1").topic,
        Topic::new("job.scheduled"),
    );
    assert_eq!(
        jobs.recv().await.expect("job 2").topic,
        Topic::new("job.dispatched"),
    );
    assert!(jobs.try_recv().is_none());
    assert_eq!(
        agents.recv().await.expect("agent 1").topic,
        Topic::new("agent.heartbeat"),
    );

    // The store is drained.
    assert!(
        store
            .list_unsent_outbox(16)
            .await
            .expect("unsent")
            .is_empty(),
    );

    // A second drain is a no-op — nothing published, nothing delivered.
    let again = relay
        .drain_once(Timestamp::from_millis(6_000))
        .await
        .expect("drain 2");
    assert_eq!(again, DrainSummary::default());
    assert!(jobs.try_recv().is_none());
    assert!(agents.try_recv().is_none());
}

/// A missed ack redelivers the row on the next drain, and both deliveries carry
/// the same dedup key — so a consumer dedups for exactly-once *effects*.
#[tokio::test]
async fn a_missed_ack_redelivers_with_a_stable_dedup_key() {
    let inner = Arc::new(FakeStore::new());
    enqueue(&inner, "job.scheduled", 1_000).await;

    let bus = InProcBus::new();
    let mut sub = bus.subscribe(Topic::job_lifecycle()).await.expect("sub");

    let flaky = Arc::new(FlakyOutbox::new(inner.clone(), 1)); // fail exactly one ack
    let relay = OutboxRelay::new(flaky, Arc::new(bus.clone()), RelayConfig::default());

    // Drain 1: the row publishes, but the ack fails — it stays unsent.
    let first = relay
        .drain_once(Timestamp::from_millis(2_000))
        .await
        .expect("drain 1");
    assert_eq!(
        first,
        DrainSummary {
            published: 1,
            acked: 0
        },
    );
    assert_eq!(first.missed_acks(), 1);
    let delivery_1 = sub.recv().await.expect("first delivery");
    assert!(
        !inner
            .list_unsent_outbox(16)
            .await
            .expect("unsent")
            .is_empty(),
        "an unacked row stays pending",
    );

    // Drain 2: the same row redelivers and now acks.
    let second = relay
        .drain_once(Timestamp::from_millis(3_000))
        .await
        .expect("drain 2");
    assert_eq!(
        second,
        DrainSummary {
            published: 1,
            acked: 1
        },
    );
    let delivery_2 = sub.recv().await.expect("redelivery");

    // Same event, twice — dedup key and subject are stable across redelivery.
    assert!(delivery_1.dedup_key.is_some());
    assert_eq!(delivery_1.dedup_key, delivery_2.dedup_key);
    assert_eq!(delivery_1.topic, delivery_2.topic);

    // Drain 3: fully drained.
    let third = relay
        .drain_once(Timestamp::from_millis(4_000))
        .await
        .expect("drain 3");
    assert_eq!(third, DrainSummary::default());
    assert!(
        inner
            .list_unsent_outbox(16)
            .await
            .expect("unsent")
            .is_empty(),
    );
    assert!(sub.try_recv().is_none());
}

/// One drain pass never exceeds `batch_limit`; successive passes clear a backlog.
#[tokio::test]
async fn the_batch_limit_bounds_one_pass() {
    let store = Arc::new(FakeStore::new());
    for i in 0..5 {
        enqueue(&store, "job.scheduled", 1_000 + i).await;
    }

    let bus = InProcBus::new();
    let relay = OutboxRelay::new(
        Arc::new(StoreOutbox::new(store.clone())),
        Arc::new(bus.clone()),
        RelayConfig {
            batch_limit: 2,
            ..RelayConfig::default()
        },
    );

    let now = Timestamp::from_millis(9_000);
    assert_eq!(relay.drain_once(now).await.expect("p1").published, 2);
    assert_eq!(relay.drain_once(now).await.expect("p2").published, 2);
    assert_eq!(relay.drain_once(now).await.expect("p3").published, 1);
    assert_eq!(relay.drain_once(now).await.expect("p4").published, 0);
    assert!(
        store
            .list_unsent_outbox(16)
            .await
            .expect("unsent")
            .is_empty(),
    );
}

/// The spawnable task drains on start, drains again when poked, and stops cleanly
/// on shutdown — the shape `loomd` runs it in.
#[tokio::test]
async fn the_run_loop_drains_on_start_and_wakeup_then_stops() {
    let store = Arc::new(FakeStore::new());
    enqueue(&store, "job.scheduled", 1_000).await;

    let bus = InProcBus::new();
    let mut sub = bus.subscribe(Topic::job_lifecycle()).await.expect("sub");

    let relay = OutboxRelay::new(
        Arc::new(StoreOutbox::new(store.clone())),
        Arc::new(bus.clone()),
        // A long interval so only the initial drain and explicit pokes fire —
        // the periodic backstop never trips during the test.
        RelayConfig {
            batch_limit: 16,
            poll_interval: Duration::from_secs(3_600),
        },
    );
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(Timestamp::from_millis(5_000)));
    let wakeup = Arc::new(Notify::new());
    let shutdown = Arc::new(Notify::new());
    let handle = tokio::spawn(relay.run(clock, wakeup.clone(), shutdown.clone()));

    // The initial drain clears the startup backlog.
    assert_eq!(
        sub.recv().await.expect("startup delivery").topic,
        Topic::new("job.scheduled"),
    );

    // A row enqueued after start is drained when the relay is poked.
    enqueue(&store, "job.dispatched", 2_000).await;
    wakeup.notify_one();
    assert_eq!(
        sub.recv().await.expect("wakeup delivery").topic,
        Topic::new("job.dispatched"),
    );

    // Shutdown joins the task.
    shutdown.notify_one();
    handle.await.expect("relay task joins");

    assert!(
        store
            .list_unsent_outbox(16)
            .await
            .expect("unsent")
            .is_empty(),
    );
}
