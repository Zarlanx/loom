// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Establishing (and re-establishing) the single outbound control connection
//! (`agent-protocol.md` §1.1, §1.3).
//!
//! The agent is **outbound-only** (`networking.md` §2): it opens one connection to the
//! gateway and never listens. [`Connector`] is the seam that produces a fresh
//! [`WsTransport`] per attempt — a real [`WssConnector`] in production, an in-process
//! duplex connector in tests — so the reconnect/backoff logic is identical and fully
//! testable without a network. Reconnect uses **exponential backoff + full jitter**
//! (base 500 ms, cap ~30 s, §1.3), modelled here as pure, unit-tested arithmetic.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{config::ReconnectConfig, transport::TransportError, transport::WsTransport};

/// Produces a fresh control-channel transport for each (re)connect attempt.
///
/// Implementations are used generically (never as a `dyn` trait object), so `async fn` in
/// the trait needs no `Send` desugaring — the reconnect driver is monomorphized per
/// connector.
#[allow(async_fn_in_trait)]
pub trait Connector {
    /// The underlying byte stream the WebSocket runs over.
    type Stream: AsyncRead + AsyncWrite + Unpin;

    /// Opens a new connection and completes the WebSocket handshake.
    ///
    /// # Errors
    ///
    /// [`TransportError`] if the connection or handshake fails; the reconnect loop treats
    /// this as transient and backs off.
    async fn connect(&self) -> Result<WsTransport<Self::Stream>, TransportError>;
}

/// Exponential-backoff-with-full-jitter bounds (`agent-protocol.md` §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    base: Duration,
    cap: Duration,
}

impl BackoffPolicy {
    /// Constructs a policy, clamping `cap` up to at least `base`.
    #[must_use]
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap: cap.max(base),
        }
    }

    /// The un-jittered delay ceiling for a zero-based `attempt`: `min(cap, base * 2^attempt)`.
    ///
    /// Saturating: a large `attempt` pins at `cap` rather than overflowing.
    #[must_use]
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let factor = 1u64.checked_shl(attempt.min(63)).unwrap_or(u64::MAX);
        let scaled = self
            .base
            .checked_mul(u32::try_from(factor.min(u64::from(u32::MAX))).unwrap_or(u32::MAX))
            .unwrap_or(self.cap);
        scaled.min(self.cap)
    }

    /// The actual backoff for `attempt`: full jitter in `[0, ceiling]`, drawn from `jitter`.
    #[must_use]
    pub fn delay(&self, attempt: u32, jitter: &mut Jitter) -> Duration {
        jitter.full(self.ceiling(attempt))
    }
}

impl From<ReconnectConfig> for BackoffPolicy {
    fn from(rc: ReconnectConfig) -> Self {
        Self::new(rc.base(), rc.cap())
    }
}

/// A tiny deterministic PRNG (`splitmix64`) for full-jitter selection.
///
/// Seedable so tests are reproducible; no `rand` dependency and no wall-clock read. Full
/// jitter (a uniform draw in `[0, ceiling]`) is the recommended anti-thundering-herd
/// backoff and is what §1.3 specifies.
#[derive(Debug, Clone)]
pub struct Jitter {
    state: u64,
}

impl Jitter {
    /// A generator seeded with `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next raw 64-bit value.
    fn next_u64(&mut self) -> u64 {
        // splitmix64.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform draw in `[0, ceiling]`.
    #[must_use]
    pub fn full(&mut self, ceiling: Duration) -> Duration {
        let ceil_ms = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
        if ceil_ms == 0 {
            return Duration::ZERO;
        }
        // Inclusive of the ceiling via `% (ceil_ms + 1)`.
        Duration::from_millis(self.next_u64() % (ceil_ms + 1))
    }
}

/// The production connector: a real WSS connection via `tokio-tungstenite`.
///
/// Not exercised by the (network-free) test suite; the reconnect/enrollment logic is
/// proven against the in-process duplex connector instead. mTLS client-cert presentation
/// and gateway-identity pinning layer on here once enrollment mints the node cert
/// (`agent-protocol.md` §1.2, PR-09/PR-21).
#[derive(Debug, Clone)]
pub struct WssConnector {
    endpoint: String,
}

impl WssConnector {
    /// A connector targeting `endpoint` (e.g. `wss://loomd.local:8443/agent`).
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

impl Connector for WssConnector {
    type Stream = tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>;

    async fn connect(&self) -> Result<WsTransport<Self::Stream>, TransportError> {
        let (ws, _resp) = tokio_tungstenite::connect_async(&self.endpoint).await?;
        Ok(WsTransport::new(ws))
    }
}

#[cfg(test)]
mod tests {
    use super::{BackoffPolicy, Jitter};
    use std::time::Duration;

    #[test]
    fn ceiling_doubles_then_saturates_at_cap() {
        let p = BackoffPolicy::new(Duration::from_millis(500), Duration::from_secs(30));
        assert_eq!(p.ceiling(0), Duration::from_millis(500));
        assert_eq!(p.ceiling(1), Duration::from_millis(1000));
        assert_eq!(p.ceiling(2), Duration::from_millis(2000));
        // Eventually pinned at the cap, never beyond, even for absurd attempts.
        assert_eq!(p.ceiling(20), Duration::from_secs(30));
        assert_eq!(p.ceiling(1000), Duration::from_secs(30));
    }

    #[test]
    fn cap_is_clamped_up_to_base() {
        let p = BackoffPolicy::new(Duration::from_secs(5), Duration::from_secs(1));
        assert_eq!(p.ceiling(0), Duration::from_secs(5));
    }

    #[test]
    fn full_jitter_stays_within_ceiling() {
        let p = BackoffPolicy::new(Duration::from_millis(500), Duration::from_secs(30));
        let mut j = Jitter::new(0xDEAD_BEEF);
        for attempt in 0..12 {
            let ceiling = p.ceiling(attempt);
            for _ in 0..256 {
                let d = p.delay(attempt, &mut j);
                assert!(d <= ceiling, "delay {d:?} exceeded ceiling {ceiling:?}");
            }
        }
    }

    #[test]
    fn zero_ceiling_yields_zero_delay() {
        let mut j = Jitter::new(1);
        assert_eq!(j.full(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn jitter_is_deterministic_for_a_seed() {
        let mut a = Jitter::new(7);
        let mut b = Jitter::new(7);
        let ceiling = Duration::from_millis(1000);
        for _ in 0..16 {
            assert_eq!(a.full(ceiling), b.full(ceiling));
        }
    }
}
