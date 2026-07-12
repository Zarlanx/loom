// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`SessionTerminator`] — the server-side agent-gateway loop.
//!
//! It drives one [`Session`], demuxes the four wire streams (agent-protocol.md §1.4),
//! decodes each [`Envelope`], and bridges it onto the [`loom_bus`] as an
//! [`AgentEvent`](crate::event::AgentEvent) stamped with the connection's authenticated
//! `agent_id`. That identity is the *connection's*, never a field in the message, so an
//! agent's events can never be attributed to another node (§5 fencing, §6).
//!
//! This is the PR-09a terminator: it bridges an already-authenticated connection.
//! Token-only enrollment — a bootstrap connection presenting an `EnrollRequest`, verified
//! against the [`bootstrap`](crate::bootstrap) `CA`/token machinery — lands in PR-09b.

use std::sync::Arc;

use loom_bus::Bus;
use loom_proto::Body;
use tracing::warn;

use crate::bridge::{BridgeError, BusBridge};
use crate::event::AgentEvent;
use crate::session::{InboundFrame, PeerIdentity, Session, SessionError};

/// A failure driving an agent session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TerminatorError {
    /// The underlying session failed (transport, codec, or protocol).
    #[error("session: {0}")]
    Session(#[from] SessionError),

    /// Publishing a decoded event to the bus failed.
    #[error("bridge: {0}")]
    Bridge(#[from] BridgeError),

    /// The connection violated the terminator's expectations for its identity (e.g. a
    /// token-only bootstrap connection reached the bridge-only PR-09a terminator, which
    /// has no enrollment handler).
    #[error("protocol: {0}")]
    Protocol(String),
}

/// What a completed [`serve`](SessionTerminator::serve) run did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServeSummary {
    /// The authenticated agent this session belonged to, once known.
    pub agent_id: Option<String>,
    /// How many decoded messages were bridged onto the bus.
    pub events_bridged: usize,
}

/// Drives agent sessions: demux, decode, and bridge onto the bus.
#[derive(Clone)]
pub struct SessionTerminator {
    bridge: BusBridge,
}

impl std::fmt::Debug for SessionTerminator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTerminator").finish_non_exhaustive()
    }
}

impl SessionTerminator {
    /// Builds a terminator publishing decoded events onto `bus`.
    #[must_use]
    pub fn new(bus: Arc<dyn Bus>) -> Self {
        Self {
            bridge: BusBridge::new(bus),
        }
    }

    /// Serves one agent `session` under the connection's `identity` until the agent
    /// closes, bridging every decoded message onto the bus.
    ///
    /// PR-09a requires an already-authenticated [`PeerIdentity::Node`]; a token-only
    /// [`PeerIdentity::Bootstrap`] connection is closed with `enroll_unsupported`, since
    /// enrollment lands in PR-09b.
    ///
    /// # Errors
    /// [`TerminatorError::Protocol`] if a token-only connection reaches this bridge-only
    /// terminator; [`TerminatorError::Session`] on a transport/codec failure that is not
    /// a clean close; [`TerminatorError::Bridge`] if an event cannot be published.
    pub async fn serve<S>(
        &self,
        mut session: S,
        identity: PeerIdentity,
    ) -> Result<ServeSummary, TerminatorError>
    where
        S: Session + Send,
    {
        let Some(agent_id) = identity.agent_id().map(str::to_owned) else {
            // A bootstrap connection has no enrollment handler yet (PR-09b).
            let _ = session.close_with_reason("enroll_unsupported").await;
            return Err(TerminatorError::Protocol(
                "token-only bootstrap connection, but this terminator has no enrollment handler"
                    .to_owned(),
            ));
        };

        let mut summary = ServeSummary {
            agent_id: Some(agent_id.clone()),
            events_bridged: 0,
        };

        loop {
            match session.recv().await {
                Ok(frame) => {
                    if self.bridge_frame(&agent_id, &frame).await? {
                        summary.events_bridged += 1;
                    }
                }
                // A clean or reasoned close ends the session normally.
                Err(SessionError::Closed { .. }) => break,
                Err(other) => return Err(other.into()),
            }
        }

        Ok(summary)
    }

