// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The one error type the security bootstrap returns.
//!
//! It separates the *refusal* cases a caller acts on — a malformed or bad-`MAC`
//! enrollment token ([`BootstrapError::TokenInvalid`]) versus an expired but
//! well-formed one ([`BootstrapError::TokenExpired`]) — from the plumbing
//! failures (`I/O`, `TOML`, the `X.509` backend, persistence) it merely
//! propagates. The enrollment path answers "refuse this agent" on either token
//! variant; keeping them distinct lets `loomd` log *why* without leaking the
//! token bytes.

use loom_store::StoreError;

/// A failure from a security-bootstrap operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BootstrapError {
    /// The secrets directory already holds a bootstrap; refuse to overwrite a
    /// live `CA` and admin token.
    #[error("bootstrap already initialized at this path")]
    AlreadyInitialized,

    /// The secrets directory holds no bootstrap to load.
    #[error("no bootstrap found at this path")]
    NotInitialized,

    /// An enrollment token was malformed, carried a bad `MAC`, or was signed by
    /// a different enrollment key — an agent presenting it is refused.
    #[error("enrollment token invalid")]
    TokenInvalid,

    /// A well-formed enrollment token whose expiry has passed — refused.
    #[error("enrollment token expired")]
    TokenExpired,

    /// The `X.509` backend (`rcgen`) failed to generate, parse, or sign.
    #[error("certificate error: {0}")]
    Certificate(#[from] rcgen::Error),

    /// A persisted certificate `PEM` could not be decoded to `DER` (PR-09b enrollment).
    #[error("certificate encoding error: {0}")]
    CertificateEncoding(String),

    /// A filesystem operation on the secrets directory failed.
    #[error("secrets I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The `secrets.toml` file could not be parsed.
    #[error("secrets file parse error: {0}")]
    SecretsParse(#[from] toml::de::Error),

    /// The `secrets.toml` file could not be serialized.
    #[error("secrets file serialize error: {0}")]
    SecretsSerialize(#[from] toml::ser::Error),

    /// Hex-decoding persisted key or token material failed.
    #[error("malformed hex in persisted secret")]
    MalformedHex,

    /// The operating-system random source was unavailable.
    #[error("secure random source unavailable: {0}")]
    Random(String),

    /// Persisting the admin credential to the [`Store`](loom_store::Store) failed.
    #[error("store error: {0}")]
    Store(#[from] StoreError),
}
