// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-store` — the persistence seam.
//!
//! One [`Store`] trait defines every durable surface the control plane touches:
//! jobs, attempts, leases (with the fencing compare-and-set), usage records,
//! the transactional outbox, idempotency keys, and the enrollment/auth tables
//! (`hosts`/`gpus`/`nodes`, `accounts`/`api_keys`). Phase 1 ships exactly two
//! implementations of it — an in-memory [`FakeStore`] (behind the `test-support`
//! feature) and a [`SqliteStore`] on `sqlx` over a file-backed WAL database —
//! and a single shared [`conformance`] suite (behind the `conformance` feature)
//! that both must pass, so a fake can never silently diverge from the real
//! backend (workspace-setup.md §6).
//!
//! The migration set lives at the repo-root `migrations/` (one logical history,
//! dialect notes inline) and is embedded and run through [`migrate`].
//!
//! Persistence returns the same `loom-core` aggregates the pure logic operates
//! on (`Job`, `Attempt`, `Lease`, `UsageRecord`, `Node`); the row types this
//! crate adds ([`records`]) are the ones the domain model does not own — the
//! outbox, idempotency, and enrollment/auth surfaces.

// Store-layer type names deliberately echo their module (`StoreError`,
// `OutboxEvent`) so they read unambiguously at call sites in `loomd`.
#![allow(clippy::module_name_repetitions)]

pub mod error;
pub mod migrate;
pub mod records;
pub mod sqlite;
pub mod store;

#[cfg(feature = "test-support")]
pub mod fake;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use error::StoreError;
pub use records::{
    Account, ApiKey, Gpu, Host, HostStatus, IdempotencyOutcome, IdempotencyRecord, JobQuery,
    LeaseCommit, NewOutboxEvent, OutboxEvent, OutboxId,
};
pub use sqlite::SqliteStore;
pub use store::Store;

#[cfg(feature = "test-support")]
pub use fake::FakeStore;
