// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The one error type every [`Store`](crate::Store) method returns.
//!
//! It is deliberately backend-agnostic: [`FakeStore`](crate::FakeStore) and the
//! `SqliteStore` both map their failures onto these variants, so callers match
//! on failure *modes* (a uniqueness conflict vs. a missing row) rather than on a
//! concrete backend's error. `sqlx` never appears in the public signature — the
//! `SqliteStore` translates its errors into [`StoreError`] at the boundary.

/// A failure from a [`Store`](crate::Store) operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A required row was not found (e.g. an update targeting a missing id).
    #[error("record not found")]
    NotFound,

    /// A uniqueness or referential constraint was violated — a duplicate
    /// primary key, a repeated `(job, attempt_no)`, or an orphaned foreign key.
    #[error("constraint conflict: {0}")]
    Conflict(String),

    /// An idempotency key was reused with a *different* request body — the
    /// renter-api §1.3 `idempotency_key_reused` case, surfaced to the caller so
    /// it can answer `422` rather than silently double-submit.
    #[error("idempotency key reused with a different request")]
    IdempotencyMismatch,

    /// A backend-level failure with no more specific mapping (I/O, a pool
    /// timeout, a malformed persisted value). The string is the backend's own
    /// message, for logging — never part of the matched contract.
    #[error("store backend error: {0}")]
    Backend(String),
}
