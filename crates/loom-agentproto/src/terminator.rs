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
//! A connection arrives one of two ways (agent-protocol.md §1.2):
//!
//! - **already authenticated** ([`PeerIdentity::Node`]) — its client certificate was
//!   validated at the `TLS` layer and the `agent_id` read from the subject; the terminator
//!   goes straight to bridging;
//! - **token-only** ([`PeerIdentity::Bootstrap`]) — no certificate yet. The terminator
//!   runs the enrollment handshake through its [`Enroller`]: it expects one
//!   `EnrollRequest`, verifies the token against the [`bootstrap`](crate::bootstrap) `CA`
//!   machinery, returns a signed node certificate in an `EnrollGrant`, and then bridges
//!   the now-identified connection. A terminator built without an enroller
//!   ([`new`](SessionTerminator::new)) refuses token-only connections outright.

use std::sync::Arc;

use loom_bus::Bus;
use loom_proto::codec::Channel;
use loom_proto::{Body, Envelope};
use tracing::warn;

use crate::bridge::{BridgeError, BusBridge};
use crate::enroll::{EnrollError, Enroller};
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

    /// Enrollment of a token-only connection failed (the agent was refused).
    #[error("enroll: {0}")]
    Enroll(#[from] EnrollError),

    /// The connection violated the terminator's expectations for its identity — e.g. a
    /// token-only connection whose first message was not an `EnrollRequest`, or one that
    /// reached a terminator built without an enrollment handler.
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

/// Drives agent sessions: demux, decode, and bridge onto the bus, running enrollment for
/// token-only connections when configured with an [`Enroller`].
#[derive(Clone)]
pub struct SessionTerminator {
    bridge: BusBridge,
    enroller: Option<Arc<Enroller>>,
}

impl std::fmt::Debug for SessionTerminator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTerminator")
            .field("enrollment", &self.enroller.is_some())
            .finish_non_exhaustive()
    }
}

impl SessionTerminator {
    /// Builds a bridge-only terminator over `bus`. It serves already-authenticated
    /// connections; a token-only connection is refused, since it has no enroller.
    #[must_use]
    pub fn new(bus: Arc<dyn Bus>) -> Self {
        Self {
            bridge: BusBridge::new(bus),
            enroller: None,
        }
    }

    /// Builds a terminator that also enrolls token-only connections through `enroller`.
    #[must_use]
    pub fn with_enrollment(bus: Arc<dyn Bus>, enroller: Enroller) -> Self {
        Self {
            bridge: BusBridge::new(bus),
            enroller: Some(Arc::new(enroller)),
        }
    }

    /// Serves one agent `session` under the connection's `identity` until the agent
    /// closes, bridging every decoded message onto the bus.
    ///
    /// A token-only [`PeerIdentity::Bootstrap`] connection first runs the enrollment
    /// handshake (see the module docs); an already-authenticated [`PeerIdentity::Node`]
    /// goes straight to bridging.
    ///
    /// # Errors
    /// [`TerminatorError::Enroll`] if a token-only connection is refused;
    /// [`TerminatorError::Protocol`] if a token-only connection misbehaves or reaches a
    /// terminator with no enroller; [`TerminatorError::Session`] on a transport/codec
    /// failure that is not a clean close; [`TerminatorError::Bridge`] if an event cannot
    /// be published.
    pub async fn serve<S>(
        &self,
        mut session: S,
        identity: PeerIdentity,
    ) -> Result<ServeSummary, TerminatorError>
    where
        S: Session + Send,
    {
        // Resolve the connection's agent_id: present already for a Node identity, or minted
        // by the enrollment handshake for a token-only Bootstrap one.
        let (agent_id, mut events) = match identity.agent_id() {
            Some(id) => (id.to_owned(), 0),
            None => (self.enroll_bootstrap(&mut session).await?, 1),
        };

        loop {
            match session.recv().await {
                Ok(frame) => {
                    if self.bridge_frame(&agent_id, &frame).await? {
                        events += 1;
                    }
                }
                // A clean or reasoned close ends the session normally.
                Err(SessionError::Closed { .. }) => break,
                Err(other) => return Err(other.into()),
            }
        }

        Ok(ServeSummary {
            agent_id: Some(agent_id),
            events_bridged: events,
        })
    }

    /// Runs the enrollment handshake on a token-only connection: expect one control-lane
    /// `EnrollRequest`, verify + sign through the [`Enroller`], reply with the
    /// `EnrollGrant`, publish `agent.enrolled`, and return the granted `agent_id`.
    async fn enroll_bootstrap<S>(&self, session: &mut S) -> Result<String, TerminatorError>
    where
        S: Session + Send,
    {
        let Some(enroller) = self.enroller.as_ref() else {
            let _ = session.close_with_reason("enroll_unsupported").await;
            return Err(TerminatorError::Protocol(
                "token-only connection, but this terminator has no enrollment handler".to_owned(),
            ));
        };

        let frame = session.recv().await?;
        let Some(Body::EnrollRequest(request)) = frame.envelope.body else {
            let _ = session.close_with_reason("expected_enroll").await;
            return Err(TerminatorError::Protocol(
                "token-only connection's first message was not an EnrollRequest".to_owned(),
            ));
        };
        let correlation_id = frame.envelope.msg_id;

        let grant = match enroller.enroll(&request).await {
            Ok(grant) => grant,
            Err(e) => {
                // Tell the agent why, in a reason code that leaks no gateway internals.
                let _ = session.close_with_reason(e.close_reason()).await;
                return Err(TerminatorError::Enroll(e));
            }
        };

        let grant_envelope = Envelope {
            protocol_version: grant.chosen_version,
            msg_id: mint_msg_id(),
            correlation_id,
            timestamp_ms: grant.issued_at.as_millis(),
            body: Some(Body::EnrollGrant(grant.to_enroll_grant())),
        };
        session.send(Channel::Control, &grant_envelope).await?;

        // Announce the enrolled node on the bus under its fenced identity.
        let event = AgentEvent::enrolled(&grant.agent_id, &grant_envelope.msg_id);
        self.bridge.publish(&event).await?;

        Ok(grant.agent_id)
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

/// Mints a `msg_id` for a gateway-originated envelope (the `EnrollGrant`). Uniqueness is
/// best-effort — the field is advisory (agent-protocol.md §2.2) — so an unavailable random
/// source falls back to a zero id rather than failing the connection.
fn mint_msg_id() -> String {
    let mut buf = [0u8; 12];
    let _ = getrandom::fill(&mut buf);
    format!("grant_{}", hex::encode(buf))
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
