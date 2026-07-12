// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Protocol golden vectors (CI job f, `cargo test -p loom-proto --test golden_vectors`).
//!
//! These assertions are the mechanical enforcement of the "wire contract frozen first,
//! additive-only after" hard call (README §1): every canonical message is checked in as
//! serialized bytes and re-verified here on every build, so a schema change that alters
//! the wire is caught immediately. `cargo xtask golden regen` is the only blessed way to
//! update the checked-in vectors.

use loom_proto::codec::{self, Channel};
use loom_proto::golden;
use loom_proto::v1::Envelope;
use prost::Message;
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

/// Every canonical vector matches its checked-in bytes exactly.
#[test]
fn vectors_match_checked_in_bytes() {
    // The checked-in `.bin` set must be exactly the canonical set: a stale file
    // left behind after a vector is renamed/removed would otherwise never be
    // flagged, since the per-vector loop below only iterates current vectors.
    let expected: std::collections::HashSet<String> = golden::vectors()
        .iter()
        .map(|v| format!("{}.bin", v.name))
        .collect();
    let actual: std::collections::HashSet<String> = std::fs::read_dir(golden_dir())
        .expect("read golden dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bin"))
        })
        .collect();
    assert_eq!(
        actual, expected,
        "tests/golden contains .bin files not produced by golden::vectors() (or vice versa); \
         run `cargo xtask golden regen`"
    );

    for vector in golden::vectors() {
        let path = golden_dir().join(format!("{}.bin", vector.name));
        let checked_in = std::fs::read(&path).unwrap_or_else(|err| {
            panic!(
                "missing golden vector {}: {err}. Run `cargo xtask golden regen`.",
                path.display()
            )
        });
        assert_eq!(
            checked_in, vector.bytes,
            "golden vector `{}` drifted from the checked-in bytes; \
             run `cargo xtask golden regen` if the change is intentional",
            vector.name
        );
    }
}

/// The checked-in bytes decode as an `Envelope` and re-encode byte-identically —
/// proving both directions of the codec against the frozen bytes.
#[test]
fn vectors_round_trip_through_decode() {
    for vector in golden::vectors() {
        let decoded = Envelope::decode(vector.bytes.as_slice()).unwrap_or_else(|err| {
            panic!("golden vector `{}` failed to decode: {err}", vector.name)
        });
        let re_encoded = decoded.encode_to_vec();
        assert_eq!(
            re_encoded, vector.bytes,
            "re-encoding golden vector `{}` was not byte-identical",
            vector.name
        );
    }
}

/// Each vector survives a full framing round-trip on both transports (agent-protocol.md §2.1).
#[test]
fn vectors_survive_framing() {
    for vector in golden::vectors() {
        let envelope = Envelope::decode(vector.bytes.as_slice())
            .unwrap_or_else(|err| panic!("decode `{}`: {err}", vector.name));

        // QUIC form: bare length-prefixed frame.
        let framed = codec::encode_frame(&envelope).expect("frame");
        let (payload, rest) = codec::decode_frame(&framed).expect("deframe");
        assert!(
            rest.is_empty(),
            "unexpected trailing bytes for `{}`",
            vector.name
        );
        assert_eq!(payload, vector.bytes.as_slice());

        // WSS form: channel-tagged frame.
        let wss = codec::wss_frame(Channel::Control, &envelope).expect("wss frame");
        let (channel, wss_payload, wss_rest) = codec::decode_wss_frame(&wss).expect("wss deframe");
        assert_eq!(channel, Channel::Control);
        assert!(wss_rest.is_empty());
        assert_eq!(wss_payload, vector.bytes.as_slice());
    }
}

/// PR-02b gate: a message a fake agent frames onto the wire is decoded to a byte- and
/// value-identical message on the server side — the two sides of the wire, compiled from
/// one schema, agree.
#[test]
fn fake_agent_and_server_decode_identical_bytes() {
    use loom_proto::Body;
    use loom_proto::v1::JobAccept;

    // Agent side: build a JobAccept, wrap it in an Envelope, frame it for the WSS
    // control channel.
    let agent_view = Envelope {
        protocol_version: 1,
        msg_id: "0000000000000000000000AGENT".to_string(),
        correlation_id: "0000000000000000000000OFFER".to_string(),
        timestamp_ms: 1_700_000_000_000,
        body: Some(Body::JobAccept(JobAccept {
            attempt_id: "00000000000000000000ATTEMPT".to_string(),
        })),
    };
    let on_wire = codec::wss_frame(Channel::Control, &agent_view).expect("agent frames message");

    // Server side: deframe on the control channel, decode the payload, and confirm both
    // the decoded value and its re-encoded bytes match what the agent sent.
    let (channel, payload, rest) = codec::decode_wss_frame(&on_wire).expect("server deframes");
    assert_eq!(channel, Channel::Control);
    assert!(rest.is_empty());
    let server_view: Envelope = codec::decode_message(payload).expect("server decodes envelope");

    assert_eq!(
        server_view, agent_view,
        "server decoded a different message"
    );
    assert_eq!(
        server_view.encode_to_vec(),
        agent_view.encode_to_vec(),
        "server re-encoded different bytes"
    );
    match server_view.body {
        Some(Body::JobAccept(accept)) => {
            assert_eq!(accept.attempt_id, "00000000000000000000ATTEMPT");
        }
        other => panic!("expected a JobAccept body, got {other:?}"),
    }
}
