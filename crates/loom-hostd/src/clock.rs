// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The wall-clock seam.
//!
//! `Envelope.timestamp_ms` is an advisory wall-clock stamp (`agent-protocol.md` §3b);
//! anything that *bills* uses monotonic counters instead. Wall-clock reads are injected so
//! tests are deterministic and so the single real `SystemTime::now` call is isolated —
//! `loom-core` bans that method outright, and the agent keeps its one use behind this seam.

/// A source of advisory wall-clock time in Unix milliseconds.
pub trait WallClock: Send + Sync + std::fmt::Debug {
    /// Milliseconds since the Unix epoch (advisory; see the module note).
    fn now_unix_ms(&self) -> i64;
}

/// The production clock, reading the host RTC.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl WallClock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        // The one sanctioned wall-clock read in the agent (see the module note): advisory
        // only, never a billing input.
        #[allow(clippy::disallowed_methods)]
        let now = std::time::SystemTime::now();
        now.duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// A fixed clock for deterministic tests and fakes.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub i64);

impl WallClock for FixedClock {
    fn now_unix_ms(&self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{FixedClock, SystemClock, WallClock};

    #[test]
    fn fixed_clock_is_constant() {
        assert_eq!(FixedClock(42).now_unix_ms(), 42);
    }

    #[test]
    fn system_clock_is_after_2020() {
        // 2020-01-01T00:00:00Z in ms — a sanity floor that the real clock is plausible.
        assert!(SystemClock.now_unix_ms() > 1_577_836_800_000);
    }
}
