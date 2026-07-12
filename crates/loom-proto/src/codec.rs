// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Length-prefix framing for the agent control channel (agent-protocol.md §2.1).
//!
//! Every message on the wire is a length-prefixed protobuf `Envelope`: a `u32`
//! big-endian byte length followed by the serialized bytes. On the QUIC transport the
//! stream itself is the channel, so a bare length-prefixed frame is used
//! ([`encode_frame`] / [`decode_frame`]). On the WSS fallback the four logical streams
//! share one TCP connection, so each frame is tagged with a one-byte [`Channel`]
//! ([`wss_frame`] / [`decode_wss_frame`]).
//!
//! This module is schema-independent: it frames arbitrary payload bytes and knows
//! nothing about the message catalog. The convenience helpers ([`encode_frame`],
//! [`decode_message`]) work over any [`prost::Message`].

use prost::Message;

/// Width of the big-endian length prefix, in bytes.
pub const LEN_PREFIX_BYTES: usize = 4;

/// Maximum accepted frame payload length.
///
/// The control channel is deliberately thin — bulk artifacts, checkpoints, and weights
/// never ride it (agent-protocol.md §3e) — so 8 MiB is comfortably above any legitimate
/// control message while still bounding a hostile length prefix.
pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;

/// The four logical streams multiplexed over a single WSS connection (agent-protocol.md §1.4).
///
/// On QUIC each stream is a real, independently flow-controlled transport stream, so the
/// tag is omitted. On the WSS fallback all four share one TCP stream: the tag only
/// identifies which logical stream a received frame belongs to — it does not isolate them,
/// so TCP head-of-line blocking is accepted as the fallback penalty and a slow log frame
/// can still delay a following heartbeat (agent-protocol.md §1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Bidirectional control: enrollment, job offers, config push, state reports.
    Control,
    /// Agent → gateway presence and health.
    Heartbeat,
    /// Agent → gateway log chunks (backpressured, may lag).
    Log,
    /// Agent → gateway usage records (durable, spooled).
    Metering,
}

impl Channel {
    /// Returns the one-byte wire tag for this channel.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Control => 0,
            Self::Heartbeat => 1,
            Self::Log => 2,
            Self::Metering => 3,
        }
    }

    /// Parses a channel from its one-byte wire tag.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::UnknownChannel`] if `tag` is not one of `0..=3`.
    pub const fn from_tag(tag: u8) -> Result<Self, CodecError> {
        match tag {
            0 => Ok(Self::Control),
            1 => Ok(Self::Heartbeat),
            2 => Ok(Self::Log),
            3 => Ok(Self::Metering),
            other => Err(CodecError::UnknownChannel(other)),
        }
    }
}

/// Errors from framing, deframing, and message (de)serialization.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The length prefix declares a payload larger than [`MAX_FRAME_LEN`].
    #[error("frame length {len} exceeds maximum {MAX_FRAME_LEN}")]
    FrameTooLarge {
        /// The declared payload length.
        len: usize,
    },
    /// The buffer is shorter than the frame it claims to contain.
    #[error("short buffer: need {need} bytes, have {have}")]
    ShortBuffer {
        /// Bytes required to read the frame.
        need: usize,
        /// Bytes actually available.
        have: usize,
    },
    /// A WSS frame carried a channel tag outside `0..=3`.
    #[error("unknown channel tag {0}")]
    UnknownChannel(u8),
    /// The payload bytes did not decode as the expected protobuf message.
    #[error("protobuf decode failed: {0}")]
    Decode(#[from] prost::DecodeError),
}

/// Frames a protobuf message as a bare length-prefixed frame (QUIC form).
///
/// The encoded length is checked against [`MAX_FRAME_LEN`] *before* the message is
/// serialized, so an oversized message is rejected without ever allocating its payload.
///
/// # Errors
///
/// Returns [`CodecError::FrameTooLarge`] if the encoded message exceeds [`MAX_FRAME_LEN`].
pub fn encode_frame<M: Message>(msg: &M) -> Result<Vec<u8>, CodecError> {
    let len = msg.encoded_len();
    if len > MAX_FRAME_LEN {
        return Err(CodecError::FrameTooLarge { len });
    }
    frame(&msg.encode_to_vec())
}

