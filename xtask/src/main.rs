// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask` — typed dev tooling (docs/build/workspace-setup.md §5).
//!
//! Anything a human would otherwise paste from a README into a shell becomes a verb
//! here. Every verb below is a stub that reports which PR gives it teeth; CI invokes
//! `codegen --check` and `migrate` today, so their no-op success paths are deliberate.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().verb {
        Verb::Codegen { check } => {
            // Gains teeth at PR-02 (proto regen) and PR-04/PR-11 (OpenAPI diff gate).
            let mode = if check { " --check" } else { "" };
            println!("xtask codegen{mode}: no codegen targets yet — scaffold only (PR-02/PR-04)");
        }
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

/// Regenerate the checked-in `loom-proto` golden vectors from the canonical message set
/// (workspace-setup.md §5). The blessed path for an intentional additive schema change;
/// CI only ever *verifies* these bytes, never regenerates them.
fn golden_regen() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("crates")
        .join("loom-proto")
        .join("tests")
        .join("golden");
    std::fs::create_dir_all(&dir)?;

    // Stage the full set into a sibling temp dir first, so a mid-generation failure (a
    // panicking `vectors()` or a failed write) can never empty or half-populate the live
    // fixtures directory: nothing under `dir` is touched until every vector is written.
    let staging = dir.with_extension("regen-tmp");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    let vectors = loom_proto::golden::vectors();
    for vector in &vectors {
        std::fs::write(staging.join(vector.filename()), &vector.bytes)?;
    }

    // Generation succeeded — swap the completed set into place. Clear stale vectors first
    // (a `.bin` left behind after a vector is renamed or removed would otherwise linger,
    // and the golden test only iterates the current set), using the same case-insensitive
    // extension check the golden test applies so the two consumers never disagree.
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("bin"))
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    for vector in &vectors {
        let path = dir.join(vector.filename());
        std::fs::rename(staging.join(vector.filename()), &path)?;
        println!("wrote {} ({} bytes)", path.display(), vector.bytes.len());
    }
    std::fs::remove_dir_all(&staging)?;
    Ok(())
}
