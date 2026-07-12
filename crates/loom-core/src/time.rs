// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Injected time.
//!
//! `loom-core` is pure: it never reads a wall clock (`std::time::SystemTime::now`
//! is a denied method). Every state machine that needs "now" takes a
//! [`Timestamp`] argument, so tests drive time deterministically and the
//! deterministic heart stays deterministic.

use core::fmt;

/// A point in time, stored as integer milliseconds since an unspecified epoch.
///
/// The epoch is a caller convention (Unix millis in practice); `loom-core` only
/// relies on ordering and differences, never on an absolute origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp {
    millis: i64,
}

impl Timestamp {
    /// Constructs a timestamp from milliseconds since the epoch.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self { millis }
    }

    /// Returns the milliseconds since the epoch.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.millis
    }

    /// Milliseconds elapsed since `earlier`, saturating (never negative-overflows).
    ///
    /// Returns a negative value if `earlier` is actually later than `self`.
    #[must_use]
    pub const fn saturating_millis_since(self, earlier: Self) -> i64 {
        self.millis.saturating_sub(earlier.millis)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.millis)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_order_and_difference() {
        let a = Timestamp::from_millis(1_000);
        let b = Timestamp::from_millis(4_500);
        assert!(a < b);
        assert_eq!(b.saturating_millis_since(a), 3_500);
        assert_eq!(a.saturating_millis_since(b), -3_500);
        assert_eq!(b.as_millis(), 4_500);
    }

    #[test]
    fn difference_saturates_instead_of_overflowing() {
        let lo = Timestamp::from_millis(i64::MIN);
        let hi = Timestamp::from_millis(i64::MAX);
        assert_eq!(hi.saturating_millis_since(lo), i64::MAX);
    }
}
