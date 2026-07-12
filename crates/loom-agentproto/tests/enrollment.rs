// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! PR-09b proving suite: a fake agent enrolls through the **real** session terminator,
//! in-process, and its events land on the bus with the granted (fenced) identity.
//!
//! The whole path is real: a loopback WSS session (no sockets, no `TLS` stack), the real
//! [`SessionTerminator`] wired to a real [`Enroller`] over the [`Bootstrap`] `CA`/token
//! machinery and a [`FakeStore`], and the real 4-stream demux + bus bridge. Only the agent
//! is a fake — the counterpart the gateway exists to serve.

use std::sync::Arc;

use loom_agentproto::bootstrap::Bootstrap;
use loom_agentproto::testing::{FakeAgent, FixedClock, loopback};
use loom_agentproto::{Enroller, PeerIdentity, SessionTerminator, TerminatorError};
use loom_bus::{Bus, InProcBus, Topic};
use loom_core::{HostId, Timestamp};
use loom_proto::codec::Channel;
use loom_proto::v1::{EnrollRequest, HardwareInventory, Heartbeat};
use loom_proto::{Body, Envelope};
use loom_store::{FakeStore, HostStatus, Store};
use rcgen::{CertificateParams, DnType, KeyPair};
use x509_parser::pem::parse_x509_pem;
use x509_parser::prelude::FromDer;

const NOW_MS: i64 = 1_700_000_000_000;

/// A fresh security bootstrap in a temp dir. The bootstrap holds its `CA`/keys in memory,
/// so the returned dir guard only needs to outlive `create`.
fn fresh_bootstrap() -> (Arc<Bootstrap>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (boot, _admin) = Bootstrap::create(dir.path()).expect("bootstrap");
    (Arc::new(boot), dir)
}

/// Builds a real agent-side keypair + `CSR` and returns its `DER` bytes (as the wire
/// carries in `EnrollRequest.csr_der`).
fn agent_csr_der(common_name: &str) -> Vec<u8> {
    let key = KeyPair::generate().expect("key");
    let mut params = CertificateParams::new(vec![common_name.to_owned()]).expect("params");
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.serialize_request(&key).expect("csr").der().to_vec()
}

fn enroll_request(token: String, csr_der: Vec<u8>) -> Envelope {
    Envelope {
        protocol_version: 1,
        msg_id: "01J0000000000000000000ENRQ".to_owned(),
        correlation_id: String::new(),
        timestamp_ms: NOW_MS,
        body: Some(Body::EnrollRequest(EnrollRequest {
            enroll_token: token,
            csr_der,
            hw: Some(HardwareInventory {
                gpu_model: "M3 Max".to_owned(),
                unified_memory_mb: 48_000,
                ..HardwareInventory::default()
            }),
            ..EnrollRequest::default()
        })),
    }
}

/// Asserts a `DER` leaf certificate is cryptographically signed by the `CA` in `ca_pem`.
fn assert_cert_der_chains_to(ca_pem: &str, leaf_der: &[u8]) {
    let (_, ca_block) = parse_x509_pem(ca_pem.as_bytes()).expect("parse CA PEM");
    let ca_cert = ca_block.parse_x509().expect("parse CA X.509");
    let (_, leaf) =
        x509_parser::certificate::X509Certificate::from_der(leaf_der).expect("leaf DER");
    leaf.verify_signature(Some(ca_cert.public_key()))
        .expect("node cert must be signed by the local CA");
}

