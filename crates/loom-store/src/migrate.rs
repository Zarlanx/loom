// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The embedded migration set and the apply/verify entry points.
//!
//! The `migrations/` directory at the repo root is the single logical history
//! (workspace-setup.md §7); [`MIGRATOR`] embeds it at compile time. `SqliteStore`
//! runs it on connect, and `cargo xtask migrate --backend sqlite-wal` (PR-05c)
//! drives [`apply`] / [`verify`] against a target database.

use sqlx::migrate::Migrator;

use crate::error::StoreError;
use crate::sqlite::connect_pool;

/// The embedded migration set (`migrations/`, one logical history).
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// The result of an apply/verify pass, for the `xtask migrate` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// The number of migrations embedded in the binary.
    pub embedded: usize,
    /// The number already applied to the target database.
    pub applied: usize,
}

impl MigrationReport {
    /// Whether the database is fully migrated (nothing pending).
    #[must_use]
    pub const fn is_current(&self) -> bool {
        self.applied >= self.embedded
    }

    /// The number of migrations still to apply.
    #[must_use]
    pub const fn pending(&self) -> usize {
        self.embedded.saturating_sub(self.applied)
    }
}

/// Applies every pending migration to the SQLite-WAL database at `path`,
/// creating it if absent, and returns the resulting [`MigrationReport`].
///
/// # Errors
/// [`StoreError::Backend`] if the database cannot be opened or a migration
/// fails to apply.
pub async fn apply(path: &std::path::Path) -> Result<MigrationReport, StoreError> {
    let pool = connect_pool(path, 1).await?;
    MIGRATOR
        .run(&pool)
        .await
        .map_err(|e| StoreError::Backend(format!("migrate: {e}")))?;
    let report = report_for(&pool).await?;
    pool.close().await;
    Ok(report)
}

/// Verifies the SQLite-WAL database at `path` is fully migrated *without*
/// applying anything, returning its [`MigrationReport`]. A database with
/// [`MigrationReport::pending`] migrations is reported (not applied) so a caller
/// can decide whether that is a failure.
///
/// # Errors
/// [`StoreError::Backend`] if the database cannot be opened or its applied-set
/// cannot be read.
pub async fn verify(path: &std::path::Path) -> Result<MigrationReport, StoreError> {
    let pool = connect_pool(path, 1).await?;
    let report = report_for(&pool).await?;
    pool.close().await;
    Ok(report)
}

/// Reads how many migrations the `_sqlx_migrations` bookkeeping table records as
/// applied, against how many are embedded.
async fn report_for(pool: &sqlx::SqlitePool) -> Result<MigrationReport, StoreError> {
    let embedded = MIGRATOR.iter().count();
    // `_sqlx_migrations` is created by the migrator; before any run it is absent,
    // so a missing table simply means zero applied.
    let applied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| StoreError::Backend(format!("migrate probe: {e}")))?;
    let applied = if applied == 0 {
        0
    } else {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await
            .map_err(|e| StoreError::Backend(format!("migrate count: {e}")))?
    };
    let applied = usize::try_from(applied)
        .map_err(|e| StoreError::Backend(format!("migrate count range: {e}")))?;
    Ok(MigrationReport { embedded, applied })
}
