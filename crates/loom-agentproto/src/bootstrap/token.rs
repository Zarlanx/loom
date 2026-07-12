// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Bearer-token machinery: the stateless enrollment token an operator hands to a
//! joining agent, and the one-time standalone admin token.
//!
//! **Enrollment token.** A short-lived, stateless credential proving an operator
//! authorized an agent to enroll. It carries an expiry and a random nonce
//! authenticated by `HMAC-SHA256` under the node's [`EnrollmentKey`] (kept in the
//! secrets file), so the agent-gateway verifies it with no database round-trip
//! and a bad or expired token is refused. It is deliberately *not* individually
//! revocable — the defense is a short expiry, not a denylist — which is the right
//! trade for a single-operator self-host bootstrap (security.md §7).
//!
//! **Admin token.** A high-entropy standalone secret the operator uses to drive
//! the local control plane before any account exists. Only its `SHA-256` hash is
//! ever persisted (in `api_keys.key_hash`, the same lookup the renter API uses);
//! the plaintext is surfaced exactly once, at [`AdminCredential::mint`] time.
//! Because the token carries 256 bits of entropy, a fast hash is safe — there is
//! no low-entropy password to slow down an attacker against.

use core::fmt;

use hmac::{Hmac, Mac};
use loom_core::Timestamp;
use sha2::{Digest, Sha256};

use super::error::BootstrapError;

/// The wire prefix of a v1 enrollment token (`le1.<payload>.<tag>`, hex fields).
const ENROLLMENT_TOKEN_V1: &str = "le1";

/// Fills a fixed-size buffer from the operating-system CSPRNG.
fn random_bytes<const N: usize>() -> Result<[u8; N], BootstrapError> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| BootstrapError::Random(e.to_string()))?;
    Ok(buf)
}

/// Computes `HMAC-SHA256(key, msg)`.
///
/// `HMAC` accepts a key of any length, so construction from a fixed 32-byte key
/// is infallible — the `expect` documents that invariant rather than hiding a
/// real failure mode.
fn hmac_sha256(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(msg);
    mac.finalize().into_bytes().into()
}

/// Constant-time check that `tag` is `HMAC-SHA256(key, msg)`.
///
/// Kept private so the panic-free public [`EnrollmentKey::verify`] carries the
/// (never-triggering) infallible-key `expect` out of its own body.
fn verify_tag(key: &[u8; 32], msg: &[u8], tag: &[u8]) -> bool {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(msg);
    mac.verify_slice(tag).is_ok()
}

/// The secret `HMAC` key that signs and verifies enrollment tokens.
///
/// Persisted (hex-encoded) in the secrets file and loaded on every `loomd` boot,
/// so tokens issued before a restart still verify afterward.
#[derive(Clone)]
pub struct EnrollmentKey([u8; 32]);

impl EnrollmentKey {
    /// Mints a fresh random enrollment key from the `OS` CSPRNG.
    ///
    /// # Errors
    /// [`BootstrapError::Random`] if the operating-system random source is
    /// unavailable.
    pub fn generate() -> Result<Self, BootstrapError> {
        Ok(Self(random_bytes()?))
    }

