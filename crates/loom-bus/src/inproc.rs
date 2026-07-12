// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`InProcBus`] — the in-process [`Bus`] used in the standalone profile.
//!
//! It is a fan-out over tokio channels: `subscribe` registers an unbounded
//! receiver against a [`Topic`]; `publish` clones the message to every registered
//! subscriber whose topic covers the subject. Unbounded per-subscriber queues
//! mean a live subscriber never loses a covering message to lag — the property
//! the at-least-once delivery tests assert — while the store-reconciliation
//! backstop (ADR-0013) covers the case of a subscriber that is simply absent when
//! an event is published. Clones share one registry, so a whole `loomd` can be
//! booted around one bus with no sockets and no root.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::bus::{Bus, BusError, Subscription};
use crate::message::Message;
use crate::topic::Topic;

/// One registered listener: the topic it wants and the sender feeding its
/// [`Subscription`].
#[derive(Debug)]
struct Subscriber {
    topic: Topic,
    tx: mpsc::UnboundedSender<Message>,
}

/// The mutable heart of an [`InProcBus`].
#[derive(Debug, Default)]
struct Registry {
    subscribers: Vec<Subscriber>,
}

/// An in-process [`Bus`]. Cheap to clone; every clone shares one registry.
#[derive(Debug, Clone, Default)]
pub struct InProcBus {
    registry: Arc<Mutex<Registry>>,
}

impl InProcBus {
    /// Creates an empty bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the registry, recovering from a poisoned lock rather than panicking
    /// — the crate's no-`unwrap` discipline (a poisoned registry still holds
    /// valid subscriber handles).
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The number of live subscriptions — for diagnostics and tests.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.lock().subscribers.len()
    }
}

#[async_trait]
impl Bus for InProcBus {
    async fn publish(&self, message: Message) -> Result<(), BusError> {
        // No `.await` is held across this guard, so the returned future stays
        // `Send` despite the `std` mutex; sends into unbounded channels never
        // block.
        let mut registry = self.lock();
        registry.subscribers.retain(|sub| {
            if sub.tx.is_closed() {
                return false; // the receiver was dropped — unsubscribe it.
            }
            if sub.topic.covers(&message.topic) {
                // A send only fails if the receiver is gone, in which case we prune.
                sub.tx.send(message.clone()).is_ok()
            } else {
                true
            }
        });
        Ok(())
    }

    async fn subscribe(&self, topic: Topic) -> Result<Subscription, BusError> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.lock().subscribers.push(Subscriber {
            topic: topic.clone(),
            tx,
        });
        Ok(Subscription::new(topic, rx))
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_covering_subscriber_receives_the_message() {
        let bus = InProcBus::new();
        let mut sub = bus
            .subscribe(Topic::job_lifecycle())
            .await
            .expect("subscribe");
        bus.publish(Message::text(Topic::new("job.scheduled"), "{}"))
            .await
            .expect("publish");
        let got = sub.recv().await.expect("message");
        assert_eq!(got.topic, Topic::new("job.scheduled"));
    }

    #[tokio::test]
    async fn a_non_covering_subscriber_receives_nothing() {
        let bus = InProcBus::new();
        let mut agent = bus
            .subscribe(Topic::agent_events())
            .await
            .expect("subscribe");
        bus.publish(Message::text(Topic::new("job.scheduled"), "{}"))
            .await
            .expect("publish");
        assert!(agent.try_recv().is_none());
    }

    #[tokio::test]
    async fn publishing_with_no_subscriber_is_a_no_op() {
        let bus = InProcBus::new();
        bus.publish(Message::text(Topic::new("job.scheduled"), "{}"))
            .await
            .expect("publish");
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn a_dropped_subscriber_is_pruned_on_publish() {
        let bus = InProcBus::new();
        let sub = bus
            .subscribe(Topic::job_lifecycle())
            .await
            .expect("subscribe");
        assert_eq!(bus.subscriber_count(), 1);
        drop(sub);
        bus.publish(Message::text(Topic::new("job.scheduled"), "{}"))
            .await
            .expect("publish");
        assert_eq!(bus.subscriber_count(), 0);
    }
}
