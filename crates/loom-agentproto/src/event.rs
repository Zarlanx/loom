// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`AgentEvent`] — the decoded agent message the terminator republishes on the
//! [`loom_bus`], under the `agent.*` family (control-plane §1).
//!
//! The wire carries protobuf [`Envelope`]s; the bus carries these summaries. Each event
//! is stamped with the connection's *authenticated* `agent_id` — never a self-asserted
//! field in the message — so a downstream consumer (the scheduler reconciling attempt
//! state, control-plane §3) always knows which enrolled node an event truly came from
//! and a message can never speak for another agent (agent-protocol.md §5, §6).
//!
//! Events serialize to compact JSON, matching the bus's "human-debuggable JSON in
//! tooling" convention (agent-protocol.md §2.1): the wire stays binary, the bus stays
//! inspectable. The full envelope is handled by the terminator; the bus event is the
//! low-latency *nudge* consumers reconcile against the store from (ADR-0013).

use loom_bus::{Message, Topic};
use loom_proto::{Body, Envelope, codec::Channel};
use serde::{Deserialize, Serialize};

/// The kind of an [`AgentEvent`] — one variant per bridged agent→gateway message.
///
/// Gateway→agent messages (`EnrollGrant`, `ReEnroll`, `JobOffer`, `JobAbort`) are not
/// bridged: they originate here, not at the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    /// A node completed enrollment and holds a signed certificate (emitted by the
    /// terminator's enrollment handler, not decoded from an agent message).
    Enrolled,
    /// Presence/health heartbeat (heartbeat lane).
    Heartbeat,
    /// Full state resync after reconnect (control lane).
    StateReport,
    /// The agent accepted a job offer.
    JobAccept,
    /// The agent rejected a job offer.
    JobReject,
    /// Image/weights preparation progress.
    JobPrepareProgress,
    /// The workload started running.
    JobStarted,
    /// The attempt completed.
    JobCompleted,
    /// The attempt failed.
    JobFailed,
    /// A workload log chunk (log lane).
    Log,
}

impl AgentEventKind {
    /// The topic leaf under the `agent` family — e.g. `heartbeat` in `agent.heartbeat`.
    #[must_use]
    pub const fn subject(self) -> &'static str {
        match self {
            Self::Enrolled => "enrolled",
            Self::Heartbeat => "heartbeat",
            Self::StateReport => "state_report",
            Self::JobAccept => "job_accept",
            Self::JobReject => "job_reject",
            Self::JobPrepareProgress => "job_prepare_progress",
            Self::JobStarted => "job_started",
            Self::JobCompleted => "job_completed",
            Self::JobFailed => "job_failed",
            Self::Log => "log",
        }
    }

    /// The logical wire lane this kind is expected to arrive on (agent-protocol.md §1.4).
    /// Everything but the heartbeat and log lanes rides the bidirectional control lane.
    #[must_use]
    pub const fn expected_channel(self) -> Channel {
        match self {
            Self::Heartbeat => Channel::Heartbeat,
            Self::Log => Channel::Log,
            _ => Channel::Control,
        }
    }
}

/// A decoded agent message, ready to publish on the bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    /// The connection's authenticated `agent_id` — the fenced identity (§5, §6).
    pub agent_id: String,
    /// Which kind of message this is.
    pub kind: AgentEventKind,
    /// The originating envelope's `msg_id` (`ULID`, unique per sender per connection).
    pub msg_id: String,
    /// The envelope's `correlation_id`, if it answered a prior message.
    pub correlation_id: String,
    /// The attempt this event concerns, for the job/log lifecycle messages.
    pub attempt_id: Option<String>,
}

