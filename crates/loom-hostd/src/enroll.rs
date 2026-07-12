// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Client-side enrollment: the token-only CSR bootstrap handshake
//! (`agent-protocol.md` §1.2, §3a, `host-agent.md` §4).
//!
//! The first-ever connection is constrained: the agent presents a single-use enrollment
//! token and a locally-generated CSR, and may send **only** an [`EnrollRequest`]. The
//! gateway verifies the token, signs a node cert, and returns an [`EnrollGrant`]; a bad or
//! spent token is **terminal** and never auto-retried (§3a). Everything transient — a
//! connect failure or a mid-handshake drop — is retried with the reconnect backoff.

use loom_proto::{
    Body,
    codec::Channel,
    v1::{
        Backend, BenchmarkFingerprint, EnrollGrant, EnrollRequest, HardwareInventory, MemoryModel,
    },
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{
    clock::WallClock,
    connect::{BackoffPolicy, Connector, Jitter},
    identity::{CsrProvider, EnrolledNode, KeystoreError},
    transport::{TransportError, WsTransport},
    wire::{MsgIdGen, envelope, supported_versions},
};

/// Errors from the enrollment handshake.
#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    /// The gateway rejected enrollment (e.g. an invalid/spent token). **Terminal** —
    /// the agent surfaces it to the owner and does not retry (`agent-protocol.md` §3a).
    #[error("enrollment rejected (terminal): {0}")]
    Rejected(String),
    /// A transient transport failure; the retry loop backs off and reconnects.
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    /// The gateway sent something other than an [`EnrollGrant`] during the bootstrap.
    #[error("unexpected message during enrollment: {0}")]
    Unexpected(String),
    /// Local CSR/key generation failed (terminal — a retry cannot fix it).
    #[error("csr generation: {0}")]
    Csr(#[from] KeystoreError),
    /// The backoff loop exhausted its attempt budget without enrolling.
    #[error("exhausted {0} enrollment attempts")]
    Exhausted(u32),
}

impl EnrollError {
    /// Whether the retry loop should back off and try again, versus giving up now.
    #[must_use]
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transport(_))
    }
}

/// The hardware the agent advertises at enrollment.
///
/// A skeleton stub: real inventory (NVML / Metal probe, benchmark fingerprint) is the
/// backend-capability workstream (PR-16). The default is a GPU-less CPU node, which is all
/// the no-GPU skeleton can honestly claim.
#[derive(Debug, Clone)]
pub struct NodeProfile {
    /// Advertised accelerator model, or empty for a CPU-only node.
    pub gpu_model: String,
    /// Backends the node can run (`ADR-0015`).
    pub backends: Vec<Backend>,
    /// How the node's memory is organized.
    pub memory_model: MemoryModel,
    /// Unified-memory pool size in MB (0 for a discrete/CPU node).
    pub unified_memory_mb: u64,
}

impl Default for NodeProfile {
    fn default() -> Self {
        Self {
            gpu_model: String::new(),
            backends: vec![Backend::Cpu],
            memory_model: MemoryModel::Unspecified,
            unified_memory_mb: 0,
        }
    }
}

impl NodeProfile {
    fn inventory(&self) -> HardwareInventory {
        HardwareInventory {
            gpu_model: self.gpu_model.clone(),
            backends: self.backends.iter().map(|b| *b as i32).collect(),
            memory_model: self.memory_model as i32,
            unified_memory_mb: self.unified_memory_mb,
            ..HardwareInventory::default()
        }
    }
}

/// The inputs to one node's enrollment.
#[derive(Debug, Clone)]
pub struct EnrollmentRequest {
    /// Single-use token minted when the owner linked the machine.
    pub token: String,
    /// Advisory CSR subject nonce (the control plane assigns the real `agent_id`).
    pub subject_nonce: String,
    /// Advertised hardware profile.
    pub profile: NodeProfile,
}

/// A completed enrollment: the durable node identity plus the granted config.
#[derive(Debug, Clone)]
pub struct Enrollment {
    /// The identity to persist and present on later connections.
    pub node: EnrolledNode,
    /// Control-plane-pushed config (heartbeat cadence, spool cap, endpoints), if any.
    pub config: Option<loom_proto::v1::AgentConfig>,
    /// The mutually-chosen schema major (`agent-protocol.md` §2.3).
    pub chosen_version: u32,
}

/// Drives the client side of enrollment.
///
/// Holds the swappable CSR/clock seams and the request inputs; the transport is supplied
/// per call so the same enroller works over the production WSS connector or an in-process
/// fake gateway.
pub struct Enroller<'a> {
    /// Produces the keypair + CSR (real crypto swaps in via this seam).
    pub csr: &'a dyn CsrProvider,
    /// Advisory wall clock for `Envelope.timestamp_ms`.
    pub clock: &'a dyn WallClock,
    /// The enrollment inputs.
    pub request: EnrollmentRequest,
}

