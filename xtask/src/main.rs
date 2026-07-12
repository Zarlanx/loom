// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask` — typed dev tooling (docs/build/workspace-setup.md §5).
//!
//! Anything a human would otherwise paste from a README into a shell becomes a verb
//! here. `codegen --check` validates the committed `openapi.json` (PR-04a); the
//! remaining verbs are stubs that report which PR gives them teeth.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

mod openapi;

#[derive(Parser, Debug)]
#[command(name = "xtask", about = "Loom dev tooling", version)]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand, Debug)]
enum Verb {
    /// Regenerate `loom-proto` prost types and the `OpenAPI` spec from axum handlers.
    Codegen {
        /// Regenerate into a temp dir and fail if `git diff` is non-empty (CI jobs e/f).
        #[arg(long)]
        check: bool,
    },
    /// Golden-vector maintenance for the wire protocol. Never run in CI — CI only verifies.
    Golden {
        #[command(subcommand)]
        action: GoldenAction,
    },
    /// Apply / check the sqlx migration set against a target backend.
    Migrate {
        /// Target backend (`postgres` joins at marketplace scale, ADR-0013).
        #[arg(long, value_enum, default_value_t = MigrateBackend::SqliteWal)]
        backend: MigrateBackend,
    },
    /// Curated runtime-image pipeline (CI job g, nightly).
    Images {
        #[command(subcommand)]
        action: ImagesAction,
    },
    /// Assemble the static release binaries + checksums — the single blessed release path.
    Release,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum MigrateBackend {
    /// File-backed WAL `SQLite` — the only Phase-1 backend (ADR-0013).
    SqliteWal,
}

#[derive(Subcommand, Debug)]
enum GoldenAction {
    /// Deliberately regenerate the checked-in vectors after an intentional additive change.
    Regen,
}

#[derive(Subcommand, Debug)]
enum ImagesAction {
    /// Build the curated images reproducibly, pin by digest, emit SBOM + scan.
    Build,
}

fn main() -> Result<()> {
    match Cli::parse().verb {
        Verb::Codegen { check } => run_codegen(check)?,
        Verb::Golden {
            action: GoldenAction::Regen,
        } => {
            golden_regen()?;
        }
        Verb::Migrate { backend } => {
            // Gains teeth at PR-05 (store + migration set).
            println!("xtask migrate --backend {backend:?}: no migrations yet (land in PR-05)");
        }
        Verb::Images {
            action: ImagesAction::Build,
        } => {
            println!("xtask images build: image/runtime pipeline lands in PR-24");
        }
        Verb::Release => {
            println!("xtask release: release pipeline lands with the first tagged release");
        }
    }
    Ok(())
}

/// Validate the committed `openapi.json` (PR-04a). Proto regeneration lands in
/// PR-02 and generated-vs-committed `OpenAPI` diffing is deferred to PR-11, once real
/// axum handlers exist to regenerate the spec from; until then this verb structurally
/// validates the hand-authored contract only.
fn run_codegen(check: bool) -> Result<()> {
    // `CARGO_MANIFEST_DIR` is the `xtask/` crate dir; the spec sits at the repo root.
    let spec_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("openapi.json");
    let report = openapi::load_and_validate(&spec_path)?;
    let mode = if check { "codegen --check" } else { "codegen" };
    println!(
        "xtask {mode}: openapi.json valid — {ops} operations, {schemas} schemas, \
         {codes} error codes, {routes}/{total} golden-path routes",
        ops = report.operations,
        schemas = report.schemas,
        codes = report.error_codes,
        routes = report.golden_routes,
        total = openapi::GOLDEN_PATH_ROUTES.len(),
    );
    println!(
        "  note: proto regen lands in PR-02; generated-vs-committed OpenAPI diffing is \
         deferred to PR-11 (this verb validates the committed spec only)."
    );
    Ok(())
}

/// Regenerate the checked-in `loom-proto` golden vectors from the canonical message set
/// (workspace-setup.md §5). The blessed path for an intentional additive schema change;
/// CI only ever *verifies* these bytes, never regenerates them.
fn golden_regen() -> Result<()> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("crates")
        .join("loom-proto")
        .join("tests")
        .join("golden");
    std::fs::create_dir_all(&dir)?;

    for vector in loom_proto::golden::vectors() {
        let path = dir.join(format!("{}.bin", vector.name));
        std::fs::write(&path, &vector.bytes)?;
        println!("wrote {} ({} bytes)", path.display(), vector.bytes.len());
    }
    Ok(())
}
