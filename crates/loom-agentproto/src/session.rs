// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The transport-neutral [`Session`] seam and the connection's [`PeerIdentity`].
//!
//! A session is one accepted agent connection, demuxed into the four logical streams
//! of the wire contract (control / heartbeat / log / metering, agent-protocol.md §1.4).
//! The terminator drives a `Session` without caring which transport is underneath:
//! [`WssSession`](crate::wss::WssSession) is the WSS baseline self-host ships, and the
//! [`quic`](crate::quic) scaffold documents the `quinn` mapping the same seam admits.
//!
//! # `mTLS` identity is bound to the connection, not the payload
//!
//! Steady-state connections are `mTLS` (agent-protocol.md §1.2): the gateway pins the
//! Loom `CA`, validates the agent's client certificate, and reads the `agent_id` from
//! its subject. That authenticated identity is a [`PeerIdentity`] the transport hands
//! the terminator — every event the terminator publishes is stamped with *it*, never
//! with a self-asserted field in the message, so a compromised or buggy agent can never
//! post events in another agent's name (§5 fencing, §6). Before enrollment an agent has
//! no certificate and connects [token-only](PeerIdentity::Bootstrap); such a connection
//! may send nothing but an `EnrollRequest` (§1.2).
//!
//! Certificate validation and subject extraction live at the `rustls` layer that `loomd`
//! wires around this crate (PR-11); here the identity is the *input* that layer produces,
//! which keeps the terminator bootable in-process with no sockets or `TLS` stack.

use async_trait::async_trait;
use loom_proto::{
    Envelope,
    codec::{Channel, CodecError},
};

/// The transport-framing protocol identifier offered at connection setup
/// (agent-protocol.md §1.1): the WSS `Sec-WebSocket-Protocol` value and the QUIC ALPN
/// token. It versions the *framing/multiplexing*, independent of the protobuf schema
/// version carried in each [`Envelope`].
pub const WIRE_PROTOCOL_ID: &str = "loom/1";

/// The authenticated identity of a connected agent, established by the transport's
/// `mTLS` handshake (agent-protocol.md §1.2).
///
/// The terminator stamps this — never a message field — onto every bridged event, so
/// identity is fenced to the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerIdentity {
    /// A token-only bootstrap connection: the agent has no certificate yet and may send
    /// only an `EnrollRequest` (agent-protocol.md §1.2).
    Bootstrap,
    /// An enrolled node presenting a `CA`-signed client certificate whose subject is
    /// this `agent_id`.
    Node {
        /// The `agent_id` read from the client certificate subject (`CN`/`SAN`).
        agent_id: String,
    },
}

impl PeerIdentity {
    /// Constructs an authenticated node identity for `agent_id`.
    #[must_use]
    pub fn node(agent_id: impl Into<String>) -> Self {
        Self::Node {
            agent_id: agent_id.into(),
        }
    }

    /// The `agent_id`, or `None` on a token-only bootstrap connection.
    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Bootstrap => None,
            Self::Node { agent_id } => Some(agent_id),
        }
    }

    /// Whether this is a token-only bootstrap connection.
    #[must_use]
    pub fn is_bootstrap(&self) -> bool {
        matches!(self, Self::Bootstrap)
    }
}

/// One demuxed inbound message: the logical [`Channel`] it arrived on plus the decoded
/// [`Envelope`].
#[derive(Debug, Clone, PartialEq)]
pub struct InboundFrame {
    /// The logical stream the frame arrived on.
    pub channel: Channel,
    /// The decoded wire envelope.
    pub envelope: Envelope,
}

/// A failure sending or receiving on an agent session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// A frame failed to encode or decode against the `loom-proto` codec.
    #[error("codec: {0}")]
    Codec(#[from] CodecError),

    /// The underlying `WebSocket` errored. Boxed: `tungstenite::Error` is large, and
    /// keeping it off the stack keeps `Result<_, SessionError>` small
    /// (`clippy::result_large_err`).
    #[error("websocket: {0}")]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),

    /// The peer closed the connection (clean close or end of stream). A non-empty
    /// `reason` carries an application-level close reason (e.g. `enroll_token_invalid`,
    /// agent-protocol.md §3a), which the caller may treat as terminal.
    #[error("agent closed the session{}", .reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default())]
    Closed {
        /// The `WebSocket` close-frame reason, if the peer supplied a non-empty one.
        reason: Option<String>,
    },

    /// The peer sent something the control channel does not carry (a text frame, or a
    /// binary frame with trailing bytes after one message).
    #[error("protocol violation: {0}")]
    Protocol(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for SessionError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(e))
    }
}

impl SessionError {
    /// The clean end-of-session sentinel: the agent closed with no application reason.
    #[must_use]
    pub fn is_clean_close(&self) -> bool {
        matches!(self, Self::Closed { reason: None })
    }
}

/// One accepted agent connection, demuxed into the four logical wire streams.
///
/// The terminator drives this seam transport-agnostically:
/// [`recv`](Session::recv) yields the next `(channel, envelope)`, [`send`](Session::send)
/// writes an envelope back on a channel, and [`close`](Session::close) /
/// [`close_with_reason`](Session::close_with_reason) end the connection. End of stream
/// surfaces as [`SessionError::Closed`], so `recv` never returns a sentinel the caller
/// must special-case beyond the error.
///
/// `Send` so a terminator can be spawned onto a multi-threaded runtime.
#[async_trait]
pub trait Session: Send {
    /// Receives the next demuxed inbound frame.
    ///
    /// # Errors
    /// [`SessionError::Closed`] when the agent closes or the stream ends;
    /// [`SessionError::Protocol`] on a text frame or trailing bytes after a message;
    /// [`SessionError::Codec`] if the payload is not a decodable `Envelope`;
    /// [`SessionError::WebSocket`] on a transport-level failure.
    async fn recv(&mut self) -> Result<InboundFrame, SessionError>;

    /// Sends `envelope` to the agent on `channel`.
    ///
    /// # Errors
    /// [`SessionError::Codec`] if the envelope exceeds the frame cap;
    /// [`SessionError::WebSocket`] on a write failure.
    async fn send(&mut self, channel: Channel, envelope: &Envelope) -> Result<(), SessionError>;

    /// Initiates a clean close.
    ///
    /// # Errors
    /// [`SessionError::WebSocket`] if the close frame cannot be written.
    async fn close(&mut self) -> Result<(), SessionError>;

    /// Closes with an application-level `reason` (e.g. `enroll_token_invalid`), which the
    /// agent surfaces as [`SessionError::Closed`]`{ reason: Some(..) }`.
    ///
    /// # Errors
    /// [`SessionError::WebSocket`] if the close frame cannot be written.
    async fn close_with_reason(&mut self, reason: &str) -> Result<(), SessionError>;
}

#[cfg(test)]
mod tests {
    use super::{PeerIdentity, SessionError};

    #[test]
    fn bootstrap_identity_has_no_agent_id() {
        let id = PeerIdentity::Bootstrap;
        assert!(id.is_bootstrap());
        assert_eq!(id.agent_id(), None);
    }

    #[test]
    fn node_identity_carries_its_agent_id() {
        let id = PeerIdentity::node("agent-7");
        assert!(!id.is_bootstrap());
        assert_eq!(id.agent_id(), Some("agent-7"));
    }

    #[test]
    fn clean_close_is_distinguished_from_a_reasoned_close() {
        assert!(SessionError::Closed { reason: None }.is_clean_close());
        assert!(
            !SessionError::Closed {
                reason: Some("enroll_token_invalid".to_owned()),
            }
            .is_clean_close()
        );
    }
}
