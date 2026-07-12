// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Clock`] — the wall-clock seam the enrollment path reads `now` from.
//!
//! `loom-core` forbids reading the clock so its logic stays pure; a service crate like
//! this one *does* read it, but behind a seam so a test can pin `now` and exercise token
//! expiry deterministically. [`SystemClock`] is the production reader; tests inject their
//! own [`Clock`] (see the `test-support` `FixedClock`).

use loom_core::Timestamp;

/// A source of the current wall-clock time, in [`Timestamp`] form.
///
/// `Send + Sync` so an `Arc<dyn Clock>` can be shared across connection tasks.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Timestamp;
}

/// The production [`Clock`]: `std::time::SystemTime` since the Unix epoch.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        // This is the single sanctioned wall-clock read in the crate: the injection point
        // the disallowed-methods rule (clippy.toml) exists to funnel every clock read
        // through, keeping `loom-core` and every logic path deterministic.
        #[allow(clippy::disallowed_methods)]
        let system_now = std::time::SystemTime::now();
        // A clock set before 1970 is the only way `duration_since` fails; treat it as the
        // epoch rather than panic — enrollment token expiry then simply reads as "very
        // old", which fails safe (a stale token is refused).
        let millis = system_now
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Timestamp::from_millis(millis)
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, SystemClock};

    #[test]
    fn system_clock_reads_a_plausible_epoch_millis() {
        // Well after 2020-01-01 in ms — the clock is advancing, not stuck at the epoch.
        assert!(SystemClock.now().as_millis() > 1_577_836_800_000);
    }
}
