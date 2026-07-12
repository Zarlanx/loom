// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The shared [`Store`] conformance suite, run against the real `SqliteStore` on
//! a **file-backed WAL** database — never `:memory:` (workspace-setup.md §6).
//!
//! This is the real backend's half of the parity contract: the *same* suite that
//! passes on `FakeStore` (PR-05a) must pass here, or the fake has drifted. Two
//! extra tests exercise the WAL/locking behavior `:memory:` would hide: the
//! journal mode is actually WAL, and concurrent writers serialize on the write
//! lock via the busy-timeout rather than erroring.

#![cfg(feature = "conformance")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use loom_store::conformance;
use loom_store::{SqliteStore, Store};
use sqlx::Row;
use tempfile::TempDir;

/// A factory that hands each scenario a brand-new file-backed WAL database in a
/// shared temp dir, so list assertions never see another scenario's rows.
struct WalDbs {
    dir: TempDir,
    counter: AtomicU32,
}

impl WalDbs {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            dir: tempfile::tempdir().expect("create temp dir"),
            counter: AtomicU32::new(0),
        })
    }

    fn next_path(&self) -> PathBuf {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        self.dir.path().join(format!("conformance-{n}.db"))
    }
}

#[tokio::test]
async fn sqlite_store_passes_the_conformance_suite_on_file_backed_wal() {
    let dbs = WalDbs::new();
    conformance::run_all(|| {
        let dbs = Arc::clone(&dbs);
        async move {
            let path = dbs.next_path();
            SqliteStore::open(&path).await.expect("open WAL store")
        }
    })
    .await;
}

#[tokio::test]
async fn database_is_actually_in_wal_mode() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SqliteStore::open(&dir.path().join("wal.db"))
        .await
        .expect("open");
    let mode: String = sqlx::query("PRAGMA journal_mode")
        .fetch_one(store.pool())
        .await
        .expect("pragma")
        .try_get(0)
        .expect("get mode");
    assert_eq!(
        mode.to_lowercase(),
        "wal",
        "the conformance leg must run on a real WAL database, not :memory:"
    );
}

#[tokio::test]
async fn concurrent_writers_serialize_through_the_busy_timeout() {
    // A file-backed WAL database with a multi-connection pool: many tasks write
    // at once. SQLite serializes writers on one write lock, and the busy-timeout
    // makes contenders *wait* rather than fail with "database is locked" — the
    // exact path :memory: never exercises. Every insert must succeed.
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SqliteStore::open(&dir.path().join("concurrent.db"))
        .await
        .expect("open");

    let writers: usize = 24;
    let mut handles = Vec::with_capacity(writers);
    for i in 0..writers {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let millis = i64::try_from(i).expect("writer index fits i64");
            store
                .enqueue_outbox(&loom_store::NewOutboxEvent {
                    topic: format!("topic-{i}"),
                    payload: format!(r#"{{"n":{i}}}"#),
                    created_at: loom_core::Timestamp::from_millis(millis),
                })
                .await
                .expect("concurrent enqueue must not error under contention");
        }));
    }
    for handle in handles {
        handle.await.expect("writer task panicked");
    }

    let drained = store.list_unsent_outbox(1_000).await.expect("drain outbox");
    assert_eq!(
        drained.len(),
        writers,
        "every concurrent writer committed exactly one row"
    );
}