impl std::fmt::Debug for Enroller<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enroller")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl Enroller<'_> {
    /// Builds the [`EnrollRequest`] body and the local key material for one attempt.
    fn build_request(&self) -> Result<(EnrollRequest, Vec<u8>), EnrollError> {
        let key = self.csr.generate(&self.request.subject_nonce)?;
        let req = EnrollRequest {
            enroll_token: self.request.token.clone(),
            csr_der: key.csr_der,
            hw: Some(self.request.profile.inventory()),
            bench: Some(BenchmarkFingerprint::default()),
            nat: None,
            supported_versions: Some(supported_versions()),
        };
        Ok((req, key.private_key_der))
    }

    /// Performs one enrollment round-trip over an already-connected transport.
    ///
    /// # Errors
    ///
    /// - [`EnrollError::Rejected`] on a terminal token rejection (close with a reason).
    /// - [`EnrollError::Transport`] on a transient transport failure.
    /// - [`EnrollError::Unexpected`] if the gateway's reply is not an `EnrollGrant`.
    /// - [`EnrollError::Csr`] if local key/CSR generation fails.
    pub async fn enroll_once<S>(
        &self,
        transport: &mut WsTransport<S>,
        ids: &MsgIdGen,
    ) -> Result<Enrollment, EnrollError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (request, private_key_der) = self.build_request()?;
        let env = envelope(
            ids.next_id(),
            String::new(),
            self.clock.now_unix_ms(),
            Body::EnrollRequest(request),
        );
        transport.send(Channel::Control, &env).await?;

        let (_channel, reply) = match transport.recv().await {
            Ok(msg) => msg,
            // A close carrying a reason is a terminal application-level rejection; a bare
            // drop is transient and left for the retry loop.
            Err(TransportError::Closed {
                reason: Some(reason),
            }) => return Err(EnrollError::Rejected(reason)),
            Err(other) => return Err(EnrollError::Transport(other)),
        };

        match reply.body {
            Some(Body::EnrollGrant(grant)) => Ok(finish_enrollment(grant, private_key_der)),
            Some(other) => Err(EnrollError::Unexpected(format!("{other:?}"))),
            None => Err(EnrollError::Unexpected("empty envelope".to_string())),
        }
    }

    /// Connects and enrolls, retrying transient failures with exponential backoff + full
    /// jitter up to `max_attempts` (`agent-protocol.md` §1.3, §3a).
    ///
    /// # Errors
    ///
    /// The last terminal [`EnrollError`], or [`EnrollError::Exhausted`] if every attempt
    /// failed transiently.
    pub async fn enroll_with_backoff<C>(
        &self,
        connector: &C,
        backoff: BackoffPolicy,
        jitter_seed: u64,
        max_attempts: u32,
    ) -> Result<Enrollment, EnrollError>
    where
        C: Connector,
    {
        let mut jitter = Jitter::new(jitter_seed);
        for attempt in 0..max_attempts {
            // A fresh per-connection id generator, matching the "unique per connection"
            // scope of `msg_id` (`agent-protocol.md` §2.2).
            let ids = MsgIdGen::new();
            let outcome = match connector.connect().await {
                Ok(mut transport) => self.enroll_once(&mut transport, &ids).await,
                Err(e) => Err(EnrollError::Transport(e)),
            };
            match outcome {
                Ok(enrollment) => return Ok(enrollment),
                Err(e) if e.is_transient() => {
                    tracing::warn!(attempt, error = %e, "enrollment attempt failed; backing off");
                    let delay = backoff.delay(attempt, &mut jitter);
                    tokio::time::sleep(delay).await;
                }
                Err(terminal) => return Err(terminal),
            }
        }
        Err(EnrollError::Exhausted(max_attempts))
    }
}

/// Combines the granted cert with the locally-held private key into a durable identity.
fn finish_enrollment(grant: EnrollGrant, private_key_der: Vec<u8>) -> Enrollment {
    Enrollment {
        node: EnrolledNode {
            agent_id: grant.agent_id,
            node_cert_der: grant.node_cert_der,
            ca_chain_der: grant.ca_chain_der,
            private_key_der,
        },
        config: grant.config,
        chosen_version: grant.chosen_version,
    }
}

#[cfg(test)]
mod tests {
    use super::{EnrollError, NodeProfile};
    use loom_proto::v1::Backend;

    #[test]
    fn default_profile_is_a_cpu_node() {
        let p = NodeProfile::default();
        assert_eq!(p.backends, vec![Backend::Cpu]);
        assert!(p.gpu_model.is_empty());
        let inv = p.inventory();
        assert_eq!(inv.backends, vec![Backend::Cpu as i32]);
    }

    #[test]
    fn only_transport_errors_are_transient() {
        assert!(
            EnrollError::Transport(crate::transport::TransportError::Closed { reason: None })
                .is_transient()
        );
        assert!(!EnrollError::Rejected("bad token".to_string()).is_transient());
        assert!(!EnrollError::Exhausted(3).is_transient());
        assert!(!EnrollError::Unexpected("x".to_string()).is_transient());
    }
}
