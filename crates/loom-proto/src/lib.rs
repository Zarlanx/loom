// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-proto` — the wire schema shared by the host agent (`loom-hostd`) and the
//! server-side agent-gateway (`loom-agentproto`).
//!
//! One schema, compiled into both sides of the wire, so agent and server can never
//! drift (backend.md §1). It owns three things:
//!
//! - the generated protobuf types under [`v1`] (the [`v1::Envelope`] plus, from PR-02b,
//!   the M1 message set),
//! - the length-prefix framing [`codec`] (agent-protocol.md §2.1), and
//! - the checked-in [`golden`] wire vectors that CI re-verifies byte-for-byte (CI job f).
//!
//! The contract is frozen once PR-02 lands and evolves **additively only** thereafter
//! (agent-protocol.md §2.3): new fields and new `oneof` variants, never a renumbering or
//! a removal inside a major version.

pub mod codec;
pub mod golden;

/// Generated prost types for the `loom.v1` protobuf package.
///
/// The generated code is exempt from the workspace lint wall — it is machine-written and
/// not ours to restyle.
pub mod v1 {
    #![allow(
        clippy::all,
        clippy::pedantic,
        clippy::nursery,
        missing_debug_implementations
    )]
    include!(concat!(env!("OUT_DIR"), "/loom.v1.rs"));
}

pub use v1::{Envelope, envelope::Body};
