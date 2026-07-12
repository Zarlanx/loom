// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Security bootstrap: the trust machinery `loomd init`/`loom init` mint once and
//! every later boot reloads.
//!
//! This is PR-26 `bootstrap-auth` — the security seam no other workstream owns.
//! It lives in `loom-agentproto` because both consumers sit at or above it: the
//! agent-gateway's enrollment handler (PR-09) signs node certs with the same
//! [`LocalCa`] this module defines, and `loomd` (PR-11) mints the bootstrap at
//! `init`. Placing it here — the lowest library both share, and one that already
//! links [`loom_store`] for the `accounts`/`api_keys` persistence — keeps the
//! `CA`/token code out of a binary neither side could import.
//!
//! A [`Bootstrap`] bundles three pieces (security.md §7):
//!
//! - a [`LocalCa`] — the self-signed root that signs one node cert per enrolled
//!   agent, anchoring fleet `mTLS`;
//! - an [`EnrollmentKey`] — the `HMAC` key behind stateless [enrollment
//!   tokens](EnrollmentToken), so the gateway admits an agent presenting a valid
//!   token and refuses a bad or expired one with no database round-trip;
//! - the [secrets file layout](secrets) plus the standalone [admin
//!   credential](AdminCredential), whose `SHA-256` hash is persisted in
//!   `api_keys` while its plaintext is shown exactly once.
//!
//! # Proving the bootstrap
//!
//! ```
//! use loom_agentproto::bootstrap::Bootstrap;
//! use loom_core::Timestamp;
//!
//! let dir = tempfile::tempdir().expect("temp dir");
//! let now = Timestamp::from_millis(1_000);
//!
//! // A fresh bootstrap mints a CA + a one-time admin token.
//! let (boot, admin) = Bootstrap::create(dir.path()).expect("bootstrap");
//! assert!(admin.token.starts_with("loom_admin_"));
//! assert!(boot.ca().cert_pem().contains("BEGIN CERTIFICATE"));
//!
//! // A valid enrollment token is honored; a bad one is refused.
//! let token = boot.issue_enrollment_token(60_000, now).expect("issue");
//! assert!(boot.verify_enrollment_token(token.as_str(), now).is_ok());
//! assert!(boot.verify_enrollment_token("le1.00.00", now).is_err());
//! ```

pub mod ca;
pub mod error;
pub mod secrets;
pub mod token;

use std::path::{Path, PathBuf};

use loom_core::{AccountId, Timestamp};
use loom_store::{Account, ApiKey, Store, StoreError};

pub use ca::LocalCa;
pub use error::BootstrapError;
pub use secrets::Secrets;
pub use token::{AdminCredential, EnrollmentClaims, EnrollmentKey, EnrollmentToken, hash_token};

use secrets::{
    CA_CERT_FILE, CA_KEY_FILE, SECRETS_FILE, SECRETS_VERSION, harden_dir, path_in,
    write_public_file, write_secret_file,
};

/// The human-facing name of the bootstrap admin account.
const ADMIN_ACCOUNT_NAME: &str = "admin";
/// The label stamped on the admin token's `api_keys` row.
const ADMIN_KEY_LABEL: &str = "loomd bootstrap admin token";

/// A loaded security bootstrap: the local `CA`, the enrollment key, and the
/// persisted secrets identity, rooted at one on-disk secrets directory.
#[derive(Debug)]
pub struct Bootstrap {
    ca: LocalCa,
    enrollment_key: EnrollmentKey,
    secrets: Secrets,
    dir: PathBuf,
}

impl Bootstrap {
    /// Performs a fresh bootstrap in `dir`: mints a local `CA`, an enrollment key,
    /// and a one-time admin token, then writes the secrets layout (`ca.crt`,
    /// `ca.key`, `secrets.toml`). Returns the loaded [`Bootstrap`] and the
    /// [`AdminCredential`] whose plaintext token the caller must surface now —
    /// only its hash is persisted.
    ///
    /// The store rows for the admin credential are written separately via
    /// [`persist_admin`](Self::persist_admin), which needs an async
    /// [`Store`](loom_store::Store) and stamps the rows with its `now`.
    ///
    /// # Errors
    /// [`BootstrapError::AlreadyInitialized`] if `dir` already holds a bootstrap;
    /// [`BootstrapError::Certificate`] if `CA` generation fails;
    /// [`BootstrapError::Random`] if the `OS` random source is unavailable;
    /// [`BootstrapError::Io`]/[`BootstrapError::SecretsSerialize`] if the secrets
    /// files cannot be written.
    pub fn create(dir: &Path) -> Result<(Self, AdminCredential), BootstrapError> {
        if Self::is_initialized(dir) {
            return Err(BootstrapError::AlreadyInitialized);
        }
        std::fs::create_dir_all(dir)?;
        harden_dir(dir)?;

        let ca = LocalCa::generate()?;
        let enrollment_key = EnrollmentKey::generate()?;
        let admin = AdminCredential::mint()?;
        let secrets = Secrets {
            version: SECRETS_VERSION,
            enrollment_key: enrollment_key.to_hex(),
            admin_account_id: admin.account_id.clone(),
            admin_key_id: admin.key_id.clone(),
            admin_token_hash: admin.token_hash.clone(),
        };

        write_public_file(&path_in(dir, CA_CERT_FILE), ca.cert_pem())?;
        write_secret_file(&path_in(dir, CA_KEY_FILE), &ca.key_pem())?;
        // secrets.toml is written last: is_initialized keys on it, so a bootstrap
        // interrupted before this point is re-attempted rather than half-adopted.
        write_secret_file(&path_in(dir, SECRETS_FILE), &secrets.to_toml()?)?;

        Ok((
            Self {
                ca,
                enrollment_key,
                secrets,
                dir: dir.to_path_buf(),
            },
            admin,
        ))
    }

