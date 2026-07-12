// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The WSS control-channel transport: `Envelope`s over a WebSocket, framed with the
//! `loom-proto` codec (`agent-protocol.md` §1.4, §2.1).
//!
//! Baseline transport is **WSS** (`host-agent.md` §10, `agent-protocol.md` §1.1). Each
//! logical stream — control / heartbeat / log / metering — is carried as a WebSocket
//! *binary* message whose payload is one channel-tagged [`loom_proto::codec`] frame, so on
//! the single TCP substream the four channels multiplex by tag exactly as the WSS-fallback
//! contract specifies (§1.4).
//!
//! [`WsTransport`] is generic over the underlying byte stream `S`, which keeps it free of
//! any socket/TLS assumption: in production `S` is a TLS TCP stream; in tests it is an
//! in-process [`tokio::io::duplex`] pipe driven by the fake gateway — the same WebSocket
//! handshake and framing on both, with no real network and no GPU.

use futures_util::{SinkExt, StreamExt};
use loom_proto::{
    Envelope,
    codec::{Channel, CodecError, decode_message, decode_wss_frame, wss_frame},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

/// Errors from sending or receiving on the control channel.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// A frame failed to encode/decode against the `loom-proto` codec.
    #[error("codec: {0}")]
    Codec(#[from] CodecError),
    /// The underlying WebSocket errored. Boxed: `tungstenite::Error` is large, and keeping
    /// it off the stack keeps `Result<_, TransportError>` small (`clippy::result_large_err`).
    #[error("websocket: {0}")]
    Ws(Box<tokio_tungstenite::tungstenite::Error>),
    /// A connection could not be established (DNS/TCP/handshake). Transient — the
    /// reconnect loop backs off and retries.
    #[error("connect failed: {0}")]
    Connect(String),
    /// The peer closed the connection (clean close or end of stream). A non-empty
    /// `reason` carries an application-level close reason (e.g. `enroll_token_invalid`,
    /// `agent-protocol.md` §3a), which the caller may treat as terminal.
    #[error("peer closed the control channel{}", .reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default())]
    Closed {
        /// The WebSocket close-frame reason, if the peer supplied a non-empty one.
        reason: Option<String>,
    },
    /// The peer sent something the control channel does not carry (e.g. a text frame,
    /// or a binary frame with trailing bytes after one message).
    #[error("protocol violation: {0}")]
    Protocol(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for TransportError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Ws(Box::new(e))
    }
}

/// A framed WebSocket control channel over an arbitrary byte stream `S`.
pub struct WsTransport<S> {
    ws: WebSocketStream<S>,
}

impl<S> std::fmt::Debug for WsTransport<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsTransport").finish_non_exhaustive()
    }
}

