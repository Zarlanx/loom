// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Newtype identifiers and monotonic tokens for the domain model.
//!
//! Every identifier is a distinct type so a `JobId` can never be passed where a
//! `NodeId` is expected. String-backed ids stay dependency-free: `loom-core`
//! mints no UUIDs (that would need randomness/I-O), it only carries identity
//! assigned upstream. [`FenceToken`] and [`AttemptNo`] are the two ordered,
//! `Copy` tokens the fencing rules depend on.

use core::fmt;

/// Declares a `String`-backed newtype identifier with the standard conversions.
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Wraps an owned or borrowed string as this identifier.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrows the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(
    /// Identifies a renter job — the unit of renter intent and the fencing lineage key.
    JobId
);
string_id!(
    /// Identifies one placement (attempt) of a job on a node.
    AttemptId
);
string_id!(
    /// Identifies a schedulable capacity offer (a host + GPU + tier + price).
    NodeId
);
string_id!(
    /// Identifies a host machine.
    HostId
);
string_id!(
    /// Identifies one GPU advertised by a host.
    GpuId
);
string_id!(
    /// Identifies a lease — the exclusive, expiring claim on a node for an attempt.
    LeaseId
);
string_id!(
    /// Identifies a billing account.
    AccountId
);
string_id!(
    /// Identifies a serving deployment.
    DeploymentId
);
string_id!(
    /// Identifies a serving replica (a long-lived attempt under a deployment).
    ReplicaId
);
string_id!(
    /// A content-addressed checkpoint location carried across a requeue lineage.
    CheckpointUri
);

/// A monotonic fencing token minted by the single-writer scheduler.
///
/// The split-brain guard (agent-protocol §5): a requeued attempt always receives
/// a strictly greater fence, and any message stamped with a stale (lower) fence
/// is rejected. Ordering is the whole point, so [`FenceToken`] is `Ord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FenceToken(pub(crate) u64);

impl FenceToken {
    /// Returns the underlying token value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Reconstructs a fence token from a persisted raw value.
    ///
    /// This is the deserialization inverse of [`value`](Self::value): the storage
    /// layer uses it to rehydrate a lease/attempt read back from the database.
    /// *Minting* new fences remains the sole responsibility of
    /// [`LeaseBook`](crate::LeaseBook) — this constructor never advances the
    /// monotonic counter and must not be used to fabricate a fence in scheduling
    /// logic.
    #[must_use]
    pub const fn from_persisted(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for FenceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A monotonic per-job attempt counter (1, 2, 3, … across a requeue lineage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttemptNo(pub(crate) u32);

impl AttemptNo {
    /// The first attempt of a job.
    pub const FIRST: Self = Self(1);

    /// Returns the underlying counter value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Reconstructs an attempt number from a persisted raw value.
    ///
    /// The deserialization inverse of [`get`](Self::get), used by the storage
    /// layer to rehydrate an attempt read back from the database. New attempt
    /// numbers are produced by [`next`](Self::next); this must not be used to
    /// skip the monotone sequence in scheduling logic.
    #[must_use]
    pub const fn from_persisted(value: u32) -> Self {
        Self(value)
    }

    /// Returns the next attempt number.
    ///
    /// # Panics
    ///
    /// Panics if the counter has reached [`u32::MAX`]. Saturating here would let
    /// two consecutive attempts share a number, silently breaking the strictly
    /// monotone invariant the fencing rules depend on (agent-protocol §5); a
    /// panic surfaces that violation instead of hiding it.
    #[must_use]
    pub const fn next(self) -> Self {
        match self.0.checked_add(1) {
            Some(n) => Self(n),
            None => panic!("AttemptNo overflow: monotonic invariant violated"),
        }
    }
}

impl fmt::Display for AttemptNo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[test]
    fn distinct_ids_display_their_inner_value() {
        let job = JobId::new("job-1");
        assert_eq!(job.as_str(), "job-1");
        assert_eq!(job.to_string(), "job-1");
        assert_eq!(JobId::from("job-1"), job);
        assert_eq!(JobId::from(String::from("job-1")), job);
    }

    #[test]
    fn fence_tokens_are_totally_ordered() {
        assert!(FenceToken(7) < FenceToken(8));
        assert_eq!(FenceToken(8).value(), 8);
        assert_eq!(FenceToken(8).to_string(), "8");
    }

    #[test]
    fn fence_token_round_trips_through_a_persisted_value() {
        // The storage layer rehydrates a fence via `from_persisted`; it must be
        // the exact inverse of `value`, and never mint a fresh counter.
        assert_eq!(FenceToken::from_persisted(42).value(), 42);
        assert_eq!(FenceToken::from_persisted(42), FenceToken(42));
    }

    #[test]
    fn attempt_numbers_increment() {
        assert_eq!(AttemptNo::FIRST, AttemptNo(1));
        assert_eq!(AttemptNo(1).next(), AttemptNo(2));
        assert_eq!(AttemptNo(3).get(), 3);
    }

    #[test]
    fn attempt_no_round_trips_through_a_persisted_value() {
        assert_eq!(AttemptNo::from_persisted(3).get(), 3);
        assert_eq!(AttemptNo::from_persisted(3), AttemptNo(3));
    }

    #[test]
    #[should_panic(expected = "AttemptNo overflow")]
    fn attempt_number_overflow_panics() {
        let _ = AttemptNo(u32::MAX).next();
    }
}
