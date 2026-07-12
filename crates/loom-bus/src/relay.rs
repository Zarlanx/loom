// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The outbox relay: drains `loom-store`'s `outbox` rows onto the [`Bus`].
//!
//! A state change and the `outbox` row announcing it are written in one store
//! transaction (control-plane §3). The relay is the other half: it reads unsent
//! rows oldest-first, publishes each onto the bus, and marks it sent. The order
//! is **publish-then-ack** — a row is marked sent only after it is published — so
//! a crash between the two leaves the row unsent and it redelivers on the next
//! drain. That is the at-least-once guarantee; combined with the stable
//! [`Message::dedup_key`] a consumer gets exactly-once *effects*.
//!
//! The relay drains through the narrow [`OutboxSource`] seam — the two `Store`
//! methods it actually needs — rather than the whole persistence trait. That
//! keeps its dependency honest and lets a test inject an ack failure (the
//! "missed ack" path) without standing up a full store. [`StoreOutbox`] adapts a
//! real [`Store`] onto the seam for `loomd`.
//!
//! [`Message::dedup_key`]: crate::Message::dedup_key

use core::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use loom_core::Timestamp;
use loom_store::{OutboxEvent, OutboxId, Store, StoreError};
use tokio::sync::Notify;
use tokio::time::{MissedTickBehavior, interval};

use crate::bus::{Bus, BusError};
use crate::message::Message;

/// The slice of the store the relay drains: list the unsent `outbox` rows and
/// mark one sent. Narrowed from [`Store`] so the relay never depends on the full
/// persistence surface and a fault-injecting fake needs only two methods.
#[async_trait]
pub trait OutboxSource: Send + Sync {
    /// Lists up to `limit` unsent `outbox` rows, oldest first.
    ///
    /// # Errors
    /// [`StoreError`] on a backend failure.
    async fn list_unsent(&self, limit: u32) -> Result<Vec<OutboxEvent>, StoreError>;

    /// Marks the row `id` published at `sent_at` — the relay's ack that removes it
    /// from the unsent set.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such row exists; [`StoreError`] on a backend
    /// failure.
    async fn mark_sent(&self, id: OutboxId, sent_at: Timestamp) -> Result<(), StoreError>;
}

/// Adapts a [`Store`] onto the [`OutboxSource`] seam the relay drains.
pub struct StoreOutbox {
    store: Arc<dyn Store>,
}

impl StoreOutbox {
    /// Wraps a store so its `outbox` methods satisfy [`OutboxSource`].
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

impl fmt::Debug for StoreOutbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreOutbox").finish_non_exhaustive()
    }
}

#[async_trait]
impl OutboxSource for StoreOutbox {
    async fn list_unsent(&self, limit: u32) -> Result<Vec<OutboxEvent>, StoreError> {
        self.store.list_unsent_outbox(limit).await
    }

    async fn mark_sent(&self, id: OutboxId, sent_at: Timestamp) -> Result<(), StoreError> {
        self.store.mark_outbox_sent(id, sent_at).await
    }
}

/// The wall-clock source the relay stamps acks with.
///
/// `loom-bus` never reads the wall clock itself — `SystemTime::now` is a denied
/// method workspace-wide (`clippy.toml`), and the whole backend injects time. A
/// binary supplies the real clock; tests supply [`ManualClock`](crate::ManualClock).
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Timestamp;
}

/// How the relay drains: batch size and poll cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayConfig {
    /// The maximum rows drained in one pass (`list_unsent` limit).
    pub batch_limit: u32,
    /// How often the [`run`](OutboxRelay::run) loop drains absent an explicit
    /// wakeup — the reconciliation backstop that keeps consumers warm.
    pub poll_interval: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            batch_limit: 128,
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// The outcome of one [`drain_once`](OutboxRelay::drain_once) pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DrainSummary {
    /// Rows published onto the bus this pass.
    pub published: usize,
    /// Rows successfully marked sent this pass.
    pub acked: usize,
}

