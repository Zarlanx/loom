// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-agentproto` — server-side agent-gateway session logic (QUIC/WSS terminator, mTLS identity, 4-stream demux). Lands in PR-09.
//!
//! Ahead of the terminator, this crate carries the [`bootstrap`] security seam
//! (PR-26 `bootstrap-auth`): the local `CA` + node-cert signing, enrollment-token
//! issuance/verification, the standalone admin token, and the on-disk secrets
//! layout. The gateway's enrollment handler and `loomd init` both build on it.

// Security-bootstrap type names deliberately echo their module (`BootstrapError`,
// `EnrollmentKey`) so they read unambiguously at call sites in `loomd` and the
// gateway.
#![allow(clippy::module_name_repetitions)]

pub mod bootstrap;
