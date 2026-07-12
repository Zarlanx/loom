// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The QUIC transport scaffold: how `quinn` slots behind the same
//! [`Session`](crate::session::Session) seam once `loomd` wires the endpoint.
//!
//! QUIC is the *preferred* control-channel transport (agent-protocol.md §1.1): the agent
//! attempts QUIC over UDP/443 via `quinn` first and falls back to the
//! [WSS baseline](crate::wss) only on a UDP-hostile middlebox. Both terminate at this
//! crate's [`Session`](crate::session::Session) seam; the terminator is transport-blind.
//!
//! This module is a **scaffold**, not a live transport. It fixes the two contracts a
//! concrete `QuicSession` must honor — the [ALPN token](ALPN_PROTOCOLS) and the
//! [stream layout](ChannelStream) — and states where the endpoint itself is built. The
//! endpoint (a `quinn::Endpoint` with a `rustls` server config pinning the Loom `CA` for
//! `mTLS` client auth, 0-RTT disabled, connection migration enabled) is owned by `loomd`,
//! which already links `rustls`/`quinn`; standing it up here would drag the aws-lc-rs C
//! build and the platform certificate verifier into a crate whose gate is a pure
//! in-process WSS loopback, buying nothing. So the concrete `QuicSession` lands with that
//! endpoint wiring (PR-11/PR-21) — this module is the seam it plugs into.
//!
//! # Stream layout (agent-protocol.md §1.4)
//!
//! On QUIC each logical channel is a real, independently flow-controlled transport
//! stream, so — unlike the WSS fallback — the one-byte channel tag is *omitted*: the
//! stream identity *is* the channel. A slow log upload then cannot head-of-line-block a
//! heartbeat, the property the fallback gives up.

use loom_proto::codec::Channel;

/// The QUIC ALPN protocol list offered in the `TLS` `ClientHello` and pinned by the
/// gateway (agent-protocol.md §1.1, §8): a single entry, the wire-framing identifier
/// [`WIRE_PROTOCOL_ID`](crate::session::WIRE_PROTOCOL_ID) as bytes. The gateway rejects a
/// mismatched ALPN early.
pub const ALPN_PROTOCOLS: &[&[u8]] = &[b"loom/1"];

/// How a [`Channel`] maps onto a QUIC stream (agent-protocol.md §1.4).
///
/// The control channel is **bidirectional** (agent opens it; the gateway multiplexes its
/// own RPC back over the agent-opened stream, staying outbound-consistent). The other
/// three are **unidirectional** agent→gateway streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelStream {
    /// A bidirectional stream the agent opens: request/response plus gateway→agent RPC.
    Bidirectional,
    /// A unidirectional agent→gateway stream.
    UnidirectionalToGateway,
}

impl ChannelStream {
    /// The QUIC stream kind carrying `channel`.
    #[must_use]
    pub const fn for_channel(channel: Channel) -> Self {
        match channel {
            Channel::Control => Self::Bidirectional,
            Channel::Heartbeat | Channel::Log | Channel::Metering => Self::UnidirectionalToGateway,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ALPN_PROTOCOLS, ChannelStream};
    use crate::session::WIRE_PROTOCOL_ID;
    use loom_proto::codec::Channel;

    #[test]
    fn alpn_advertises_the_wire_protocol_id() {
        assert_eq!(ALPN_PROTOCOLS, &[WIRE_PROTOCOL_ID.as_bytes()]);
    }

    #[test]
    fn only_control_is_bidirectional() {
        assert_eq!(
            ChannelStream::for_channel(Channel::Control),
            ChannelStream::Bidirectional
        );
        for channel in [Channel::Heartbeat, Channel::Log, Channel::Metering] {
            assert_eq!(
                ChannelStream::for_channel(channel),
                ChannelStream::UnidirectionalToGateway
            );
        }
    }
}
