// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Canonical golden wire vectors: one deterministic, fully-populated `Envelope` per
//! message shape, serialized to bytes.
//!
//! This module is the **single source of truth** for the checked-in vectors under
//! `crates/loom-proto/tests/golden/`. Two consumers read it and must never disagree:
//!
//! - `cargo xtask golden regen` writes each vector to `<name>.bin` — the blessed path for
//!   an intentional, reviewable schema change (workspace-setup.md §5).
//! - the `golden_vectors` integration test (CI job f) asserts each `<name>.bin` is
//!   byte-identical to what [`vectors`] produces, and that the bytes round-trip through
//!   decode/encode unchanged.
//!
//! Because both sides call [`vectors`], a stale checked-in file is the only way they can
//! diverge — which is exactly the drift the golden gate exists to catch.

use crate::v1::Envelope;
use prost::Message;

/// A single named golden vector: canonical serialized bytes of one `Envelope`.
#[derive(Debug, Clone)]
pub struct GoldenVector {
    /// Stable identifier; the checked-in file is `<name>.bin`.
    pub name: &'static str,
    /// Canonical serialized `Envelope` bytes.
    pub bytes: Vec<u8>,
}

impl GoldenVector {
    fn new(name: &'static str, envelope: &Envelope) -> Self {
        Self {
            name,
            bytes: envelope.encode_to_vec(),
        }
    }
}

/// A deterministic 26-character ULID-shaped id, right-padded with `0` for readability.
///
/// The value is advisory (the schema does not validate ULID structure); fixing it keeps
/// the golden bytes reproducible.
fn ulid(tag: &str) -> String {
    format!("{tag:0>26}")
}

/// The canonical set of golden vectors for the current schema.
///
/// PR-02a carries the header-only `Envelope`; PR-02b extends this with one vector per M1
/// message variant.
#[must_use]
pub fn vectors() -> Vec<GoldenVector> {
    vec![envelope_header()]
}

/// A body-less `Envelope` — exercises the frozen header and framing on their own.
fn envelope_header() -> GoldenVector {
    let envelope = Envelope {
        protocol_version: 1,
        msg_id: ulid("ENVHDR"),
        correlation_id: String::new(),
        timestamp_ms: 1_700_000_000_000,
    };
    GoldenVector::new("envelope_header", &envelope)
}
