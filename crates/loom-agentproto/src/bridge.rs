// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`BusBridge`] — publishes decoded [`AgentEvent`]s onto the [`loom_bus`].
//!
//! This is the seam between the wire and the rest of the control plane: the terminator
//! decodes an agent message, and the bridge lands it on the bus for the scheduler and
//! observers to react to (control-plane §1). The bus is a best-effort, at-least-once
//! *hint*, never the source of truth (ADR-0013); a dropped nudge is recovered by
//! reconciling against the store, so the bridge only fails on a genuine transport error.

use std::sync::Arc;

use loom_bus::Bus;

use crate::event::AgentEvent;

/// A failure publishing an agent event to the bus.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// The event could not be serialized to its bus payload.
    #[error("serialize agent event: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The underlying bus transport failed.
    #[error("bus: {0}")]
    Bus(#[from] loom_bus::BusError),
}

/// Publishes [`AgentEvent`]s onto a shared [`Bus`].
#[derive(Clone)]
pub struct BusBridge {
    bus: Arc<dyn Bus>,
}

impl std::fmt::Debug for BusBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusBridge").finish_non_exhaustive()
    }
}

impl BusBridge {
    /// Wraps a shared bus.
    #[must_use]
    pub fn new(bus: Arc<dyn Bus>) -> Self {
        Self { bus }
    }

    /// Publishes `event` on its `agent.<subject>` topic.
    ///
    /// # Errors
    /// [`BridgeError::Serialize`] if the event cannot be rendered to JSON;
    /// [`BridgeError::Bus`] on a bus transport failure.
    pub async fn publish(&self, event: &AgentEvent) -> Result<(), BridgeError> {
        let message = event.to_message()?;
        self.bus.publish(message).await?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;
    use crate::event::AgentEventKind;
    use loom_bus::{InProcBus, Topic};

    #[tokio::test]
    async fn a_published_event_lands_on_the_agent_family() {
        let bus = InProcBus::new();
        let mut sub = bus
            .subscribe(Topic::agent_events())
            .await
            .expect("subscribe");
        let bridge = BusBridge::new(Arc::new(bus));

        let event = AgentEvent::enrolled("agent-1", "01J0000000000000000000ENRL");
        bridge.publish(&event).await.expect("publish");

        let msg = sub.recv().await.expect("delivered");
        assert_eq!(msg.topic, Topic::new("agent.enrolled"));
        let decoded: AgentEvent = serde_json::from_slice(&msg.payload).expect("json");
        assert_eq!(decoded.agent_id, "agent-1");
        assert_eq!(decoded.kind, AgentEventKind::Enrolled);
    }
}
