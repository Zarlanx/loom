// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Enroller`] — the enrollment handler: verify token, issue node cert, record the host.
//!
//! This is the server half of the enrollment handshake (agent-protocol.md §1.2, §3a). A
//! joining agent connects token-only (no client certificate) and sends one
//! [`EnrollRequest`](loom_proto::v1::EnrollRequest) carrying its enrollment token and a
//! `PKCS#10` `CSR`. The enroller:
//!
//! 1. verifies the token against the [`Bootstrap`] enrollment key — a bad or expired token
//!    is refused *before any certificate is signed*;
//! 2. assigns the node an `agent_id` and signs its `CSR` into a `CA`-anchored node
//!    certificate (the agent's private key never leaves its host);
//! 3. records the enrolled [`Host`] (and its advertised `GPU`) durably in the [`Store`],
//!    bound to the bootstrap admin account.
//!
//! The signed cert plus the `CA` chain go back in the [`EnrollGrant`](loom_proto::v1::EnrollGrant)
//! the [terminator](crate::terminator) sends; every *later* connection presents that cert
//! and the identity is `mTLS`, not a token (§1.2).

use std::sync::Arc;

use loom_core::{AccountId, GpuId, HostId, Timestamp};
use loom_proto::v1::{AgentConfig, EnrollGrant, EnrollRequest};
use loom_store::{Gpu, Host, HostStatus, Store, StoreError};

use crate::bootstrap::{Bootstrap, BootstrapError};
use crate::clock::Clock;

/// M1 speaks exactly protocol schema major 1 (agent-protocol.md §2.3).
const CHOSEN_VERSION: u32 = 1;
/// Default heartbeat cadence pushed in the grant's [`AgentConfig`] (agent-protocol.md §3b).
const DEFAULT_HEARTBEAT_INTERVAL_MS: u32 = 15_000;
/// Default durable-spool cap pushed in the grant's [`AgentConfig`] (agent-protocol.md §3f).
const DEFAULT_SPOOL_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// A failure enrolling a node.
///
/// The token/CSR variants are *agent-facing refusals* — the terminator closes the
/// connection with the matching reason code — while [`Bootstrap`](EnrollError::Bootstrap)
/// / [`Store`](EnrollError::Store) / [`Random`](EnrollError::Random) are internal faults.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EnrollError {
    /// The enrollment token was malformed or carried a bad `MAC` — refused.
    #[error("enrollment token invalid")]
    TokenInvalid,
    /// The enrollment token was well-formed but expired — refused.
    #[error("enrollment token expired")]
    TokenExpired,
    /// The `CSR` was missing or would not parse/sign — refused.
    #[error("enrollment CSR rejected: {0}")]
    Csr(String),
    /// An internal bootstrap failure (e.g. reading the `CA` cert).
    #[error("bootstrap: {0}")]
    Bootstrap(BootstrapError),
    /// Persisting the enrolled host failed.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// The `OS` random source was unavailable when minting the `agent_id`.
    #[error("secure random source unavailable: {0}")]
    Random(String),
}

impl EnrollError {
    /// The application-level close reason the terminator sends the agent (agent-protocol.md
    /// §3a). Internal faults collapse to a single opaque code — the agent learns only that
    /// enrollment failed, never gateway internals.
    #[must_use]
    pub fn close_reason(&self) -> &'static str {
        match self {
            Self::TokenInvalid => "enroll_token_invalid",
            Self::TokenExpired => "enroll_token_expired",
            Self::Csr(_) => "enroll_csr_invalid",
            Self::Bootstrap(_) | Self::Store(_) | Self::Random(_) => "enroll_internal_error",
        }
    }
}

/// A successful enrollment result: the identity and certificate to return, plus the
/// config to push.
#[derive(Debug, Clone)]
pub struct Grant {
    /// The `agent_id` the control plane assigned this node.
    pub agent_id: String,
    /// The `CA`-signed node certificate, `DER`-encoded (`EnrollGrant.node_cert_der`).
    pub node_cert_der: Vec<u8>,
    /// The `CA` trust anchor, `DER`-encoded (`EnrollGrant.ca_chain_der`).
    pub ca_chain_der: Vec<u8>,
    /// The chosen protocol schema major.
    pub chosen_version: u32,
    /// When the grant was issued — stamped (advisory) on the grant envelope.
    pub issued_at: Timestamp,
    /// The agent configuration to push in the grant.
    pub config: AgentConfig,
}