impl DrainSummary {
    /// Rows published but not acked — they stay unsent and redeliver next pass.
    #[must_use]
    pub const fn missed_acks(self) -> usize {
        self.published.saturating_sub(self.acked)
    }
}

/// A failure that aborts a [`drain_once`](OutboxRelay::drain_once) pass.
///
/// A failed *ack* is not one of these — it is handled in-band (the row simply
/// redelivers). Only a failed outbox read or a failed publish stops the batch.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RelayError {
    /// Reading the `outbox` failed.
    #[error("reading the outbox failed: {0}")]
    Source(#[from] StoreError),
    /// Publishing a row onto the bus failed; the row stays unsent.
    #[error("publishing to the bus failed: {0}")]
    Publish(#[from] BusError),
}

/// Drains an [`OutboxSource`] onto a [`Bus`].
pub struct OutboxRelay {
    source: Arc<dyn OutboxSource>,
    bus: Arc<dyn Bus>,
    config: RelayConfig,
}

impl fmt::Debug for OutboxRelay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutboxRelay")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OutboxRelay {
    /// Builds a relay draining `source` onto `bus`.
    #[must_use]
    pub fn new(source: Arc<dyn OutboxSource>, bus: Arc<dyn Bus>, config: RelayConfig) -> Self {
        Self {
            source,
            bus,
            config,
        }
    }

    /// Drains one batch: publishes every unsent row (oldest first, up to
    /// `batch_limit`) onto the bus, marking each sent as it goes.
    ///
    /// Publish-then-ack per row: on a successful publish the row is marked sent;
    /// if the ack fails the row is left unsent (a warning is logged) and it
    /// redelivers on the next pass — at-least-once. This processes exactly one
    /// batch and does not re-fetch, so a persistently failing ack can never spin
    /// this call; the [`run`](OutboxRelay::run) loop retries on its cadence.
    ///
    /// # Errors
    /// [`RelayError::Source`] if the outbox read fails; [`RelayError::Publish`] if
    /// a publish fails (the batch stops there, leaving that row and the rest
    /// unsent for the next pass).
    pub async fn drain_once(&self, now: Timestamp) -> Result<DrainSummary, RelayError> {
        let rows = self.source.list_unsent(self.config.batch_limit).await?;
        let mut summary = DrainSummary::default();
        for row in &rows {
            self.bus.publish(Message::from_outbox(row)).await?;
            summary.published += 1;
            match self.source.mark_sent(row.id, now).await {
                Ok(()) => summary.acked += 1,
                Err(err) => {
                    tracing::warn!(
                        outbox_id = row.id.0,
                        error = %err,
                        "outbox ack failed; row will redeliver on the next drain",
                    );
                }
            }
        }
        Ok(summary)
    }

    /// Runs the relay until `shutdown` fires.
    ///
    /// Drains once immediately (clearing any startup backlog), then drains on each
    /// `wakeup` poke and on the `poll_interval` tick — the poked path is
    /// low-latency, the interval is the reconciliation backstop. A drain error is
    /// logged and retried on the next wake; it never tears the loop down.
    pub async fn run(self, clock: Arc<dyn Clock>, wakeup: Arc<Notify>, shutdown: Arc<Notify>) {
        self.drain_and_log(clock.now()).await;
        let mut ticker = interval(self.config.poll_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await; // the first tick is immediate — consume it.
        loop {
            tokio::select! {
                () = shutdown.notified() => break,
                () = wakeup.notified() => self.drain_and_log(clock.now()).await,
                _ = ticker.tick() => self.drain_and_log(clock.now()).await,
            }
        }
    }

    /// One drain whose error is logged rather than propagated — the loop body.
    async fn drain_and_log(&self, now: Timestamp) {
        if let Err(err) = self.drain_once(now).await {
            tracing::warn!(error = %err, "outbox relay drain failed; will retry");
        }
    }
}
