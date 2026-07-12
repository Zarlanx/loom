// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The on-disk secrets layout a bootstrap writes and a later boot reloads.
//!
//! A bootstrap owns one directory (`loomd`'s state dir in practice) holding three
//! files:
//!
//! | File | Contents | Sensitivity |
//! |------|----------|-------------|
//! | `ca.crt` | the local `CA` certificate (PEM) | public — the anchor agents pin |
//! | `ca.key` | the local `CA` private key (PEM) | **secret** — `0600` |
//! | `secrets.toml` | enrollment key + admin account/key ids + admin token hash | **secret** — `0600` |
//!
//! The two secret files are created `0600` and the directory hardened to `0700`
//! on Unix (best-effort elsewhere) so the `CA` key and enrollment key are never
//! group- or world-readable. Crucially, **no plaintext bearer token is ever
//! written** — the admin token lives only as its `SHA-256` hash here (and in the
//! store's `api_keys`), and the enrollment key is an `HMAC` key, not a token.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::BootstrapError;

/// The `CA` certificate filename within the secrets directory.
pub(crate) const CA_CERT_FILE: &str = "ca.crt";
/// The `CA` private-key filename within the secrets directory.
pub(crate) const CA_KEY_FILE: &str = "ca.key";
/// The secrets-manifest filename within the secrets directory.
pub(crate) const SECRETS_FILE: &str = "secrets.toml";
/// The current on-disk secrets-layout version.
pub(crate) const SECRETS_VERSION: u32 = 1;

/// The non-certificate bootstrap secrets, as serialized to `secrets.toml`.
///
/// Holds the enrollment `HMAC` key (hex) and the identity of the admin
/// credential the bootstrap persisted — never a plaintext token.
#[derive(Clone, Serialize, Deserialize)]
pub struct Secrets {
    /// The on-disk layout version, for forward-compatible migration.
    pub version: u32,
    /// The enrollment-token `HMAC` key, hex-encoded (secret).
    pub enrollment_key: String,
    /// The `accounts.id` of the bootstrap admin account.
    pub admin_account_id: String,
    /// The `api_keys.id` of the admin token's key row.
    pub admin_key_id: String,
    /// The `SHA-256` hex hash of the admin token (persisted, never the token).
    pub admin_token_hash: String,
}

impl Secrets {
    /// Serializes to the `secrets.toml` on-disk form.
    ///
    /// # Errors
    /// [`BootstrapError::SecretsSerialize`] if `TOML` serialization fails.
    pub fn to_toml(&self) -> Result<String, BootstrapError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Parses the `secrets.toml` on-disk form.
    ///
    /// # Errors
    /// [`BootstrapError::SecretsParse`] if the input is not valid `secrets.toml`.
    pub fn from_toml(text: &str) -> Result<Self, BootstrapError> {
        Ok(toml::from_str(text)?)
    }
}

impl core::fmt::Debug for Secrets {
    /// Redacts the enrollment key; the ids and admin hash are not secret.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Secrets")
            .field("version", &self.version)
            .field("enrollment_key", &"<redacted>")
            .field("admin_account_id", &self.admin_account_id)
            .field("admin_key_id", &self.admin_key_id)
            .field("admin_token_hash", &self.admin_token_hash)
            .finish()
    }
}

/// The path of a named file within the secrets directory.
pub(crate) fn path_in(dir: &Path, file: &str) -> PathBuf {
    dir.join(file)
}

/// Writes a secret file, creating it `0600` on Unix so it is never group- or
/// world-readable.
pub(crate) fn write_secret_file(path: &Path, contents: &str) -> Result<(), BootstrapError> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Writes a non-secret file (the `CA` certificate anchor).
pub(crate) fn write_public_file(path: &Path, contents: &str) -> Result<(), BootstrapError> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o644);
    }
    let mut file = opts.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Hardens the secrets directory to `0700` on Unix (best-effort elsewhere).
pub(crate) fn harden_dir(dir: &Path) -> Result<(), BootstrapError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn secrets_round_trip_through_toml() {
        let secrets = Secrets {
            version: SECRETS_VERSION,
            enrollment_key: "aa".repeat(32),
            admin_account_id: "acct_x".to_owned(),
            admin_key_id: "key_x".to_owned(),
            admin_token_hash: "ff".repeat(32),
        };
        let restored = Secrets::from_toml(&secrets.to_toml().unwrap()).unwrap();
        assert_eq!(restored.version, secrets.version);
        assert_eq!(restored.enrollment_key, secrets.enrollment_key);
        assert_eq!(restored.admin_token_hash, secrets.admin_token_hash);
    }

    #[test]
    fn debug_redacts_the_enrollment_key() {
        let secrets = Secrets {
            version: SECRETS_VERSION,
            enrollment_key: "aa".repeat(32),
            admin_account_id: "acct_x".to_owned(),
            admin_key_id: "key_x".to_owned(),
            admin_token_hash: "ff".repeat(32),
        };
        let shown = format!("{secrets:?}");
        assert!(shown.contains("<redacted>"));
        assert!(!shown.contains(&"aa".repeat(32)));
    }

    #[cfg(unix)]
    #[test]
    fn a_secret_file_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(dir.path(), SECRETS_FILE);
        write_secret_file(&path, "x = 1").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