impl Grant {
    /// Materializes the wire [`EnrollGrant`] this result returns to the agent.
    #[must_use]
    pub fn to_enroll_grant(&self) -> EnrollGrant {
        EnrollGrant {
            agent_id: self.agent_id.clone(),
            node_cert_der: self.node_cert_der.clone(),
            ca_chain_der: self.ca_chain_der.clone(),
            config: Some(self.config.clone()),
            chosen_version: self.chosen_version,
        }
    }
}

/// Verifies enrollment tokens, issues node certificates, and records enrolled hosts.
#[derive(Clone)]
pub struct Enroller {
    bootstrap: Arc<Bootstrap>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Enroller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enroller").finish_non_exhaustive()
    }
}

impl Enroller {
    /// Builds an enroller over the security bootstrap, a store, and a clock.
    #[must_use]
    pub fn new(bootstrap: Arc<Bootstrap>, store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self {
            bootstrap,
            store,
            clock,
        }
    }

    /// Handles one [`EnrollRequest`]: verify token → sign `CSR` → record host.
    ///
    /// # Errors
    /// [`EnrollError::TokenInvalid`]/[`EnrollError::TokenExpired`] if the token is refused
    /// (checked before any certificate is signed); [`EnrollError::Csr`] if the `CSR` is
    /// missing or will not sign; [`EnrollError::Store`] if the host cannot be persisted;
    /// [`EnrollError::Random`]/[`EnrollError::Bootstrap`] on an internal fault.
    pub async fn enroll(&self, request: &EnrollRequest) -> Result<Grant, EnrollError> {
        let now = self.clock.now();

        // Verify the token first — a forgery never reaches CSR signing or the store.
        match self
            .bootstrap
            .verify_enrollment_token(&request.enroll_token, now)
        {
            Ok(_claims) => {}
            Err(BootstrapError::TokenInvalid) => return Err(EnrollError::TokenInvalid),
            Err(BootstrapError::TokenExpired) => return Err(EnrollError::TokenExpired),
            Err(other) => return Err(EnrollError::Bootstrap(other)),
        }

        if request.csr_der.is_empty() {
            return Err(EnrollError::Csr("csr_der is empty".to_owned()));
        }

        let agent_id = mint_agent_id()?;
        let node_cert_der = self
            .bootstrap
            .ca()
            .sign_node_cert_der(&request.csr_der)
            .map_err(|e| EnrollError::Csr(e.to_string()))?;
        let ca_chain_der = self
            .bootstrap
            .ca()
            .cert_der()
            .map_err(EnrollError::Bootstrap)?;

        self.record_host(&agent_id, request, &node_cert_der, now)
            .await?;

        Ok(Grant {
            agent_id,
            node_cert_der,
            ca_chain_der,
            chosen_version: CHOSEN_VERSION,
            issued_at: now,
            config: default_agent_config(),
        })
    }

    /// Persists the enrolled host — and its advertised `GPU`, if any — bound to the
    /// bootstrap admin account.
    async fn record_host(
        &self,
        agent_id: &str,
        request: &EnrollRequest,
        node_cert_der: &[u8],
        now: Timestamp,
    ) -> Result<(), EnrollError> {
        let account = AccountId::new(self.bootstrap.admin_account_id());
        let host = Host {
            id: HostId::new(agent_id),
            account,
            // The issued node cert carries the agent's public key; a later PR (metering,
            // PR-27) extracts the SPKI from it to verify signed usage records.
            agent_pubkey: node_cert_der.to_vec(),
            status: HostStatus::Enrolled,
            enrolled_at: now,
            last_seen_at: None,
        };
        self.store.insert_host(&host).await?;

        if let Some(hw) = request.hw.as_ref() {
            let model = if hw.gpu_model.is_empty() {
                "unknown".to_owned()
            } else {
                hw.gpu_model.clone()
            };
            let gpu = Gpu {
                id: GpuId::new(format!("{agent_id}-gpu0")),
                host: HostId::new(agent_id),
                model,
                memory_mb: hw.vram_mb.max(hw.unified_memory_mb),
                fingerprint: None,
            };
            self.store.insert_gpu(&gpu).await?;
        }
        Ok(())
    }
}

/// Mints a fresh `agent_id` from the `OS` CSPRNG — the control-plane-assigned identity the
/// node's certificate is issued for (agent-protocol.md §1.2).
fn mint_agent_id() -> Result<String, EnrollError> {
    let mut buf = [0u8; 12];
    getrandom::fill(&mut buf).map_err(|e| EnrollError::Random(e.to_string()))?;
    Ok(format!("agent_{}", hex::encode(buf)))
}

