// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-bus` — the event-bus seam and the transactional-outbox relay.
//!
//! Two things live here (backend.md §1, §7):
//!
//! - The [`Bus`] trait — `publish`/`subscribe` over dot-delimited [`Topic`]s for
//!   the job-lifecycle and agent-event families — and its in-process
//!   implementation [`InProcBus`] (tokio channels). The trait is what lets the
//!   scheduler emit a placement event *identically* whether the consumer is a
//!   function call away (standalone) or across a `NATS` cluster (marketplace);
//!   the `NatsBus` implementation is deferred to marketplace scale.
//! - The [`OutboxRelay`] — the task that drains `loom-store`'s `outbox` rows onto
//!   whichever [`Bus`] is active, publishing each unsent row and marking it sent.
//!
//! **Bus events are delivery hints; the store is the authority (ADR-0013).** The
//! embedded bus is not a durable, replayable log — an `InProcBus` loses in-flight
//! events across a `loomd` restart. What makes that safe is the same reconciliation
//! contract the whole backend rests on: authoritative state lives in `SQLite`, and
//! every consumer effect must be reconstructable by scanning the store on the next
//! tick. So the relay gives **at-least-once** dispatch for state changes already
//! recorded in the DB — a state change and the `outbox` row announcing it are
//! written in one transaction (control-plane §3), and the relay publishes the row
//! then marks it sent. A crash between publish and mark leaves the row unsent and
//! it redelivers; because every delivery of an `outbox` row carries the same
//! [`Message::dedup_key`], a consumer can make its effect idempotent, giving
//! exactly-once *effects* over at-least-once delivery.
//!
//! Every seam ships its fake inside this crate behind the `test-support` feature
//! (workspace-setup.md §6): [`RecordingBus`] captures published messages for
//! assertions, [`ManualClock`] drives the relay's timestamps deterministically.

// Type names deliberately echo their module (`BusError`, `OutboxRelay`,
// `RelayConfig`) so they read unambiguously at call sites in `loomd`.
#![allow(clippy::module_name_repetitions)]

pub mod bus;
pub mod inproc;
pub mod message;
pub mod relay;
pub mod topic;

#[cfg(feature = "test-support")]
pub mod testing;

pub use bus::{Bus, BusError, Subscription};
pub use inproc::InProcBus;
pub use message::Message;
pub use relay::{
    Clock, DrainSummary, OutboxRelay, OutboxSource, RelayConfig, RelayError, StoreOutbox,
};
pub use topic::Topic;

#[cfg(feature = "test-support")]
pub use testing::{ManualClock, RecordingBus};