impl<S> WsTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Wraps an established [`WebSocketStream`] as a control channel.
    #[must_use]
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self { ws }
    }

    /// Sends `envelope` on `channel` as a single channel-tagged binary WebSocket message.
    ///
    /// # Errors
    ///
    /// [`TransportError::Codec`] if the envelope exceeds the frame cap, or
    /// [`TransportError::Ws`] on a WebSocket write failure.
    pub async fn send(
        &mut self,
        channel: Channel,
        envelope: &Envelope,
    ) -> Result<(), TransportError> {
        let frame = wss_frame(channel, envelope)?;
        self.ws.send(Message::Binary(frame)).await?;
        Ok(())
    }

    /// Receives the next `(channel, envelope)`, skipping ping/pong keepalives.
    ///
    /// # Errors
    ///
    /// - [`TransportError::Closed`] when the peer closes or the stream ends.
    /// - [`TransportError::Protocol`] on a text frame or trailing bytes after the message.
    /// - [`TransportError::Codec`] if the payload is not a decodable `Envelope`.
    /// - [`TransportError::Ws`] on a transport-level WebSocket error.
    pub async fn recv(&mut self) -> Result<(Channel, Envelope), TransportError> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    let (channel, payload, rest) = decode_wss_frame(&bytes)?;
                    if !rest.is_empty() {
                        return Err(TransportError::Protocol(format!(
                            "{} trailing bytes after control frame",
                            rest.len()
                        )));
                    }
                    let envelope: Envelope = decode_message(payload)?;
                    return Ok((channel, envelope));
                }
                // Keepalives: tungstenite answers pings itself; ignore both here. `Frame`
                // is a raw-frame write helper and is not yielded by a read stream, but the
                // match must be exhaustive, so it is ignored the same way.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(frame))) => {
                    let reason = frame
                        .map(|f| f.reason.into_owned())
                        .filter(|r| !r.is_empty());
                    return Err(TransportError::Closed { reason });
                }
                None => return Err(TransportError::Closed { reason: None }),
                Some(Ok(Message::Text(_))) => {
                    return Err(TransportError::Protocol(
                        "unexpected text frame on binary control channel".to_string(),
                    ));
                }
                Some(Err(e)) => return Err(TransportError::Ws(Box::new(e))),
            }
        }
    }

    /// Initiates a clean WebSocket close.
    ///
    /// # Errors
    ///
    /// [`TransportError::Ws`] if the close frame cannot be written.
    pub async fn close(&mut self) -> Result<(), TransportError> {
        self.ws.close(None).await?;
        Ok(())
    }

    /// Closes with an application-level reason (e.g. `enroll_token_invalid`), which the
    /// peer surfaces as [`TransportError::Closed`]`{ reason: Some(..) }`.
    ///
    /// # Errors
    ///
    /// [`TransportError::Ws`] if the close frame cannot be written.
    pub async fn close_with_reason(&mut self, reason: &str) -> Result<(), TransportError> {
        use tokio_tungstenite::tungstenite::protocol::CloseFrame;
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        self.ws
            .close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: reason.to_owned().into(),
            }))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::WsTransport;
    use crate::wire::{MsgIdGen, envelope};
    use loom_proto::{
        Body,
        codec::Channel,
        v1::{Heartbeat, JobAccept},
    };

    /// Drives a client transport and a server transport over an in-process duplex through
    /// a real WebSocket handshake — no sockets, no TLS.
    async fn duplex_pair() -> (
        WsTransport<tokio::io::DuplexStream>,
        WsTransport<tokio::io::DuplexStream>,
    ) {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let ws = tokio_tungstenite::accept_async(server_io)
                .await
                .expect("accept");
            WsTransport::new(ws)
        });
        let (ws, _resp) = tokio_tungstenite::client_async("ws://loom.test/agent", client_io)
            .await
            .expect("client handshake");
        let client = WsTransport::new(ws);
        let server = server.await.expect("server task");
        (client, server)
    }

    #[tokio::test]
    async fn round_trips_envelopes_across_channels() {
        let (mut client, mut server) = duplex_pair().await;
        let ids = MsgIdGen::new();

        let hb = envelope(
            ids.next_id(),
            String::new(),
            10,
            Body::Heartbeat(Heartbeat::default()),
        );
        client.send(Channel::Heartbeat, &hb).await.expect("send hb");
        let (ch, got) = server.recv().await.expect("recv hb");
        assert_eq!(ch, Channel::Heartbeat);
        assert_eq!(got, hb);

        // Server → client on the control channel, proving the bidi direction.
        let accept = envelope(
            ids.next_id(),
            "MSG0".to_string(),
            11,
            Body::JobAccept(JobAccept {
                attempt_id: "at1".to_string(),
            }),
        );
        server
            .send(Channel::Control, &accept)
            .await
            .expect("send accept");
        let (ch, got) = client.recv().await.expect("recv accept");
        assert_eq!(ch, Channel::Control);
        assert_eq!(got, accept);
    }

    #[tokio::test]
    async fn recv_reports_closed_when_peer_drops() {
        let (mut client, server) = duplex_pair().await;
        drop(server);
        let err = client.recv().await.expect_err("closed");
        assert!(matches!(
            err,
            super::TransportError::Closed { .. } | super::TransportError::Ws(_)
        ));
    }
}
