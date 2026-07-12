// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-agentproto` — the server-side agent-gateway: the session terminator, its
//! 4-stream demux, and the bridge that lands decoded agent events on the [`loom_bus`].
//!
//! # The terminator (PR-09)
//!
//! An enrolled host agent ([`loom-hostd`](https://docs.rs/loom-hostd)) opens exactly
//! one long-lived connection to the gateway (agent-protocol.md §1.1). This crate is the
//! *server* end of that wire: it accepts the connection, demuxes the four logical
//! streams (control / heartbeat / log / metering, §1.4), decodes each
//! [`loom_proto::Envelope`], and republishes it as an [`AgentEvent`](event::AgentEvent)
//! on the bus, stamped with the connection's authenticated identity so a message can
//! never speak for another agent (§5, §6).
//!
//! The pieces:
//!
//! - [`session`] — the transport-neutral [`Session`](session::Session) seam plus
//!   [`PeerIdentity`](session::PeerIdentity), the connection's `mTLS` identity;
//! - [`wss`] — the WSS baseline ([`WssSession`](wss::WssSession)) over
//!   `tokio-tungstenite`, the transport self-host actually ships (agent-protocol.md §1.1);
//! - [`quic`] — the QUIC scaffold: the ALPN/stream mapping the same [`Session`](session::Session)
//!   admits once the `quinn` endpoint is wired (PR-11/PR-21);
//! - [`event`] / [`bridge`] — the decoded-message → [`loom_bus`] bridge;
//! - [`terminator`] — [`SessionTerminator`](terminator::SessionTerminator), which drives
//!   a session, demuxes, and (from PR-09b) runs enrollment.
//!
//! # The security bootstrap (PR-26)
//!
//! The crate also carries the [`bootstrap`] security seam (PR-26 `bootstrap-auth`): the
//! local `CA` + node-cert signing, enrollment-token issuance/verification, the standalone
//! admin token, and the on-disk secrets layout. The terminator's enrollment handler and
//! `loomd init` both build on it.

// Agent-gateway and security-bootstrap type names deliberately echo their module
// (`SessionError`, `BootstrapError`, `EnrollmentKey`) so they read unambiguously at call
// sites in `loomd` and the terminator.
#![allow(clippy::module_name_repetitions)]

pub mod bootstrap;
pub mod bridge;
pub mod clock;
pub mod enroll;
pub mod event;
pub mod quic;
pub mod session;
pub mod terminator;
pub mod wss;

#[cfg(feature = "test-support")]
pub mod testing;

pub use clock::{Clock, SystemClock};
pub use enroll::{EnrollError, Enroller, Grant};
pub use event::{AgentEvent, AgentEventKind};
pub use session::{InboundFrame, PeerIdentity, Session, SessionError, WIRE_PROTOCOL_ID};
pub use terminator::{ServeSummary, SessionTerminator, TerminatorError};
