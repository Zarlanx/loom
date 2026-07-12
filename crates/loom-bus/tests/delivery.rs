// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! At-least-once delivery over [`InProcBus`]: every live subscriber whose topic
//! covers a published subject receives every such message, with no lag drops.

use loom_bus::{Bus, InProcBus, Message, Topic};

/// The whole point: a live subscriber receives *every* covering message, in
/// order, and fan-out delivers the same stream to every subscriber.
#[tokio::test]
async fn every_live_subscriber_receives_every_covering_message() {
    let bus = InProcBus::new();
    let mut a = bus
        .subscribe(Topic::job_lifecycle())
        .await
        .expect("subscribe a");
    let mut b = bus
        .subscribe(Topic::job_lifecycle())
        .await
        .expect("subscribe b");

    let subjects = ["job.scheduled", "job.dispatched", "job.succeeded"];
    for subject in subjects {
        bus.publish(Message::text(Topic::new(subject), "{}"))
            .await
            .expect("publish");
    }

    for subject in subjects {
        let want = Topic::new(subject);
        assert_eq!(a.recv().await.expect("a message").topic, want);
        assert_eq!(b.recv().await.expect("b message").topic, want);
    }
}

/// A subscription to a family sees the fine-grained subjects beneath it, and a
/// subscription to a *different* family sees none of them.
#[tokio::test]
async fn family_subscription_filters_by_coverage() {
    let bus = InProcBus::new();
    let mut jobs = bus
        .subscribe(Topic::job_lifecycle())
        .await
        .expect("subscribe jobs");
    let mut agents = bus
        .subscribe(Topic::agent_events())
        .await
        .expect("subscribe agents");

    bus.publish(Message::text(Topic::new("job.scheduled"), "{}"))
        .await
        .expect("publish job");
    bus.publish(Message::text(Topic::new("agent.heartbeat"), "{}"))
        .await
        .expect("publish agent");

    assert_eq!(
        jobs.recv().await.expect("job message").topic,
        Topic::new("job.scheduled"),
    );
    assert!(jobs.try_recv().is_none(), "jobs sees no agent traffic");

    assert_eq!(
        agents.recv().await.expect("agent message").topic,
        Topic::new("agent.heartbeat"),
    );
    assert!(agents.try_recv().is_none(), "agents sees no job traffic");
}

/// A subscriber that joins *after* a publish does not see it — the bus is a live
/// hint, not a replayable log (ADR-0013).
#[tokio::test]
async fn a_late_subscriber_misses_earlier_messages() {
    let bus = InProcBus::new();
    bus.publish(Message::text(Topic::new("job.scheduled"), "{}"))
        .await
        .expect("publish before subscribe");
    let mut late = bus
        .subscribe(Topic::job_lifecycle())
        .await
        .expect("subscribe");
    assert!(
        late.try_recv().is_none(),
        "the earlier message is not replayed"
    );

    bus.publish(Message::text(Topic::new("job.dispatched"), "{}"))
        .await
        .expect("publish after subscribe");
    assert_eq!(
        late.recv().await.expect("later message").topic,
        Topic::new("job.dispatched"),
    );
}

/// A burst is buffered, not dropped: an unbounded per-subscriber queue means a
/// consumer that drains later still gets the whole burst (at-least-once, no lag).
#[tokio::test]
async fn a_burst_is_buffered_not_dropped() {
    const BURST: usize = 1_000;

    let bus = InProcBus::new();
    let mut sub = bus
        .subscribe(Topic::job_lifecycle())
        .await
        .expect("subscribe");

    for i in 0..BURST {
        bus.publish(Message::text(Topic::new("job.tick"), &i.to_string()))
            .await
            .expect("publish");
    }

    for i in 0..BURST {
        let message = sub.recv().await.expect("buffered message");
        assert_eq!(message.payload_utf8().expect("utf8"), i.to_string());
    }
    assert!(sub.try_recv().is_none(), "exactly the burst, nothing more");
}
