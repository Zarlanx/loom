// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! PR-26 `bootstrap-auth` proving suite.
//!
//! Drives the security bootstrap end-to-end against the real seams: a fresh
//! bootstrap mints a `CA` + a one-time admin token persisted through the
//! [`Store`](loom_store::Store); an agent presenting a valid enrollment token
//! gets a node certificate that verifies cryptographically back to that `CA`; a
//! bad or expired token is refused.

use loom_agentproto::bootstrap::{Bootstrap, BootstrapError, hash_token};
use loom_core::{AccountId, Timestamp};
use loom_store::{FakeStore, Store};
use rcgen::{CertificateParams, DnType, KeyPair};
use x509_parser::pem::parse_x509_pem;

fn now() -> Timestamp {
    Timestamp::from_millis(1_000_000)
}

/// Builds an agent-side key pair and a CSR for `common_name`, exactly as
/// `loom-hostd` would during enrollment — the agent's private key never leaves.
fn agent_csr(common_name: &str) -> String {
    let key = KeyPair::generate().expect("agent key pair");
    let mut params = CertificateParams::new(vec![common_name.to_owned()]).expect("csr params");
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params
        .serialize_request(&key)
        .expect("serialize CSR")
        .pem()
        .expect("CSR to PEM")
}

/// Asserts `leaf_pem` is cryptographically signed by the CA in `ca_pem`.
fn assert_chains_to_ca(leaf_pem: &str, ca_pem: &str) {
    let (_, ca_block) = parse_x509_pem(ca_pem.as_bytes()).expect("parse CA PEM");
    let ca_cert = ca_block.parse_x509().expect("parse CA X.509");
    let (_, leaf_block) = parse_x509_pem(leaf_pem.as_bytes()).expect("parse leaf PEM");
    let leaf_cert = leaf_block.parse_x509().expect("parse leaf X.509");
    leaf_cert
        .verify_signature(Some(ca_cert.public_key()))
        .expect("leaf certificate must be signed by the local CA");
}

#[tokio::test]
async fn fresh_bootstrap_mints_ca_and_admin_and_persists_the_key() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (boot, admin) = Bootstrap::create(dir.path()).expect("bootstrap");

    // The secrets layout landed on disk.
    assert!(dir.path().join("ca.crt").exists(), "CA cert written");
    assert!(dir.path().join("ca.key").exists(), "CA key written");
    assert!(dir.path().join("secrets.toml").exists(), "secrets written");

    // A CA and a one-time admin token were minted.
    assert!(boot.ca().cert_pem().contains("BEGIN CERTIFICATE"));
    assert!(admin.token.starts_with("loom_admin_"));

    // The admin token is persisted as a hash the auth path can resolve.
    let store = FakeStore::new();
    boot.persist_admin(&store, now())
        .await
        .expect("persist admin");

    let found = store
        .api_key_by_hash(&hash_token(&admin.token))
        .await
        .expect("lookup admin key")
        .expect("admin key present");
    assert_eq!(found.account, AccountId::new(boot.admin_account_id()));
    assert!(!found.revoked);

    let account = store
        .get_account(&AccountId::new(boot.admin_account_id()))
        .await
        .expect("lookup account");
    assert!(account.is_some(), "admin account persisted");

    // The plaintext token is never stored, and a wrong token does not resolve.
    let miss = store
        .api_key_by_hash(&hash_token("loom_admin_not_the_real_token"))
        .await
        .expect("lookup miss");
    assert!(
        miss.is_none(),
        "a wrong admin token authenticates as nobody"
    );

    // persist_admin is idempotent: a second call is a no-op success.
    boot.persist_admin(&store, now())
        .await
        .expect("re-persist admin is idempotent");
}

#[test]
fn an_agent_with_a_valid_token_gets_a_signed_node_cert() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (boot, _admin) = Bootstrap::create(dir.path()).expect("bootstrap");

    let token = boot
        .issue_enrollment_token(60_000, now())
        .expect("issue enrollment token");
    let csr = agent_csr("node-abc");

    let node_cert = boot
        .enroll_node(token.as_str(), &csr, now())
        .expect("enroll a valid agent");

    assert_chains_to_ca(&node_cert, boot.ca().cert_pem());
}

#[test]
fn a_bad_enrollment_token_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (boot, _admin) = Bootstrap::create(dir.path()).expect("bootstrap");
    let csr = agent_csr("node-x");

    // A forged token: never issued by this bootstrap's enrollment key.
    let err = boot
        .enroll_node("le1.6162.6162", &csr, now())
        .expect_err("a forged token is refused");
    assert!(matches!(err, BootstrapError::TokenInvalid));

    // A well-formed token whose expiry has passed.
    let token = boot
        .issue_enrollment_token(10_000, now())
        .expect("issue enrollment token");
    let later = Timestamp::from_millis(now().as_millis() + 20_000);
    let err = boot
        .enroll_node(token.as_str(), &csr, later)
        .expect_err("an expired token is refused");
    assert!(matches!(err, BootstrapError::TokenExpired));
}

#[test]
fn a_reloaded_bootstrap_honors_tokens_and_keeps_the_same_ca() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (created, _admin) = Bootstrap::create(dir.path()).expect("bootstrap");
    let anchor = created.ca().cert_pem().to_owned();
    drop(created);

    let reloaded = Bootstrap::load(dir.path()).expect("reload bootstrap");
    assert_eq!(reloaded.ca().cert_pem(), anchor, "trust anchor is stable");

    let token = reloaded
        .issue_enrollment_token(60_000, now())
        .expect("issue after reload");
    let node_cert = reloaded
        .enroll_node(token.as_str(), &agent_csr("node-reload"), now())
        .expect("enroll after reload");
    assert_chains_to_ca(&node_cert, &anchor);
}

#[test]
fn creating_over_an_existing_bootstrap_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    Bootstrap::create(dir.path()).expect("first bootstrap");
    let err = Bootstrap::create(dir.path()).expect_err("second bootstrap refused");
    assert!(matches!(err, BootstrapError::AlreadyInitialized));
}
