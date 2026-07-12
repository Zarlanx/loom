// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! In-crate test support: a [`FakeAgent`] and an in-process [`loopback`] pair, gated on
//! the `test-support` feature (workspace-setup.md §6 — the fake ships inside the crate
//! that defines the seam).
//!
//! The fake agent is the *client* end of the wire: it drives the **real**
//! [`WssSession`](crate::wss::WssSession) framing over a
//! [`tokio::io::duplex`](tokio::io::duplex) pipe, so a downstream test enrolls and
//! exchanges M1 messages against the real [`SessionTerminator`](crate::terminator) with
//! no sockets, no `TLS` stack, and no GPU — exactly the in-process proof PR-09 is judged
//! by. The real host agent is [`loom-hostd`](https://docs.rs/loom-hostd); this fake exists
//! only to exercise the gateway from the crate that owns it.

use loom_proto::{Body, Envelope, codec::Channel};
use tokio::io::DuplexStream;

use crate::session::{InboundFrame, Session};
use crate::wss::WssSession;

/// Buffer size for the in-process duplex pipe backing a loopback session — comfortably
/// above any control-channel frame.
const LOOPBACK_BUFFER_BYTES: usize = 64 * 1024;

/// Establishes an in-process WSS connection over a duplex pipe and returns
/// `(server, client)` sessions, each having completed the real `WebSocket` handshake.
///
/// Hand the `server` to [`SessionTerminator::serve`](crate::terminator::SessionTerminator::serve)
/// and wrap the `client` in a [`FakeAgent`].
///
/// # Panics
/// If either side of the in-process `WebSocket` handshake fails — a test-only helper.
#[must_use = "the returned sessions are the two ends of the loopback wire"]
pub async fn loopback() -> (WssSession<DuplexStream>, WssSession<DuplexStream>) {
    let (client_io, server_io) = tokio::io::duplex(LOOPBACK_BUFFER_BYTES);
    let server = tokio::spawn(async move {
        WssSession::accept(server_io)
            .await
            .expect("server handshake")
    });
    let client = WssSession::connect(client_io)
        .await
        .expect("client handshake");
    let server = server.await.expect("server accept task");
    (server, client)
}

/// The client end of a loopback wire, driving the real WSS framing as a joining agent
/// would. A thin helper: it mints monotonic `msg_id`s and panics on transport errors,
/// because it exists only inside tests.
#[derive(Debug)]
pub struct FakeAgent {
    session: WssSession<DuplexStream>,
    next_seq: u64,
}

impl FakeAgent {
    /// Wraps a client [`WssSession`] (the second element of [`loopback`]).
    #[must_use]
    pub fn new(session: WssSession<DuplexStream>) -> Self {
        Self {
            session,
            next_seq: 0,
        }
    }

    /// Wraps `body` in an [`Envelope`] with a fresh monotonic `msg_id`.
    #[must_use]
    pub fn envelope(&mut self, body: Body) -> Envelope {
        let seq = self.next_seq;
        self.next_seq += 1;
        Envelope {
            protocol_version: 1,
            msg_id: format!("01J000000000000000000FAKE{seq:02}"),
            correlation_id: String::new(),
            timestamp_ms: 1_700_000_000_000 + i64::try_from(seq).unwrap_or(0),
            body: Some(body),
        }
    }

    /// Sends `body` on `channel` (wrapping it in a fresh envelope).
    ///
    /// # Panics
    /// If the underlying session send fails — a test-only fake.
    pub async fn send(&mut self, channel: Channel, body: Body) {
        let envelope = self.envelope(body);
        self.session
            .send(channel, &envelope)
            .await
            .expect("fake agent send");
    }

    /// Sends a fully-formed `envelope` on `channel` (for messages a test builds directly,
    /// e.g. an `EnrollRequest` carrying a real `CSR`).
    ///
    /// # Panics
    /// If the underlying session send fails.
    pub async fn send_envelope(&mut self, channel: Channel, envelope: &Envelope) {
        self.session
            .send(channel, envelope)
            .await
            .expect("fake agent send envelope");
    }

    /// Receives the next inbound frame from the gateway.
    ///
    /// # Panics
    /// If the session errors before a frame arrives.
    pub async fn recv(&mut self) -> InboundFrame {
        self.session.recv().await.expect("fake agent recv")
    }

    /// Closes the connection cleanly.
    ///
    /// # Panics
    /// If the close frame cannot be written.
    pub async fn close(&mut self) {
        self.session.close().await.expect("fake agent close");
    }
}
