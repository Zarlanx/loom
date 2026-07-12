// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The [`Bus`] trait — publish/subscribe over [`Topic`]s — and the
//! [`Subscription`] handle a subscriber drains.
//!
//! The trait is object-safe (via [`async_trait`]) so `loomd` holds an
//! `Arc<dyn Bus>` and never knows whether it is talking to an [`InProcBus`] (a
//! function call away, standalone) or a future `NatsBus` (across a cluster,
//! marketplace). Delivery is **at-least-once and best-effort**: the bus is a
//! low-latency hint that keeps consumers warm, never the source of truth — that
//! is always the store (ADR-0013, see the crate docs).
//!
//! [`InProcBus`]: crate::InProcBus

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::message::Message;
use crate::topic::Topic;

/// A failure from a [`Bus`] operation.
///
/// The [`InProcBus`](crate::InProcBus) never produces one — publishing to an
/// in-process channel cannot fail — but a networked bus (`NatsBus`) can, so the
/// seam is fallible.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BusError {
    /// The underlying transport failed. The string is the backend's own message,
    /// for logging — never part of a matched contract.
    #[error("bus transport error: {0}")]
    Backend(String),
}

/// The publish/subscribe seam every bus backend implements.
///
/// `Send + Sync` so an `Arc<dyn Bus>` crosses task boundaries on a multi-threaded
/// runtime.
#[async_trait]
pub trait Bus: Send + Sync {
    /// Publishes `message` to every current subscriber whose topic
    /// [covers](Topic::covers) the message's subject.
    ///
    /// Best-effort and non-blocking: a message published with no matching
    /// subscriber is simply dropped, and a subscriber that has gone away is
    /// pruned. Consumers recover missed events by reconciling against the store,
    /// never by relying on delivery (ADR-0013).
    ///
    /// # Errors
    /// [`BusError`] if the underlying transport fails (never for
    /// [`InProcBus`](crate::InProcBus)).
    async fn publish(&self, message: Message) -> Result<(), BusError>;

    /// Subscribes to `topic`, returning a [`Subscription`] streaming every message
    /// published under a subject that `topic` covers. Only messages published
    /// *after* the subscription is established are delivered.
    ///
    /// # Errors
    /// [`BusError`] if the underlying transport fails (never for
    /// [`InProcBus`](crate::InProcBus)).
    async fn subscribe(&self, topic: Topic) -> Result<Subscription, BusError>;
}

/// A live subscription: a receiver of the [`Message`]s a [`Bus`] delivers for a
/// [`Topic`]. Dropping it unsubscribes; the bus prunes it on the next publish.
#[derive(Debug)]
pub struct Subscription {
    topic: Topic,
    rx: mpsc::UnboundedReceiver<Message>,
}

impl Subscription {
    /// Wraps a receiver handed out by a bus backend.
    pub(crate) fn new(topic: Topic, rx: mpsc::UnboundedReceiver<Message>) -> Self {
        Self { topic, rx }
    }

    /// The topic this subscription listens to.
    #[must_use]
    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    /// Awaits the next message, or `None` once the bus is dropped and the buffer
    /// is drained.
    pub async fn recv(&mut self) -> Option<Message> {
        self.rx.recv().await
    }

    /// Returns a buffered message without waiting, or `None` if none is ready.
    pub fn try_recv(&mut self) -> Option<Message> {
        self.rx.try_recv().ok()
    }
}
