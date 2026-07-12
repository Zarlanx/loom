// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-hostd` — the Loom host agent (PR-08).
//!
//! The agent is the one piece of Loom that runs on a stranger's machine
//! ([`host-agent.md`]). It holds a single **outbound-only** control connection to the
//! agent-gateway, enrolls with a locally-generated CSR, heartbeats its state, and drives
//! the client side of the frozen M1 wire contract ([`agent-protocol.md`] §2–3). It never
//! listens, never accepts inbound connections, and — in this skeleton — never touches a
//! GPU or a real network.
//!
//! Everything lives in this library so the binary ([`main`](../loom_hostd/index.html)) is
//! thin wiring and an in-process test can boot the agent against a fake gateway with no
//! sockets (`workspace-setup.md` §1, §6). The pieces:
//!
//! - [`config`] — the owner's `host.toml`.
//! - [`transport`] — `Envelope`s over a WebSocket, framed with the `loom-proto` codec.
//! - [`connect`] — the outbound connector seam + reconnect backoff.
//! - [`enroll`] — the token-only CSR enrollment handshake.
//! - [`identity`] — the node keypair, CSR, and durable keystore.
//! - [`wire`] / [`clock`] — envelope construction and the injected wall clock.
//!
//! The agent FSM, heartbeat loop, and durable spool land in PR-08b.
//!
//! [`host-agent.md`]: https://github.com/Zarlanx/loom/blob/main/docs/platform/host-agent.md
//! [`agent-protocol.md`]: https://github.com/Zarlanx/loom/blob/main/docs/platform/agent-protocol.md

pub mod clock;
pub mod config;
pub mod connect;
pub mod enroll;
pub mod identity;
pub mod transport;
pub mod wire;

#[cfg(any(test, feature = "test-support"))]
pub mod testsupport;

pub use clock::{FixedClock, SystemClock, WallClock};
pub use config::{ConfigError, HostdConfig, ReconnectConfig};
pub use connect::{BackoffPolicy, Connector, Jitter, WssConnector};
pub use enroll::{EnrollError, Enroller, Enrollment, EnrollmentRequest, NodeProfile};
pub use identity::{CsrProvider, EnrolledNode, KeyMaterial, KeystoreError, PlaceholderCsr};
pub use transport::{TransportError, WsTransport};
pub use wire::{MsgIdGen, PROTOCOL_VERSION};