/// The default agent configuration pushed in an [`EnrollGrant`]. Values are upper bounds
/// the agent intersects with local policy — a push can tighten but never widen an owner
/// cap (agent-protocol.md §3h).
fn default_agent_config() -> AgentConfig {
    AgentConfig {
        heartbeat_interval_ms: DEFAULT_HEARTBEAT_INTERVAL_MS,
        spool_cap_bytes: DEFAULT_SPOOL_CAP_BYTES,
        control_endpoint: String::new(),
        chosen_tiers: Vec::new(),
        config_version: "1".to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;
    use crate::testing::FixedClock;
    use loom_proto::v1::HardwareInventory;
    use loom_store::FakeStore;
    use rcgen::{CertificateParams, DnType, KeyPair};

    fn csr_der(common_name: &str) -> Vec<u8> {
        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::new(vec![common_name.to_owned()]).expect("params");
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.serialize_request(&key).expect("csr").der().to_vec()
    }

    fn enroller_at(now_ms: i64) -> (Enroller, Arc<Bootstrap>, Arc<FakeStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        // Once created, the bootstrap holds its CA/key/enrollment-key in memory; enrollment
        // never re-reads the directory, so it may drop at the end of this helper.
        let (boot, _admin) = Bootstrap::create(dir.path()).expect("bootstrap");
        let boot = Arc::new(boot);
        let store = Arc::new(FakeStore::new());
        let clock = Arc::new(FixedClock::new(Timestamp::from_millis(now_ms)));
        let enroller = Enroller::new(boot.clone(), store.clone(), clock);
        (enroller, boot, store)
    }

    #[tokio::test]
    async fn a_valid_token_issues_a_cert_and_records_the_host() {
        let now = 1_000_000;
        let (enroller, boot, store) = enroller_at(now);
        let token = boot
            .issue_enrollment_token(60_000, Timestamp::from_millis(now))
            .expect("token");
        let request = EnrollRequest {
            enroll_token: token.into_string(),
            csr_der: csr_der("node-a"),
            hw: Some(HardwareInventory {
                gpu_model: "M3 Max".to_owned(),
                unified_memory_mb: 48_000,
                ..HardwareInventory::default()
            }),
            ..EnrollRequest::default()
        };

        let grant = enroller.enroll(&request).await.expect("enroll");
        assert!(grant.agent_id.starts_with("agent_"));
        assert!(!grant.node_cert_der.is_empty());
        assert_eq!(grant.chosen_version, CHOSEN_VERSION);

        let host = store
            .get_host(&HostId::new(&grant.agent_id))
            .await
            .expect("get host")
            .expect("host present");
        assert_eq!(host.status, HostStatus::Enrolled);
        let gpus = store
            .list_gpus_for_host(&HostId::new(&grant.agent_id))
            .await
            .expect("gpus");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].memory_mb, 48_000);
    }

    #[tokio::test]
    async fn a_forged_token_is_refused_before_signing() {
        let (enroller, _boot, _store) = enroller_at(1_000_000);
        let request = EnrollRequest {
            enroll_token: "le1.6162.6162".to_owned(),
            csr_der: csr_der("node-x"),
            ..EnrollRequest::default()
        };
        let err = enroller.enroll(&request).await.expect_err("refused");
        assert!(matches!(err, EnrollError::TokenInvalid));
        assert_eq!(err.close_reason(), "enroll_token_invalid");
    }

    #[tokio::test]
    async fn an_expired_token_is_refused() {
        let issued = 1_000_000;
        let (enroller, boot, _store) = enroller_at(issued + 30_000);
        let token = boot
            .issue_enrollment_token(10_000, Timestamp::from_millis(issued))
            .expect("token");
        let request = EnrollRequest {
            enroll_token: token.into_string(),
            csr_der: csr_der("node-y"),
            ..EnrollRequest::default()
        };
        let err = enroller.enroll(&request).await.expect_err("refused");
        assert!(matches!(err, EnrollError::TokenExpired));
        assert_eq!(err.close_reason(), "enroll_token_expired");
    }

    #[tokio::test]
    async fn a_missing_csr_is_rejected() {
        let now = 1_000_000;
        let (enroller, boot, _store) = enroller_at(now);
        let token = boot
            .issue_enrollment_token(60_000, Timestamp::from_millis(now))
            .expect("token");
        let request = EnrollRequest {
            enroll_token: token.into_string(),
            csr_der: Vec::new(),
            ..EnrollRequest::default()
        };
        let err = enroller.enroll(&request).await.expect_err("refused");
        assert!(matches!(err, EnrollError::Csr(_)));
    }
}
