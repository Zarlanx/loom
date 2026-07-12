// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The local certificate authority: a self-signed root that signs one node
//! certificate per enrolled agent.
//!
//! This is the trust anchor of the fleet's `mTLS` (security.md §7): every
//! agent↔control-plane link is mutually authenticated, the agent presenting a
//! per-enrollment client certificate this `CA` signed. The `CA` is generated once
//! at bootstrap, its certificate published as the anchor agents pin, and its
//! private key held only in the owner-only secrets file.
//!
//! Signing is `CSR`-driven: the agent generates its own key pair and sends a
//! certificate signing request, so the agent's private key never leaves its host
//! and the `CA` only ever sees a public key to sign. [`LocalCa::sign_node_cert`]
//! parses and verifies the `CSR`'s self-signature (via `rcgen`) before issuing.

use core::fmt;

use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls_pki_types::{CertificateDer, CertificateSigningRequestDer, pem::PemObject};

use super::error::BootstrapError;

/// The common name stamped on the local `CA` certificate's subject.
const CA_COMMON_NAME: &str = "Loom Local CA";

/// A local certificate authority able to sign agent node certificates.
///
/// Holds the `CA` private key in memory for the process lifetime; construct it
/// with [`generate`](Self::generate) at bootstrap or [`from_pem`](Self::from_pem)
/// on a later boot, then sign agent `CSR`s with
/// [`sign_node_cert`](Self::sign_node_cert).
pub struct LocalCa {
    /// The issuer template used when signing leaves. On a reload this is a
    /// re-`self_signed` reconstruction carrying the persisted `CA`'s subject and
    /// key identity — `rcgen` uses only its distinguished name, key-id method,
    /// and key usages, so leaves it signs still chain to [`cert_pem`](Self::cert_pem).
    issuer: rcgen::Certificate,
    /// The `CA` signing key.
    key: KeyPair,
    /// The canonical, persisted `CA` certificate in PEM — the anchor agents pin.
    cert_pem: String,
}

impl LocalCa {
    /// Generates a fresh self-signed `CA` (`ECDSA` `P-256`).
    ///
    /// # Errors
    /// [`BootstrapError::Certificate`] if key generation or self-signing fails.
    pub fn generate() -> Result<Self, BootstrapError> {
        let key = KeyPair::generate()?;
        let params = Self::ca_params()?;
        let cert = params.self_signed(&key)?;
        let cert_pem = cert.pem();
        Ok(Self {
            issuer: cert,
            key,
            cert_pem,
        })
    }

    /// Reloads a `CA` from its persisted certificate and key PEM.
    ///
    /// The certificate PEM is retained verbatim as the trust anchor; the issuer
    /// template is reconstructed from it so freshly signed leaves chain back to
    /// the exact bytes agents pinned.
    ///
    /// # Errors
    /// [`BootstrapError::Certificate`] if either PEM cannot be parsed or the
    /// issuer template cannot be reconstructed.
    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self, BootstrapError> {
        let key = KeyPair::from_pem(key_pem)?;
        let params = CertificateParams::from_ca_cert_pem(cert_pem)?;
        let issuer = params.self_signed(&key)?;
        Ok(Self {
            issuer,
            key,
            cert_pem: cert_pem.to_owned(),
        })
    }

    /// The `CA` certificate in PEM — the public trust anchor agents pin.
    #[must_use]
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// The `CA` private key in PEM.
    ///
    /// This is secret material; the caller writes it only to the owner-only
    /// secrets file, never to a log or the wire.
    #[must_use]
    pub fn key_pem(&self) -> String {
        self.key.serialize_pem()
    }