    /// Demuxes one inbound frame and, if it is a bridgeable agent message, publishes it.
    /// Returns whether an event was bridged.
    async fn bridge_frame(
        &self,
        agent_id: &str,
        frame: &InboundFrame,
    ) -> Result<bool, TerminatorError> {
        // An authenticated connection re-presenting an EnrollRequest is already enrolled;
        // ignore it rather than re-issuing a certificate (agent-protocol.md §4).
        if matches!(frame.envelope.body, Some(Body::EnrollRequest(_))) {
            warn!(
                agent_id,
                "ignoring EnrollRequest on an already-authenticated session"
            );
            return Ok(false);
        }

        let Some(event) = AgentEvent::from_envelope(agent_id, &frame.envelope) else {
            return Ok(false);
        };

        // The channel is a routing hint; the decoded body is authoritative. A message on
        // the wrong lane is bridged anyway but flagged — a misbehaving or buggy agent.
        if frame.channel != event.kind.expected_channel() {
            warn!(
                agent_id,
                got = ?frame.channel,
                want = ?event.kind.expected_channel(),
                subject = event.kind.subject(),
                "agent message arrived on an unexpected lane",
            );
        }

        self.bridge.publish(&event).await?;
        Ok(true)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;
    use crate::session::PeerIdentity;
    use crate::testing::{FakeAgent, loopback};
    use loom_bus::{InProcBus, Topic};
    use loom_proto::Body;
    use loom_proto::codec::Channel;
    use loom_proto::v1::{Heartbeat, JobAccept};

    #[tokio::test]
    async fn an_authenticated_session_bridges_messages_onto_the_bus() {
        let bus = Arc::new(InProcBus::new());
        let mut sub = bus
            .subscribe(Topic::agent_events())
            .await
            .expect("subscribe");

        let terminator = SessionTerminator::new(bus.clone());
        let (server, client) = loopback().await;
        let serve_task = tokio::spawn(async move {
            terminator
                .serve(server, PeerIdentity::node("agent-42"))
                .await
        });

        let mut agent = FakeAgent::new(client);
        agent
            .send(Channel::Heartbeat, Body::Heartbeat(Heartbeat::default()))
            .await;
        agent
            .send(
                Channel::Control,
                Body::JobAccept(JobAccept {
                    attempt_id: "at-1".to_owned(),
                }),
            )
            .await;
        agent.close().await;

        let summary = serve_task.await.expect("join").expect("serve");
        assert_eq!(summary.agent_id.as_deref(), Some("agent-42"));
        assert_eq!(summary.events_bridged, 2);

        let first: AgentEvent =
            serde_json::from_slice(&sub.recv().await.expect("e1").payload).expect("json");
        let second: AgentEvent =
            serde_json::from_slice(&sub.recv().await.expect("e2").payload).expect("json");
        // Both events carry the connection's fenced identity, not anything self-asserted.
        assert_eq!(first.agent_id, "agent-42");
        assert_eq!(second.agent_id, "agent-42");
        assert_eq!(second.attempt_id.as_deref(), Some("at-1"));
    }

    #[tokio::test]
    async fn a_bootstrap_connection_is_refused_by_the_bridge_only_terminator() {
        let bus = Arc::new(InProcBus::new());
        let terminator = SessionTerminator::new(bus);
        let (server, client) = loopback().await;
        let serve_task =
            tokio::spawn(async move { terminator.serve(server, PeerIdentity::Bootstrap).await });
        // Keep the client alive until the server has refused, then drop it.
        let err = serve_task
            .await
            .expect("join")
            .expect_err("bootstrap refused");
        assert!(matches!(err, TerminatorError::Protocol(_)));
        drop(client);
    }
}