    /// Reloads an existing bootstrap from the secrets directory `dir`.
    ///
    /// # Errors
    /// [`BootstrapError::NotInitialized`] if `dir` holds no bootstrap;
    /// [`BootstrapError::Io`]/[`BootstrapError::SecretsParse`] if the secrets
    /// files are missing or malformed;
    /// [`BootstrapError::Certificate`]/[`BootstrapError::MalformedHex`] if the
    /// persisted `CA` or enrollment key cannot be reconstructed.
    pub fn load(dir: &Path) -> Result<Self, BootstrapError> {
        if !Self::is_initialized(dir) {
            return Err(BootstrapError::NotInitialized);
        }
        let cert_pem = std::fs::read_to_string(path_in(dir, CA_CERT_FILE))?;
        let key_pem = std::fs::read_to_string(path_in(dir, CA_KEY_FILE))?;
        let secrets = Secrets::from_toml(&std::fs::read_to_string(path_in(dir, SECRETS_FILE))?)?;

        let ca = LocalCa::from_pem(&cert_pem, &key_pem)?;
        let enrollment_key = EnrollmentKey::from_hex(&secrets.enrollment_key)?;
        Ok(Self {
            ca,
            enrollment_key,
            secrets,
            dir: dir.to_path_buf(),
        })
    }

    /// Whether `dir` already holds a completed bootstrap.
    #[must_use]
    pub fn is_initialized(dir: &Path) -> bool {
        path_in(dir, SECRETS_FILE).exists()
    }

    /// The local `CA` — used by the agent-gateway to sign enrolled node certs and
    /// to publish the trust anchor.
    #[must_use]
    pub fn ca(&self) -> &LocalCa {
        &self.ca
    }

    /// The secrets directory this bootstrap is rooted at.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The `accounts.id` of the bootstrap admin account.
    #[must_use]
    pub fn admin_account_id(&self) -> &str {
        &self.secrets.admin_account_id
    }

    /// Issues an enrollment token valid for `ttl_millis` after `now`.
    ///
    /// # Errors
    /// [`BootstrapError::Random`] if the `OS` random source is unavailable.
    pub fn issue_enrollment_token(
        &self,
        ttl_millis: i64,
        now: Timestamp,
    ) -> Result<EnrollmentToken, BootstrapError> {
        self.enrollment_key.issue(ttl_millis, now)
    }

    /// Verifies a presented enrollment token.
    ///
    /// # Errors
    /// [`BootstrapError::TokenInvalid`] if the token is malformed or its `MAC`
    /// does not verify; [`BootstrapError::TokenExpired`] if it is past its expiry.
    pub fn verify_enrollment_token(
        &self,
        token: &str,
        now: Timestamp,
    ) -> Result<EnrollmentClaims, BootstrapError> {
        self.enrollment_key.verify(token, now)
    }

    /// Enrolls an agent: verifies its enrollment token, then signs its `CSR` into
    /// a node certificate, returning the leaf in PEM. A bad or expired token is
    /// refused before any certificate is signed.
    ///
    /// # Errors
    /// [`BootstrapError::TokenInvalid`]/[`BootstrapError::TokenExpired`] if the
    /// token is refused; [`BootstrapError::Certificate`] if the `CSR` is
    /// malformed or signing fails.
    pub fn enroll_node(
        &self,
        token: &str,
        csr_pem: &str,
        now: Timestamp,
    ) -> Result<String, BootstrapError> {
        self.verify_enrollment_token(token, now)?;
        self.ca.sign_node_cert(csr_pem)
    }

    /// Persists the bootstrap admin credential into the store: the admin account
    /// and its `api_keys` row (holding only the token's `SHA-256` hash). Idempotent
    /// — a row that already exists is left as-is, so re-running is safe.
    ///
    /// This is the auth-side of the admin token: a later request bearing the
    /// plaintext is authenticated by
    /// [`Store::api_key_by_hash`](loom_store::Store::api_key_by_hash) of
    /// [`hash_token`] of the presented value.
    ///
    /// # Errors
    /// [`BootstrapError::Store`] on a persistence failure other than a benign
    /// uniqueness conflict.
    pub async fn persist_admin(
        &self,
        store: &dyn Store,
        now: Timestamp,
    ) -> Result<(), BootstrapError> {
        let account_id = AccountId::new(&self.secrets.admin_account_id);
        let account = Account {
            id: account_id.clone(),
            name: ADMIN_ACCOUNT_NAME.to_owned(),
            created_at: now,
        };
        ignore_conflict(store.insert_account(&account).await)?;

        let key = ApiKey {
            id: self.secrets.admin_key_id.clone(),
            account: account_id,
            key_hash: self.secrets.admin_token_hash.clone(),
            label: ADMIN_KEY_LABEL.to_owned(),
            created_at: now,
            revoked: false,
        };
        ignore_conflict(store.insert_api_key(&key).await)?;
        Ok(())
    }
}

/// Treats a uniqueness conflict as success — the row is already persisted — so
/// [`persist_admin`](Bootstrap::persist_admin) is idempotent across re-runs.
fn ignore_conflict(result: Result<(), StoreError>) -> Result<(), BootstrapError> {
    match result {
        Ok(()) | Err(StoreError::Conflict(_)) => Ok(()),
        Err(other) => Err(other.into()),
    }
}
