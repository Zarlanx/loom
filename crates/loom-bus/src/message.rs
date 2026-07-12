// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Message`] — one published bus event: a [`Topic`] plus an opaque payload.

use std::sync::Arc;

use loom_store::OutboxEvent;

use crate::topic::Topic;

/// A published bus event.
///
/// The payload is an opaque `Arc<[u8]>` so a single publish fans out to many
/// subscribers with only a refcount bump per delivery — it holds JSON text for
/// job-lifecycle nudges and encoded protobuf for agent events alike.
///
/// [`dedup_key`](Message::dedup_key), when set, is a stable publisher-side
/// identity: every redelivery of the same `outbox` row carries the *same* key, so
/// a consumer can dedup and make its effect idempotent (exactly-once *effects*
/// over at-least-once delivery — the `JetStream` discipline in a smaller form,
/// control-plane §3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The subject the message is published under (e.g. `job.scheduled`).
    pub topic: Topic,
    /// The opaque serialized payload.
    pub payload: Arc<[u8]>,
    /// A stable dedup identity, if the publisher assigned one.
    pub dedup_key: Option<String>,
}

impl Message {
    /// Builds a message on `topic` with the given payload and no dedup key.
    #[must_use]
    pub fn new(topic: Topic, payload: impl Into<Arc<[u8]>>) -> Self {
        Self {
            topic,
            payload: payload.into(),
            dedup_key: None,
        }
    }

    /// Builds a message whose payload is the UTF-8 bytes of `text`.
    #[must_use]
    pub fn text(topic: Topic, text: &str) -> Self {
        Self::new(topic, Arc::<[u8]>::from(text.as_bytes()))
    }

    /// Attaches a dedup key, returning the updated message.
    #[must_use]
    pub fn with_dedup_key(mut self, key: impl Into<String>) -> Self {
        self.dedup_key = Some(key.into());
        self
    }

    /// Builds the bus message for an `outbox` row: the row's `topic` verbatim, its
    /// payload bytes, and a dedup key derived from the row id so every redelivery
    /// is recognizably the same event.
    #[must_use]
    pub fn from_outbox(event: &OutboxEvent) -> Self {
        Self {
            topic: Topic::new(event.topic.clone()),
            payload: Arc::<[u8]>::from(event.payload.as_bytes()),
            dedup_key: Some(format!("outbox-{}", event.id.0)),
        }
    }

    /// The payload interpreted as UTF-8 text.
    ///
    /// # Errors
    /// [`core::str::Utf8Error`] if the payload is not valid UTF-8.
    pub fn payload_utf8(&self) -> Result<&str, core::str::Utf8Error> {
        core::str::from_utf8(&self.payload)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips_through_payload_utf8() {
        let msg = Message::text(Topic::new("job.scheduled"), r#"{"job":"j1"}"#);
        assert_eq!(msg.payload_utf8().expect("utf8"), r#"{"job":"j1"}"#);
        assert!(msg.dedup_key.is_none());
    }

    #[test]
    fn with_dedup_key_sets_identity() {
        let msg = Message::text(Topic::new("job.scheduled"), "{}").with_dedup_key("k1");
        assert_eq!(msg.dedup_key.as_deref(), Some("k1"));
    }
}