impl AgentEvent {
    /// Builds the `agent.enrolled` event the terminator publishes once a node's
    /// certificate is issued.
    #[must_use]
    pub fn enrolled(agent_id: impl Into<String>, msg_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            kind: AgentEventKind::Enrolled,
            msg_id: msg_id.into(),
            correlation_id: String::new(),
            attempt_id: None,
        }
    }

    /// Maps a decoded [`Envelope`] arriving from `agent_id` to a bridged event, or `None`
    /// for a body that is not bridged (a gateway→agent message echoed back, an
    /// enrollment request handled internally, or an empty body).
    #[must_use]
    pub fn from_envelope(agent_id: &str, envelope: &Envelope) -> Option<Self> {
        let (kind, attempt_id) = match envelope.body.as_ref()? {
            Body::Heartbeat(_) => (AgentEventKind::Heartbeat, None),
            Body::StateReport(_) => (AgentEventKind::StateReport, None),
            Body::JobAccept(m) => (AgentEventKind::JobAccept, Some(m.attempt_id.clone())),
            Body::JobReject(m) => (AgentEventKind::JobReject, Some(m.attempt_id.clone())),
            Body::JobPrepareProgress(m) => (
                AgentEventKind::JobPrepareProgress,
                Some(m.attempt_id.clone()),
            ),
            Body::JobStarted(m) => (AgentEventKind::JobStarted, Some(m.attempt_id.clone())),
            Body::JobCompleted(m) => (AgentEventKind::JobCompleted, Some(m.attempt_id.clone())),
            Body::JobFailed(m) => (AgentEventKind::JobFailed, Some(m.attempt_id.clone())),
            Body::LogChunk(m) => (AgentEventKind::Log, Some(m.attempt_id.clone())),
            // Not bridged: gateway→agent bodies, enrollment (handled by the terminator),
            // and cert rotation.
            Body::EnrollRequest(_)
            | Body::EnrollGrant(_)
            | Body::ReEnroll(_)
            | Body::RotateCert(_)
            | Body::JobOffer(_)
            | Body::JobAbort(_) => return None,
        };
        Some(Self {
            agent_id: agent_id.to_owned(),
            kind,
            msg_id: envelope.msg_id.clone(),
            correlation_id: envelope.correlation_id.clone(),
            attempt_id,
        })
    }

    /// The bus topic this event publishes under — `agent.<subject>`.
    #[must_use]
    pub fn topic(&self) -> Topic {
        Topic::new(format!("agent.{}", self.kind.subject()))
    }

    /// Renders this event as a bus [`Message`]: JSON payload on its [`topic`](Self::topic),
    /// with a per-`(agent, msg)` dedup key so a redelivered event is recognizably the same
    /// one (at-least-once → idempotent effects, ADR-0013).
    ///
    /// # Errors
    /// [`serde_json::Error`] if the event cannot be serialized (not expected for these
    /// plain-data fields).
    pub fn to_message(&self) -> Result<Message, serde_json::Error> {
        let payload = serde_json::to_vec(self)?;
        let dedup_key = format!("agent.{}.{}", self.agent_id, self.msg_id);
        Ok(Message::new(self.topic(), payload).with_dedup_key(dedup_key))
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;
    use loom_proto::v1::{Heartbeat, JobCompleted, JobOffer};

    fn envelope(body: Body) -> Envelope {
        Envelope {
            protocol_version: 1,
            msg_id: "01J0000000000000000000MSG0".to_owned(),
            correlation_id: "corr-1".to_owned(),
            timestamp_ms: 1_700_000_000_000,
            body: Some(body),
        }
    }

    #[test]
    fn a_heartbeat_maps_to_a_heartbeat_event_with_no_attempt() {
        let event =
            AgentEvent::from_envelope("agent-1", &envelope(Body::Heartbeat(Heartbeat::default())))
                .expect("bridged");
        assert_eq!(event.agent_id, "agent-1");
        assert_eq!(event.kind, AgentEventKind::Heartbeat);
        assert_eq!(event.attempt_id, None);
        assert_eq!(event.topic(), Topic::new("agent.heartbeat"));
    }

    #[test]
    fn a_job_completed_carries_its_attempt_id() {
        let body = Body::JobCompleted(JobCompleted {
            attempt_id: "at-9".to_owned(),
            exit_code: 0,
        });
        let event = AgentEvent::from_envelope("agent-1", &envelope(body)).expect("bridged");
        assert_eq!(event.kind, AgentEventKind::JobCompleted);
        assert_eq!(event.attempt_id.as_deref(), Some("at-9"));
        assert_eq!(event.kind.expected_channel(), Channel::Control);
    }

    #[test]
    fn a_gateway_to_agent_body_is_not_bridged() {
        let offer = Body::JobOffer(JobOffer::default());
        assert!(AgentEvent::from_envelope("agent-1", &envelope(offer)).is_none());
    }

    #[test]
    fn events_round_trip_through_their_bus_message_json() {
        let event = AgentEvent::enrolled("agent-1", "01J0000000000000000000ENRL");
        let msg = event.to_message().expect("to message");
        assert_eq!(msg.topic, Topic::new("agent.enrolled"));
        assert_eq!(
            msg.dedup_key.as_deref(),
            Some("agent.agent-1.01J0000000000000000000ENRL")
        );
        let decoded: AgentEvent = serde_json::from_slice(&msg.payload).expect("from json");
        assert_eq!(decoded, event);
    }
}
