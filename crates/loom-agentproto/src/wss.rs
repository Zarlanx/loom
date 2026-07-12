// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`WssSession`] — the WSS baseline transport self-host ships (agent-protocol.md §1.1).
//!
//! Each logical stream — control / heartbeat / log / metering — is carried as a
//! `WebSocket` *binary* message whose payload is one channel-tagged
//! [`loom_proto::codec`] frame, so on the single TCP substream the four channels
//! multiplex by tag exactly as the WSS-fallback contract specifies (§1.4). This is the
//! server end of the same framing [`loom-hostd`](https://docs.rs/loom-hostd) speaks as
//! the client, both over the one shared [`loom_proto`] codec.
//!
//! [`WssSession`] is generic over the underlying byte stream `S`, which keeps it free of
//! any socket/`TLS` assumption: in production `S` is a `TLS` `TCP` stream terminated by
//! `loomd`; in tests it is an in-process [`tokio::io::duplex`](tokio::io::duplex) pipe —
//! the same `WebSocket` handshake and framing on both, with no real network and no GPU.

use futures_util::{SinkExt, StreamExt};
use loom_proto::{
    Envelope,
    codec::{Channel, decode_message, decode_wss_frame, wss_frame},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

use crate::session::{InboundFrame, Session, SessionError};

use async_trait::async_trait;

/// A framed `WebSocket` agent session over an arbitrary byte stream `S`.
pub struct WssSession<S> {
    ws: WebSocketStream<S>,
}

impl<S> std::fmt::Debug for WssSession<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WssSession").finish_non_exhaustive()
    }
}

impl<S> WssSession<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Wraps an already-established [`WebSocketStream`] as a session.
    #[must_use]
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self { ws }
    }

    /// Completes the *server* side of the `WebSocket` handshake over `stream` and returns
    /// the accepted session — what `loomd` calls once it has a `TLS`-terminated `TCP`
    /// stream from an agent.
    ///
    /// # Errors
    /// [`SessionError::WebSocket`] if the handshake fails.
    pub async fn accept(stream: S) -> Result<Self, SessionError> {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        Ok(Self::new(ws))
    }

    /// Completes the *client* side of the `WebSocket` handshake over `stream` — used by a
    /// fake agent to drive the real terminator in-process.
    ///
    /// # Errors
    /// [`SessionError::WebSocket`] if the handshake fails.
    pub async fn connect(stream: S) -> Result<Self, SessionError> {
        let (ws, _resp) =
            tokio_tungstenite::client_async("ws://loom.invalid/agent", stream).await?;
        Ok(Self::new(ws))
    }
}

#[async_trait]
impl<S> Session for WssSession<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn recv(&mut self) -> Result<InboundFrame, SessionError> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    let (channel, payload, rest) = decode_wss_frame(&bytes)?;
                    if !rest.is_empty() {
                        return Err(SessionError::Protocol(format!(
                            "{} trailing bytes after control frame",
                            rest.len()
                        )));
                    }
                    let envelope: Envelope = decode_message(payload)?;
                    return Ok(InboundFrame { channel, envelope });
                }
                // Keepalives: tungstenite answers pings itself; ignore both. `Frame` is a
                // raw-write helper never yielded by a read stream, but the match must be
                // exhaustive, so it is ignored the same way.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(frame))) => {
                    let reason = frame
                        .map(|f| f.reason.into_owned())
                        .filter(|r| !r.is_empty());
                    return Err(SessionError::Closed { reason });
                }
                None => return Err(SessionError::Closed { reason: None }),
                Some(Ok(Message::Text(_))) => {
                    return Err(SessionError::Protocol(
                        "unexpected text frame on binary control channel".to_owned(),
                    ));
                }
                Some(Err(e)) => return Err(SessionError::from(e)),
            }
        }
    }

    async fn send(&mut self, channel: Channel, envelope: &Envelope) -> Result<(), SessionError> {
        let frame = wss_frame(channel, envelope)?;
        self.ws.send(Message::Binary(frame)).await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), SessionError> {
        self.ws.close(None).await?;
        Ok(())
    }

    async fn close_with_reason(&mut self, reason: &str) -> Result<(), SessionError> {
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
    use super::WssSession;
    use crate::session::Session;
    use loom_proto::{
        Body,
        codec::Channel,
        v1::{Envelope, Heartbeat},
    };

    async fn duplex_pair() -> (
        WssSession<tokio::io::DuplexStream>,
        WssSession<tokio::io::DuplexStream>,
    ) {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server =
            tokio::spawn(async move { WssSession::accept(server_io).await.expect("accept") });
        let client = WssSession::connect(client_io).await.expect("connect");
        let server = server.await.expect("server task");
        (client, server)
    }

    fn heartbeat_envelope() -> Envelope {
        Envelope {
            protocol_version: 1,
            msg_id: "01J0000000000000000000BEAT".to_owned(),
            correlation_id: String::new(),
            timestamp_ms: 1_700_000_000_000,
            body: Some(Body::Heartbeat(Heartbeat::default())),
        }
    }

    #[tokio::test]
    async fn a_frame_round_trips_over_a_real_handshake() {
        let (mut client, mut server) = duplex_pair().await;
        let sent = heartbeat_envelope();
        client
            .send(Channel::Heartbeat, &sent)
            .await
            .expect("client send");
        let frame = server.recv().await.expect("server recv");
        assert_eq!(frame.channel, Channel::Heartbeat);
        assert_eq!(frame.envelope, sent);
    }

    #[tokio::test]
    async fn a_client_close_surfaces_as_closed() {
        let (mut client, mut server) = duplex_pair().await;
        client.close().await.expect("client close");
        let err = server.recv().await.expect_err("server sees close");
        assert!(err.is_clean_close());
    }
}