    /// Reconstructs a key from its hex encoding as stored in the secrets file.
    ///
    /// # Errors
    /// [`BootstrapError::MalformedHex`] if `hex_str` is not exactly 32 hex-encoded
    /// bytes.
    pub fn from_hex(hex_str: &str) -> Result<Self, BootstrapError> {
        let bytes = hex::decode(hex_str).map_err(|_| BootstrapError::MalformedHex)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| BootstrapError::MalformedHex)?;
        Ok(Self(arr))
    }

    /// Hex-encodes the key for persistence in the secrets file.
    ///
    /// This exposes secret material; the caller writes it only to the
    /// owner-only secrets file, never to a log.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Issues an enrollment token valid for `ttl_millis` after `now`.
    ///
    /// # Errors
    /// [`BootstrapError::Random`] if the `OS` random source is unavailable.
    pub fn issue(
        &self,
        ttl_millis: i64,
        now: Timestamp,
    ) -> Result<EnrollmentToken, BootstrapError> {
        let nonce = hex::encode(random_bytes::<16>()?);
        let expires_at = now.as_millis().saturating_add(ttl_millis);
        let payload = format!("{expires_at}:{nonce}");
        let tag = hmac_sha256(&self.0, payload.as_bytes());
        Ok(EnrollmentToken(format!(
            "{ENROLLMENT_TOKEN_V1}.{}.{}",
            hex::encode(payload.as_bytes()),
            hex::encode(tag),
        )))
    }

    /// Verifies a presented enrollment token against this key and `now`.
    ///
    /// The `MAC` is checked in constant time before the expiry, so a forged token
    /// never reaches the clock comparison.
    ///
    /// # Errors
    /// [`BootstrapError::TokenInvalid`] if the token is malformed or its `MAC`
    /// does not verify; [`BootstrapError::TokenExpired`] if it is well-formed but
    /// past its expiry.
    pub fn verify(&self, token: &str, now: Timestamp) -> Result<EnrollmentClaims, BootstrapError> {
        let mut parts = token.split('.');
        let (Some(version), Some(payload_hex), Some(tag_hex), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(BootstrapError::TokenInvalid);
        };
        if version != ENROLLMENT_TOKEN_V1 {
            return Err(BootstrapError::TokenInvalid);
        }

        let payload = hex::decode(payload_hex).map_err(|_| BootstrapError::TokenInvalid)?;
        let tag = hex::decode(tag_hex).map_err(|_| BootstrapError::TokenInvalid)?;

        // Constant-time MAC check first — a forgery is refused before we read the
        // clock, and `verify_slice` (inside `verify_tag`) compares in constant time.
        if !verify_tag(&self.0, &payload, &tag) {
            return Err(BootstrapError::TokenInvalid);
        }

        let payload = String::from_utf8(payload).map_err(|_| BootstrapError::TokenInvalid)?;
        let (expires_raw, nonce) = payload
            .split_once(':')
            .ok_or(BootstrapError::TokenInvalid)?;
        let expires_millis: i64 = expires_raw
            .parse()
            .map_err(|_| BootstrapError::TokenInvalid)?;

        if now.as_millis() > expires_millis {
            return Err(BootstrapError::TokenExpired);
        }
        Ok(EnrollmentClaims {
            expires_at: Timestamp::from_millis(expires_millis),
            nonce: nonce.to_owned(),
        })
    }
}

impl fmt::Debug for EnrollmentKey {
    /// Redacts the key bytes — key material must never reach a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("EnrollmentKey").field(&"<redacted>").finish()
    }
}

/// An opaque enrollment token handed to a joining agent. It is a bearer secret;
/// its [`Debug`] is redacted so it does not leak through diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct EnrollmentToken(String);

impl EnrollmentToken {
    /// Borrows the token as the string an agent presents on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the token, yielding the owned wire string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for EnrollmentToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("EnrollmentToken")
            .field(&"<redacted>")
            .finish()
    }
}

/// The verified content of an enrollment token: its expiry and the random nonce
/// that makes each issued token unique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentClaims {
    /// The instant past which the token is refused.
    pub expires_at: Timestamp,
    /// The per-token random nonce (hex).
    pub nonce: String,
}

/// Hashes a presented token the way it is stored in `api_keys.key_hash`.
///
/// The hash is `SHA-256` hex; this is the exact value the auth path looks up via
/// [`Store::api_key_by_hash`](loom_store::Store::api_key_by_hash), so verifying an
/// admin token is `store.api_key_by_hash(&hash_token(presented))`.
#[must_use]
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// A freshly minted admin credential.
///
/// The [`token`](Self::token) is the plaintext the operator must save now — it is
/// never recoverable later, because only its [`token_hash`](Self::token_hash) is
/// persisted. The [`account_id`](Self::account_id)/[`key_id`](Self::key_id)
/// identify the `accounts`/`api_keys` rows the bootstrap writes.
#[derive(Clone)]
pub struct AdminCredential {
    /// The plaintext admin token, surfaced exactly once.
    pub token: String,
    /// The `accounts.id` of the bootstrap admin account.
    pub account_id: String,
    /// The `api_keys.id` of the admin token's key row.
    pub key_id: String,
    /// The `SHA-256` hex hash persisted in `api_keys.key_hash`.
    pub token_hash: String,
}