#[tokio::test]
async fn a_fake_agent_enrolls_and_its_events_land_on_the_bus_with_the_granted_identity() {
    let (boot, _dir) = fresh_bootstrap();
    let ca_pem = boot.ca().cert_pem().to_owned();
    let bus = Arc::new(InProcBus::new());
    let store = Arc::new(FakeStore::new());
    let mut sub = bus
        .subscribe(Topic::agent_events())
        .await
        .expect("subscribe");

    let clock = Arc::new(FixedClock::new(Timestamp::from_millis(NOW_MS)));
    let enroller = Enroller::new(boot.clone(), store.clone(), clock);
    let terminator = SessionTerminator::with_enrollment(bus.clone(), enroller);

    // A valid enrollment token, and the real terminator serving the server end.
    let token = boot
        .issue_enrollment_token(60_000, Timestamp::from_millis(NOW_MS))
        .expect("token")
        .into_string();
    let (server, client) = loopback().await;
    let serve_task =
        tokio::spawn(async move { terminator.serve(server, PeerIdentity::Bootstrap).await });

    // The fake agent enrolls token-only, receives the grant, then heartbeats.
    let mut agent = FakeAgent::new(client);
    agent
        .send_envelope(
            Channel::Control,
            &enroll_request(token, agent_csr_der("node-a")),
        )
        .await;

    let grant_frame = agent.recv().await;
    let Some(Body::EnrollGrant(grant)) = grant_frame.envelope.body else {
        panic!(
            "expected an EnrollGrant, got {:?}",
            grant_frame.envelope.body
        );
    };
    assert!(grant.agent_id.starts_with("agent_"));
    assert_eq!(grant.chosen_version, 1);
    // The issued node cert cryptographically chains back to the bootstrap CA.
    assert_cert_der_chains_to(&ca_pem, &grant.node_cert_der);
    let granted_id = grant.agent_id.clone();

    // Now exchange an M1 message on its authenticated identity.
    agent
        .send(Channel::Heartbeat, Body::Heartbeat(Heartbeat::default()))
        .await;
    agent.close().await;

    let summary = serve_task.await.expect("join").expect("serve");
    assert_eq!(summary.agent_id.as_deref(), Some(granted_id.as_str()));
    // The enrolled event plus the heartbeat.
    assert_eq!(summary.events_bridged, 2);

    // The host was recorded durably, bound to the admin account, marked enrolled.
    let host = store
        .get_host(&HostId::new(&granted_id))
        .await
        .expect("get host")
        .expect("host present");
    assert_eq!(host.status, HostStatus::Enrolled);
    assert_eq!(host.account.as_str(), boot.admin_account_id());

    // Both bus events carry the granted (fenced) identity, in order.
    let enrolled_event: loom_agentproto::AgentEvent =
        serde_json::from_slice(&sub.recv().await.expect("enrolled").payload).expect("json");
    assert_eq!(enrolled_event.agent_id, granted_id);
    assert_eq!(
        enrolled_event.kind,
        loom_agentproto::AgentEventKind::Enrolled
    );

    let beat_event: loom_agentproto::AgentEvent =
        serde_json::from_slice(&sub.recv().await.expect("heartbeat").payload).expect("json");
    assert_eq!(beat_event.agent_id, granted_id);
    assert_eq!(beat_event.kind, loom_agentproto::AgentEventKind::Heartbeat);
}

#[tokio::test]
async fn a_bad_token_is_refused_and_the_connection_is_closed_with_a_reason() {
    let (boot, _dir) = fresh_bootstrap();
    let bus = Arc::new(InProcBus::new());
    let store = Arc::new(FakeStore::new());
    let clock = Arc::new(FixedClock::new(Timestamp::from_millis(NOW_MS)));
    let enroller = Enroller::new(boot, store, clock);
    let terminator = SessionTerminator::with_enrollment(bus, enroller);

    let (server, client) = loopback().await;
    let serve_task =
        tokio::spawn(async move { terminator.serve(server, PeerIdentity::Bootstrap).await });

    let mut agent = FakeAgent::new(client);
    // A forged token: never issued by this bootstrap's enrollment key.
    agent
        .send_envelope(
            Channel::Control,
            &enroll_request("le1.6162.6162".to_owned(), agent_csr_der("node-x")),
        )
        .await;

    // The gateway refuses; the terminator surfaces the enrollment error.
    let err = serve_task.await.expect("join").expect_err("refused");
    assert!(matches!(err, TerminatorError::Enroll(_)));

    // And the agent sees the connection closed with the refusal reason.
    let closed = agent.recv_result().await.expect_err("closed");
    match closed {
        loom_agentproto::SessionError::Closed { reason } => {
            assert_eq!(reason.as_deref(), Some("enroll_token_invalid"));
        }
        other => panic!("expected a reasoned close, got {other:?}"),
    }
}
