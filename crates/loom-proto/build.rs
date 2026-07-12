// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Build script: compiles the authoritative `.proto` sources (`proto/loom/v1`) into
//! prost types via `prost-build`.
//!
//! Parsing goes through `protox` (a pure-Rust protobuf compiler) rather than shelling
//! out to a system `protoc`, so the build is hermetic — CI runners need no `protoc`
//! installed (workspace-setup.md §4). On the founder's macOS box `protoc` also exists at
//! `/opt/homebrew/bin/protoc`, but the build never depends on it.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("proto");

    // The `.proto` files to compile. Imports are resolved from `proto_root`; listing every
    // file explicitly (rather than relying on import discovery) keeps the codegen inputs
    // auditable.
    let files = [
        proto_root.join("loom/v1/common.proto"),
        proto_root.join("loom/v1/enrollment.proto"),
        proto_root.join("loom/v1/health.proto"),
        proto_root.join("loom/v1/job.proto"),
        proto_root.join("loom/v1/log.proto"),
        proto_root.join("loom/v1/envelope.proto"),
    ];

    // The per-file directives below track edits to the listed sources. The
    // directory-level watch is deliberately kept *as well*: it is the only thing
    // that fires when a brand-new `.proto` is added under `proto/` but not yet
    // listed above, so codegen can't silently go stale. Do not remove it as
    // "redundant" — the two watches cover different changes.
    println!("cargo:rerun-if-changed={}", proto_root.display());
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
    }

    let descriptors = protox::compile(&files, [&proto_root])?;
    prost_build::Config::new().compile_fds(descriptors)?;
    Ok(())
}
