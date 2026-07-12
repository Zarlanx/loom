// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Test-support fakes for the bus seam (workspace-setup.md §6).
//!
//! [`RecordingBus`] is a capturing tap over an [`InProcBus`]: it delivers to
//! subscribers exactly like the real in-process bus while recording every
//! published [`Message`], so a downstream test can assert "a placement event was
//! published" without managing a subscription. [`ManualClock`] is a
//! deterministic [`Clock`] for driving the relay's ack timestamps.
//!
//! These ship inside the crate that defines the trait, behind `test-support`, so
//! a downstream crate depends on `loom-bus` with `features = ["test-support"]` in
//! its `[dev-dependencies]` and gets them.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use loom_core::Timestamp;

use crate::bus::{Bus, BusError, Subscription};
use crate::inproc::InProcBus;
use crate::message::Message;
use crate::relay::Clock;
use crate::topic::Topic;

/// A [`Bus`] that records every published [`Message`] while still delivering to
/// subscribers (a capturing tap over an [`InProcBus`]).
#[derive(Debug, Clone, Default)]
pub struct RecordingBus {
    inner: InProcBus,
    published: Arc<Mutex<Vec<Message>>>,
}

impl RecordingBus {
    /// Creates an empty recording bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot of every message published so far, in publish order.
    #[must_use]
    pub fn published(&self) -> Vec<Message> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The subjects of every published message, in publish order.
    #[must_use]
    pub fn published_topics(&self) -> Vec<Topic> {
        self.published()
            .into_iter()
            .map(|message| message.topic)
            .collect()
    }
}

#[async_trait]
impl Bus for RecordingBus {
    async fn publish(&self, message: Message) -> Result<(), BusError> {
        // Record first, then deliver; no lock is held across the `.await`.
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.clone());
        self.inner.publish(message).await
    }

    async fn subscribe(&self, topic: Topic) -> Result<Subscription, BusError> {
        self.inner.subscribe(topic).await
    }
}

/// A [`Clock`] returning a manually-set instant — deterministic time for relay
/// tests, honoring the inject-a-clock discipline (`clippy.toml`).
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<Timestamp>>,
}

impl ManualClock {
    /// Creates a clock reading `start`.
    #[must_use]
    pub fn new(start: Timestamp) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Sets the instant the clock reports.
    pub fn set(&self, now: Timestamp) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    /// Advances the clock by `delta` milliseconds.
    pub fn advance_millis(&self, delta: i64) {
        let mut guard = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *guard = Timestamp::from_millis(guard.as_millis() + delta);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recording_bus_captures_and_delivers() {
        let bus = RecordingBus::new();
        let mut sub = bus
            .subscribe(Topic::job_lifecycle())
            .await
            .expect("subscribe");
        bus.publish(Message::text(Topic::new("job.scheduled"), "{}"))
            .await
            .expect("publish");
        // Delivered to the subscriber …
        assert!(sub.recv().await.is_some());
        // … and captured for assertions.
        assert_eq!(
            bus.published_topics(),
            vec![Topic::new("job.scheduled")],
            "the published subject is recorded",
        );
    }

    #[test]
    fn manual_clock_advances() {
        let clock = ManualClock::new(Timestamp::from_millis(1_000));
        assert_eq!(clock.now(), Timestamp::from_millis(1_000));
        clock.advance_millis(500);
        assert_eq!(clock.now(), Timestamp::from_millis(1_500));
        clock.set(Timestamp::from_millis(9_000));
        assert_eq!(clock.now(), Timestamp::from_millis(9_000));
    }
}