impl AdminCredential {
    /// Mints a fresh admin token plus the account/key identity the bootstrap will
    /// persist. Only the hash is durable; [`token`](Self::token) is shown once.
    ///
    /// # Errors
    /// [`BootstrapError::Random`] if the `OS` random source is unavailable.
    pub fn mint() -> Result<Self, BootstrapError> {
        let token = format!("loom_admin_{}", hex::encode(random_bytes::<32>()?));
        let token_hash = hash_token(&token);
        let account_id = format!("acct_{}", hex::encode(random_bytes::<12>()?));
        let key_id = format!("key_{}", hex::encode(random_bytes::<12>()?));
        Ok(Self {
            token,
            account_id,
            key_id,
            token_hash,
        })
    }
}

impl fmt::Debug for AdminCredential {
    /// Redacts the plaintext token; only the non-secret ids and hash are shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminCredential")
            .field("token", &"<redacted>")
            .field("account_id", &self.account_id)
            .field("key_id", &self.key_id)
            .field("token_hash", &self.token_hash)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ts(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    #[test]
    fn a_freshly_issued_token_verifies_before_expiry() {
        let key = EnrollmentKey::generate().unwrap();
        let token = key.issue(60_000, ts(1_000)).unwrap();
        let claims = key.verify(token.as_str(), ts(30_000)).unwrap();
        assert_eq!(claims.expires_at, ts(61_000));
        assert!(!claims.nonce.is_empty());
    }

    #[test]
    fn an_expired_token_is_refused() {
        let key = EnrollmentKey::generate().unwrap();
        let token = key.issue(10_000, ts(1_000)).unwrap();
        let err = key.verify(token.as_str(), ts(20_000)).unwrap_err();
        assert!(matches!(err, BootstrapError::TokenExpired));
    }

    #[test]
    fn a_token_from_a_different_key_is_refused() {
        let issuer = EnrollmentKey::generate().unwrap();
        let attacker = EnrollmentKey::generate().unwrap();
        let token = issuer.issue(60_000, ts(1_000)).unwrap();
        let err = attacker.verify(token.as_str(), ts(2_000)).unwrap_err();
        assert!(matches!(err, BootstrapError::TokenInvalid));
    }

    #[test]
    fn a_tampered_token_is_refused() {
        let key = EnrollmentKey::generate().unwrap();
        let token = key.issue(60_000, ts(1_000)).unwrap().into_string();
        // Flip the last hex nibble of the MAC.
        let mut bytes = token.into_bytes();
        let last = bytes.last_mut().unwrap();
        *last = if *last == b'a' { b'b' } else { b'a' };
        let tampered = String::from_utf8(bytes).unwrap();
        let err = key.verify(&tampered, ts(2_000)).unwrap_err();
        assert!(matches!(err, BootstrapError::TokenInvalid));
    }

    #[test]
    fn malformed_tokens_are_refused_not_panicking() {
        let key = EnrollmentKey::generate().unwrap();
        for bad in ["", "le1", "le1.zz.zz", "le2.00.00", "le1.00.00.00"] {
            assert!(matches!(
                key.verify(bad, ts(0)),
                Err(BootstrapError::TokenInvalid)
            ));
        }
    }

    #[test]
    fn enrollment_key_round_trips_through_hex() {
        let key = EnrollmentKey::generate().unwrap();
        let restored = EnrollmentKey::from_hex(&key.to_hex()).unwrap();
        // A token issued by the original verifies under the restored key.
        let token = key.issue(60_000, ts(0)).unwrap();
        assert!(restored.verify(token.as_str(), ts(1)).is_ok());
    }

    #[test]
    fn admin_token_mint_hashes_and_is_unique() {
        let a = AdminCredential::mint().unwrap();
        let b = AdminCredential::mint().unwrap();
        assert!(a.token.starts_with("loom_admin_"));
        assert_ne!(a.token, b.token);
        assert_ne!(a.account_id, b.account_id);
        assert_eq!(a.token_hash, hash_token(&a.token));
        assert_ne!(a.token_hash, b.token_hash);
        // The Debug redaction never prints the plaintext token.
        assert!(!format!("{a:?}").contains(&a.token));
    }
}