/// Frames raw payload bytes as a bare length-prefixed frame (QUIC form).
///
/// # Errors
///
/// Returns [`CodecError::FrameTooLarge`] if `payload` exceeds [`MAX_FRAME_LEN`].
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, CodecError> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(CodecError::FrameTooLarge { len: payload.len() });
    }
    // Total after the bound check: MAX_FRAME_LEN is far below `u32::MAX`, so `try_from`
    // never actually fails here — but express it fallibly rather than with a lossy cast.
    let len = u32::try_from(payload.len())
        .map_err(|_| CodecError::FrameTooLarge { len: payload.len() })?;
    let mut out = Vec::with_capacity(LEN_PREFIX_BYTES + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Reads one bare length-prefixed frame from the front of `buf`.
///
/// Returns the frame payload and the unconsumed remainder of `buf`.
///
/// # Errors
///
/// - [`CodecError::ShortBuffer`] if `buf` is too short to hold the length prefix or the
///   full declared payload.
/// - [`CodecError::FrameTooLarge`] if the declared length exceeds [`MAX_FRAME_LEN`].
pub fn decode_frame(buf: &[u8]) -> Result<(&[u8], &[u8]), CodecError> {
    let Some(len_bytes) = buf.get(..LEN_PREFIX_BYTES) else {
        return Err(CodecError::ShortBuffer {
            need: LEN_PREFIX_BYTES,
            have: buf.len(),
        });
    };
    let mut prefix = [0u8; LEN_PREFIX_BYTES];
    prefix.copy_from_slice(len_bytes);
    let len = u32::from_be_bytes(prefix) as usize;
    if len > MAX_FRAME_LEN {
        return Err(CodecError::FrameTooLarge { len });
    }
    let end = LEN_PREFIX_BYTES + len;
    let Some(payload) = buf.get(LEN_PREFIX_BYTES..end) else {
        return Err(CodecError::ShortBuffer {
            need: end,
            have: buf.len(),
        });
    };
    Ok((payload, &buf[end..]))
}

/// Frames a protobuf message as a channel-tagged WSS frame: `[tag][u32 len][payload]`.
///
/// As in [`encode_frame`], the encoded length is checked against [`MAX_FRAME_LEN`]
/// *before* serialization, so an oversized message never allocates its payload.
///
/// # Errors
///
/// Returns [`CodecError::FrameTooLarge`] if the encoded message exceeds [`MAX_FRAME_LEN`].
pub fn wss_frame<M: Message>(channel: Channel, msg: &M) -> Result<Vec<u8>, CodecError> {
    let len = msg.encoded_len();
    if len > MAX_FRAME_LEN {
        return Err(CodecError::FrameTooLarge { len });
    }
    let inner = frame(&msg.encode_to_vec())?;
    let mut out = Vec::with_capacity(1 + inner.len());
    out.push(channel.tag());
    out.extend_from_slice(&inner);
    Ok(out)
}

/// Reads one channel-tagged WSS frame from the front of `buf`.
///
/// Returns the decoded [`Channel`], the frame payload, and the unconsumed remainder.
///
/// # Errors
///
/// - [`CodecError::ShortBuffer`] if `buf` lacks the channel tag or the full frame.
/// - [`CodecError::UnknownChannel`] if the tag is outside `0..=3`.
/// - [`CodecError::FrameTooLarge`] if the declared length exceeds [`MAX_FRAME_LEN`].
pub fn decode_wss_frame(buf: &[u8]) -> Result<(Channel, &[u8], &[u8]), CodecError> {
    let Some((&tag, rest)) = buf.split_first() else {
        return Err(CodecError::ShortBuffer { need: 1, have: 0 });
    };
    let channel = Channel::from_tag(tag)?;
    let (payload, remainder) = decode_frame(rest)?;
    Ok((channel, payload, remainder))
}

/// Decodes a length-prefixed frame's payload as a concrete protobuf message.
///
/// # Errors
///
/// Returns [`CodecError::Decode`] if `payload` is not a valid encoding of `M`.
pub fn decode_message<M: Message + Default>(payload: &[u8]) -> Result<M, CodecError> {
    Ok(M::decode(payload)?)
}

#[cfg(test)]
mod tests {
    use super::{
        Channel, CodecError, MAX_FRAME_LEN, decode_frame, decode_message, decode_wss_frame,
        encode_frame, frame, wss_frame,
    };
    use crate::v1::Envelope;

    fn sample_envelope() -> Envelope {
        Envelope {
            protocol_version: 1,
            msg_id: "01J0000000000000000000CODE".to_string(),
            correlation_id: String::new(),
            timestamp_ms: 1_700_000_000_000,
            body: None,
        }
    }

    #[test]
    fn channel_tags_round_trip() {
        for channel in [
            Channel::Control,
            Channel::Heartbeat,
            Channel::Log,
            Channel::Metering,
        ] {
            let parsed = Channel::from_tag(channel.tag()).expect("known tag parses");
            assert_eq!(parsed, channel);
        }
        assert!(matches!(
            Channel::from_tag(4),
            Err(CodecError::UnknownChannel(4))
        ));
    }

    #[test]
    fn bare_frame_round_trips_and_leaves_remainder() {
        let payload = b"hello wire".as_slice();
        let mut framed = frame(payload).expect("frame");
        framed.extend_from_slice(b"trailing");

        let (got, rest) = decode_frame(&framed).expect("deframe");
        assert_eq!(got, payload);
        assert_eq!(rest, b"trailing");
    }

    #[test]
    fn wss_frame_carries_channel_and_message() {
        let envelope = sample_envelope();
        let framed = wss_frame(Channel::Control, &envelope).expect("wss frame");

        let (channel, payload, rest) = decode_wss_frame(&framed).expect("wss deframe");
        assert_eq!(channel, Channel::Control);
        assert!(rest.is_empty());

        let decoded: Envelope = decode_message(payload).expect("decode envelope");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn short_buffer_is_detected() {
        assert!(matches!(
            decode_frame(&[0, 0]),
            Err(CodecError::ShortBuffer { .. })
        ));
        // Declares 4 payload bytes but supplies only 1.
        assert!(matches!(
            decode_frame(&[0, 0, 0, 4, 0xAA]),
            Err(CodecError::ShortBuffer { .. })
        ));
    }

    #[test]
    fn oversized_length_prefix_is_rejected() {
        let huge = u32::try_from(MAX_FRAME_LEN + 1)
            .expect("MAX_FRAME_LEN + 1 fits in u32")
            .to_be_bytes();
        assert!(matches!(
            decode_frame(&huge),
            Err(CodecError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn malformed_payload_is_a_decode_error() {
        // Field 1, wire type 0 (varint) tag followed by a truncated varint: valid
        // framing but not a decodable `Envelope`.
        let payload = [0x08u8, 0xFF];
        assert!(matches!(
            decode_message::<Envelope>(&payload),
            Err(CodecError::Decode(_))
        ));
    }

    /// A `prost::Message` that reports one byte past [`MAX_FRAME_LEN`] but panics if
    /// anything ever tries to serialize it. If the preflight bound check runs before
    /// `encode_to_vec`, the panic is unreachable — proving oversized frames are rejected
    /// on `encoded_len` alone, without allocating or writing the payload.
    #[derive(Debug)]
    struct Oversized;

    impl prost::Message for Oversized {
        fn encoded_len(&self) -> usize {
            MAX_FRAME_LEN + 1
        }

        fn encode_raw(&self, _buf: &mut impl prost::bytes::BufMut) {
            panic!("preflight must reject before encoding");
        }

        fn merge_field(
            &mut self,
            _tag: u32,
            _wire_type: prost::encoding::WireType,
            _buf: &mut impl prost::bytes::Buf,
            _ctx: prost::encoding::DecodeContext,
        ) -> Result<(), prost::DecodeError> {
            unreachable!("the oversized fixture is never decoded");
        }

        fn clear(&mut self) {}
    }

    #[test]
    fn oversized_message_is_rejected_before_encoding() {
        assert!(matches!(
            encode_frame(&Oversized),
            Err(CodecError::FrameTooLarge { len }) if len == MAX_FRAME_LEN + 1
        ));
        assert!(matches!(
            wss_frame(Channel::Control, &Oversized),
            Err(CodecError::FrameTooLarge { len }) if len == MAX_FRAME_LEN + 1
        ));
    }
}
