// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The shared [`Store`] conformance suite, run against the in-memory
//! [`FakeStore`].
//!
//! This is the fake's half of the parity contract (workspace-setup.md §6): the
//! *same* suite runs against the file-backed WAL `SqliteStore` in PR-05b, so a
//! fake that drifts from the real backend fails here. Requires the
//! `test-support` (for `FakeStore`) and `conformance` (for the suite) features.

#![cfg(all(feature = "test-support", feature = "conformance"))]

use loom_store::FakeStore;
use loom_store::conformance;

#[tokio::test]
async fn fake_store_passes_the_conformance_suite() {
    conformance::run_all(|| async { FakeStore::new() }).await;
}