    /// Signs an agent's certificate signing request (PEM) into a node certificate,
    /// returning the leaf certificate in PEM.
    ///
    /// The `CSR`'s self-signature is verified while parsing; the issued leaf is
    /// constrained to an end-entity (`CA:FALSE`) certificate usable for `mTLS`
    /// client and server authentication.
    ///
    /// # Errors
    /// [`BootstrapError::Certificate`] if the `CSR` is malformed, its self-
    /// signature does not verify, or signing fails.
    pub fn sign_node_cert(&self, csr_pem: &str) -> Result<String, BootstrapError> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
        Ok(self.sign_csr(csr)?.pem())
    }

    /// Signs an agent's `DER`-encoded `CSR` — the form carried on the wire in
    /// `EnrollRequest.csr_der` (agent-protocol.md §3a) — into a node certificate,
    /// returning the leaf certificate as `DER` bytes ready for `EnrollGrant.node_cert_der`.
    ///
    /// Same constraints as [`sign_node_cert`](Self::sign_node_cert): the self-signature is
    /// verified and the leaf is an end-entity `mTLS` identity, never a `CA`.
    ///
    /// # Errors
    /// [`BootstrapError::Certificate`] if the `CSR` is malformed, its self-signature does
    /// not verify, or signing fails.
    pub fn sign_node_cert_der(&self, csr_der: &[u8]) -> Result<Vec<u8>, BootstrapError> {
        let csr = CertificateSigningRequestParams::from_der(&CertificateSigningRequestDer::from(
            csr_der,
        ))?;
        Ok(self.sign_csr(csr)?.der().to_vec())
    }

    /// The persisted `CA` certificate as `DER` bytes — the trust anchor an
    /// `EnrollGrant.ca_chain_der` carries so a joining agent can validate the chain.
    ///
    /// Decoded from the canonical [`cert_pem`](Self::cert_pem) rather than from the
    /// reconstructed issuer template, whose re-signed bytes would differ from what agents
    /// pinned.
    ///
    /// # Errors
    /// [`BootstrapError::CertificateEncoding`] if the persisted `CA` `PEM` cannot be
    /// decoded (not expected — this crate wrote it).
    pub fn cert_der(&self) -> Result<Vec<u8>, BootstrapError> {
        let der = CertificateDer::from_pem_slice(self.cert_pem.as_bytes())
            .map_err(|e| BootstrapError::CertificateEncoding(e.to_string()))?;
        Ok(der.to_vec())
    }

    /// Constrains a parsed `CSR` to an end-entity `mTLS` identity and signs it under this
    /// `CA`. Shared by the `PEM` and `DER` entry points.
    fn sign_csr(
        &self,
        mut csr: CertificateSigningRequestParams,
    ) -> Result<rcgen::Certificate, BootstrapError> {
        // Constrain the issued node cert regardless of what the CSR requested: an
        // end-entity mTLS identity, never a CA.
        csr.params.is_ca = IsCa::ExplicitNoCa;
        csr.params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ClientAuth,
            ExtendedKeyUsagePurpose::ServerAuth,
        ];
        Ok(csr.signed_by(&self.issuer, &self.key)?)
    }

    /// Builds the parameters for the local `CA` certificate.
    fn ca_params() -> Result<CertificateParams, BootstrapError> {
        let mut params = CertificateParams::new(Vec::new())?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        params.use_authority_key_identifier_extension = true;
        params
            .distinguished_name
            .push(DnType::CommonName, CA_COMMON_NAME);
        Ok(params)
    }
}

impl fmt::Debug for LocalCa {
    /// Redacts the private key; only the public certificate PEM is shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalCa")
            .field("key", &"<redacted>")
            .field("cert_pem", &self.cert_pem)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Builds an agent-side key pair and a CSR for `common_name`, as `loom-hostd`
    /// would during enrollment.
    fn agent_csr(common_name: &str) -> String {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![common_name.to_owned()]).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.serialize_request(&key).unwrap().pem().unwrap()
    }

    /// Builds an agent-side `CSR` in DER form, as it rides the wire in
    /// `EnrollRequest.csr_der`.
    fn agent_csr_der(common_name: &str) -> Vec<u8> {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![common_name.to_owned()]).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.serialize_request(&key).unwrap().der().to_vec()
    }

    #[test]
    fn a_generated_ca_signs_a_node_csr() {
        let ca = LocalCa::generate().unwrap();
        assert!(ca.cert_pem().contains("BEGIN CERTIFICATE"));
        let leaf = ca.sign_node_cert(&agent_csr("node-1")).unwrap();
        assert!(leaf.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn a_generated_ca_signs_a_der_csr_and_exposes_its_cert_der() {
        let ca = LocalCa::generate().unwrap();
        let leaf_der = ca.sign_node_cert_der(&agent_csr_der("node-der")).unwrap();
        assert!(!leaf_der.is_empty());
        // The persisted CA cert decodes to non-empty DER (the EnrollGrant trust anchor).
        assert!(!ca.cert_der().unwrap().is_empty());
    }

    #[test]
    fn a_garbage_der_csr_is_rejected() {
        let ca = LocalCa::generate().unwrap();
        assert!(matches!(
            ca.sign_node_cert_der(b"not a der csr"),
            Err(BootstrapError::Certificate(_))
        ));
    }

    #[test]
    fn a_reloaded_ca_still_signs() {
        let ca = LocalCa::generate().unwrap();
        let reloaded = LocalCa::from_pem(ca.cert_pem(), &ca.key_pem()).unwrap();
        assert_eq!(reloaded.cert_pem(), ca.cert_pem());
        assert!(reloaded.sign_node_cert(&agent_csr("node-2")).is_ok());
    }

    #[test]
    fn a_garbage_csr_is_rejected() {
        let ca = LocalCa::generate().unwrap();
        assert!(matches!(
            ca.sign_node_cert("not a csr"),
            Err(BootstrapError::Certificate(_))
        ));
    }

    #[test]
    fn debug_does_not_leak_the_private_key() {
        let ca = LocalCa::generate().unwrap();
        let shown = format!("{ca:?}");
        assert!(shown.contains("<redacted>"));
        assert!(!shown.contains("PRIVATE KEY"));
    }
}
