// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! PR-08a gate: the host agent connects to a fake in-process gateway, drives the
//! token-only CSR enrollment handshake, obtains a (fake-signed) node cert, and reconnects
//! with backoff through transient connect failures — all over a real WebSocket session on
//! an in-process duplex, with no real network and no GPU.

use std::time::Duration;

use loom_hostd::{
    BackoffPolicy, Enroller, EnrollmentRequest, NodeProfile, PlaceholderCsr,
    clock::FixedClock,
    enroll::EnrollError,
    testsupport::{DuplexConnector, FakeGateway, GatewayConfig},
};

fn enroller(token: &str) -> (PlaceholderCsr, FixedClock, EnrollmentRequest) {
    (
        PlaceholderCsr,
        FixedClock(1_700_000_000_000),
        EnrollmentRequest {
            token: token.to_string(),
            subject_nonce: "rig-01".to_string(),
            profile: NodeProfile::default(),
        },
    )
}

fn fast_backoff() -> BackoffPolicy {
    // Tiny bounds so the reconnect test spends microseconds, not seconds, in backoff.
    BackoffPolicy::new(Duration::from_millis(1), Duration::from_millis(4))
}

#[tokio::test]
async fn enrolls_against_the_fake_gateway() {
    let (csr, clock, request) = enroller("single-use-T");
    let enroller = Enroller {
        csr: &csr,
        clock: &clock,
        request,
    };
    let gateway = FakeGateway::default();
    let connector = DuplexConnector::new(gateway.clone());

    let enrollment = enroller
        .enroll_with_backoff(&connector, fast_backoff(), 7, 4)
        .await
        .expect("enrollment succeeds");

    // The client obtained a (fake-signed) node cert bound to the assigned agent_id.
    assert_eq!(enrollment.node.agent_id, "node-fake-0000000000000001");
    assert!(
        enrollment
            .node
            .node_cert_der
            .starts_with(b"LOOM-FAKE-CERT:"),
        "cert carries the fake-signer tag"
    );
    assert!(
        !enrollment.node.private_key_der.is_empty(),
        "the private key is held locally"
    );
    assert_eq!(enrollment.chosen_version, 1);
    assert!(enrollment.config.is_some(), "granted an agent config");

    // The gateway saw exactly one enroll request carrying the CSR, and it never received
    // the private key.
    let requests = gateway.enroll_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].enroll_token, "single-use-T");
    assert!(requests[0].csr_der.starts_with(b"LOOM-PLACEHOLDER-CSR-v0:"));
    assert_eq!(connector.attempts(), 1, "one connect, no retries needed");
    assert_eq!(gateway.grant_count(), 1);
}

#[tokio::test]
async fn reconnects_with_backoff_through_transient_connect_failures() {
    let (csr, clock, request) = enroller("single-use-T");
    let enroller = Enroller {
        csr: &csr,
        clock: &clock,
        request,
    };
    let gateway = FakeGateway::default();
    // The first two connects fail transiently; the loop must back off and retry.
    let connector = DuplexConnector::new(gateway.clone()).failing_first(2);

    let enrollment = enroller
        .enroll_with_backoff(&connector, fast_backoff(), 7, 6)
        .await
        .expect("enrollment eventually succeeds after reconnects");

    assert_eq!(enrollment.node.agent_id, "node-fake-0000000000000001");
    assert_eq!(
        connector.attempts(),
        3,
        "two failed connects then one success"
    );
    assert_eq!(gateway.grant_count(), 1);
}

#[tokio::test]
async fn bad_token_is_terminal_and_not_retried() {
    let (csr, clock, request) = enroller("stolen-token");
    let enroller = Enroller {
        csr: &csr,
        clock: &clock,
        request,
    };
    // Only "single-use-T" is valid; the gateway refuses anything else with a terminal
    // close reason.
    let gateway = FakeGateway::new(GatewayConfig::default());
    let connector = DuplexConnector::new(gateway.clone());

    let err = enroller
        .enroll_with_backoff(&connector, fast_backoff(), 7, 5)
        .await
        .expect_err("a spent/invalid token is terminal");

    match err {
        EnrollError::Rejected(reason) => assert_eq!(reason, "enroll_token_invalid"),
        other => panic!("expected a terminal rejection, got {other:?}"),
    }
    // Terminal: it did not burn through the whole attempt budget retrying.
    assert_eq!(connector.attempts(), 1, "no retry on a terminal rejection");
    assert_eq!(gateway.grant_count(), 0);
}
