// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Node identity: local keypair, the enrollment CSR, and the granted node cert
//! (`agent-protocol.md` §1.2, `host-agent.md` §4, §10).
//!
//! The agent generates a keypair locally, sends a PKCS#10 CSR at enrollment, and receives
//! a CA-signed node cert it presents on every later connection. **The private key never
//! leaves the host** — it lives in memory plus an encrypted-at-rest keystore.
//!
//! Scope note: the actual X.509/PKCS#10 crypto (real keypair generation, DER-encoded CSR,
//! signature verification) and the keystore *encryption* are owned by the bootstrap-auth
//! and cert-issuance workstreams (PR-26 / PR-09b). This module models the *handshake
//! shape* the client drives — a swappable [`CsrProvider`] seam producing the key material
//! and CSR bytes, plus a durable keystore — so those crypto backends drop in without
//! touching the transport or enrollment flow. The default [`PlaceholderCsr`] emits
//! structured, non-cryptographic bytes: enough to exercise the wire contract end-to-end
//! against a fake gateway, explicitly not a real CSR.

use serde::{Deserialize, Serialize};

/// Errors from keystore persistence.
#[derive(Debug, thiserror::Error)]
pub enum KeystoreError {
    /// The keystore file could not be read or written.
    #[error("keystore i/o at {path}: {source}")]
    Io {
        /// The keystore path.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The keystore file was present but not valid JSON for [`EnrolledNode`].
    #[error("keystore decode: {0}")]
    Decode(#[from] serde_json::Error),
}

/// A locally-generated keypair plus the CSR bytes derived from it.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyMaterial {
    /// Private key material — persisted to the keystore, **never** sent on the wire.
    pub private_key_der: Vec<u8>,
    /// PKCS#10 CSR (DER) sent in [`EnrollRequest.csr_der`](loom_proto::v1::EnrollRequest).
    pub csr_der: Vec<u8>,
}

// The private key must never appear in logs; its Debug is redacted.
impl std::fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyMaterial")
            .field("private_key_der", &"<redacted>")
            .field("csr_len", &self.csr_der.len())
            .finish()
    }
}

/// Produces the node keypair and CSR at enrollment/rotation time.
///
/// The seam where a real crypto backend (PR-26/PR-09b) replaces the placeholder without
/// disturbing the enrollment flow.
pub trait CsrProvider {
    /// Generates key material and a CSR whose subject carries `subject_nonce`
    /// (advisory — the control plane assigns the real `agent_id`).
    ///
    /// # Errors
    ///
    /// Returns [`KeystoreError`] only if a backing crypto/keystore operation fails; the
    /// placeholder is infallible.
    fn generate(&self, subject_nonce: &str) -> Result<KeyMaterial, KeystoreError>;
}

/// Structural, non-cryptographic CSR generator (default; see the module note).
#[derive(Debug, Clone, Copy, Default)]
pub struct PlaceholderCsr;

impl CsrProvider for PlaceholderCsr {
    fn generate(&self, subject_nonce: &str) -> Result<KeyMaterial, KeystoreError> {
        // Deterministic given the nonce so tests are reproducible. The tags document the
        // placeholder status on the wire and in any captured frame.
        let mut private_key_der = b"LOOM-PLACEHOLDER-KEY-v0:".to_vec();
        private_key_der.extend_from_slice(subject_nonce.as_bytes());
        let mut csr_der = b"LOOM-PLACEHOLDER-CSR-v0:".to_vec();
        csr_der.extend_from_slice(subject_nonce.as_bytes());
        Ok(KeyMaterial {
            private_key_der,
            csr_der,
        })
    }
}

/// The durable identity a node holds after a successful enrollment: the granted cert plus
/// the private key it must never disclose.
///
/// Serialized to the keystore file. Encryption-at-rest is deferred to PR-26; the shape is
/// stable so wrapping it in a sealed envelope later is additive.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolledNode {
    /// Control-plane-assigned agent id (CN/SAN of the node cert).
    pub agent_id: String,
    /// CA-signed node cert (DER).
    pub node_cert_der: Vec<u8>,
    /// CA chain (DER) the agent pins the gateway against.
    pub ca_chain_der: Vec<u8>,
    /// Private key material (kept local, never transmitted).
    pub private_key_der: Vec<u8>,
}

impl std::fmt::Debug for EnrolledNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrolledNode")
            .field("agent_id", &self.agent_id)
            .field("node_cert_len", &self.node_cert_der.len())
            .field("ca_chain_len", &self.ca_chain_der.len())
            .field("private_key_der", &"<redacted>")
            .finish()
    }
}

impl EnrolledNode {
    /// Writes the identity to `path` as JSON, creating parent directories.
    ///
    /// # Errors
    ///
    /// Returns [`KeystoreError::Io`] if the directory or file cannot be written, or
    /// [`KeystoreError::Decode`] if serialization fails.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), KeystoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| KeystoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, json).map_err(|source| KeystoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Loads the identity from `path`, or `Ok(None)` if the keystore does not yet exist.
    ///
    /// # Errors
    ///
    /// Returns [`KeystoreError::Io`] on a non-not-found read error, or
    /// [`KeystoreError::Decode`] if the file is not valid keystore JSON.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Option<Self>, KeystoreError> {
        let path = path.as_ref();
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(KeystoreError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CsrProvider, EnrolledNode, PlaceholderCsr};

    #[test]
    fn placeholder_csr_is_deterministic_and_keeps_key_private() {
        let a = PlaceholderCsr.generate("rig-01").expect("gen");
        let b = PlaceholderCsr.generate("rig-01").expect("gen");
        assert_eq!(a, b);
        let c = PlaceholderCsr.generate("rig-02").expect("gen");
        assert_ne!(a.csr_der, c.csr_der);
        // The CSR never carries the private key bytes.
        assert!(!a.csr_der.windows(3).any(|w| w == b"KEY"));
        // Debug redacts the private key.
        assert!(format!("{a:?}").contains("<redacted>"));
    }

    #[test]
    fn keystore_round_trips_and_redacts_debug() {
        let dir = std::env::temp_dir().join(format!("loom-hostd-ks-{}", std::process::id()));
        let path = dir.join("keystore.json");
        let node = EnrolledNode {
            agent_id: "node-abc".to_string(),
            node_cert_der: b"cert".to_vec(),
            ca_chain_der: b"chain".to_vec(),
            private_key_der: b"secret".to_vec(),
        };
        assert!(EnrolledNode::load(&path).expect("load empty").is_none());
        node.save(&path).expect("save");
        let loaded = EnrolledNode::load(&path).expect("load").expect("present");
        assert_eq!(loaded, node);
        assert!(format!("{loaded:?}").contains("<redacted>"));
        assert!(!format!("{loaded:?}").contains("secret"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
