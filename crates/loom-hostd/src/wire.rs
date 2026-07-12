// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Envelope construction over the frozen `loom-proto` schema (`agent-protocol.md` §2.2).
//!
//! Small helpers that wrap a `oneof` body in an [`Envelope`] with a fresh per-connection
//! `msg_id`, the schema major, and an advisory timestamp. Framing itself lives in
//! `loom-proto`'s codec; this module only builds the messages that ride it.

use std::sync::atomic::{AtomicU64, Ordering};

use loom_proto::{Body, Envelope, v1::VersionRange};

/// The schema major this agent speaks (`agent-protocol.md` §2.3). M1 is major 1.
pub const PROTOCOL_VERSION: u32 = 1;

/// The version range the agent advertises at enrollment and on every reconnect
/// `StateReport` (`agent-protocol.md` §2.3). M1 supports exactly major 1.
#[must_use]
pub const fn supported_versions() -> VersionRange {
    VersionRange {
        min: PROTOCOL_VERSION,
        max: PROTOCOL_VERSION,
    }
}

/// Mints monotonic `msg_id`s — unique per sender per connection (`agent-protocol.md` §2.2).
///
/// A real ULID is not required for correctness (the schema treats `msg_id` as an opaque
/// string); a monotonic counter is deterministic for tests and still unique within a
/// connection. A fresh generator is created per connection, so ids restart at 0 — matching
/// the "per connection" scope exactly.
#[derive(Debug, Default)]
pub struct MsgIdGen {
    counter: AtomicU64,
}

impl MsgIdGen {
    /// A generator starting from 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    /// Returns the next id, e.g. `MSG00000000000000000000042` (26 chars, ULID-shaped).
    #[must_use]
    pub fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("MSG{n:023}")
    }
}

/// Wraps `body` in an `Envelope` with the given ids and advisory timestamp.
#[must_use]
pub fn envelope(msg_id: String, correlation_id: String, timestamp_ms: i64, body: Body) -> Envelope {
    Envelope {
        protocol_version: PROTOCOL_VERSION,
        msg_id,
        correlation_id,
        timestamp_ms,
        body: Some(body),
    }
}

#[cfg(test)]
mod tests {
    use super::{MsgIdGen, PROTOCOL_VERSION, envelope, supported_versions};
    use loom_proto::{Body, v1::JobAccept};

    #[test]
    fn msg_ids_are_monotonic_and_fixed_width() {
        let ids = MsgIdGen::new();
        let a = ids.next_id();
        let b = ids.next_id();
        assert_eq!(a.len(), 26);
        assert_eq!(b.len(), 26);
        assert_ne!(a, b);
        assert!(a < b, "ids order monotonically as strings");
    }

    #[test]
    fn supported_range_is_m1_only() {
        let r = supported_versions();
        assert_eq!(r.min, PROTOCOL_VERSION);
        assert_eq!(r.max, PROTOCOL_VERSION);
    }

    #[test]
    fn envelope_carries_body_and_version() {
        let env = envelope(
            "MSG1".to_string(),
            "MSG0".to_string(),
            123,
            Body::JobAccept(JobAccept {
                attempt_id: "at1".to_string(),
            }),
        );
        assert_eq!(env.protocol_version, PROTOCOL_VERSION);
        assert_eq!(env.correlation_id, "MSG0");
        assert_eq!(env.timestamp_ms, 123);
        assert!(matches!(env.body, Some(Body::JobAccept(_))));
    }
}
