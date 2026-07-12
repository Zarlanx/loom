// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The lease — the scheduler's exclusive, expiring claim on a node for one
//! attempt, carrying the fencing token.
//!
//! This module holds the lease *data type*. The rules that mint and fence
//! leases live in [`crate::fencing`]; keeping the two apart lets the invariant
//! logic be reviewed as one coherent unit.

use crate::ids::{AttemptId, FenceToken, LeaseId, NodeId};
use crate::time::Timestamp;

/// Lifecycle status of a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LeaseState {
    /// Live and renewable.
    #[default]
    Active,
    /// Lapsed without renewal — fail-safe (control-plane §3).
    Expired,
    /// Voluntarily given up.
    Released,
}

/// An exclusive claim on a node for a single attempt, with an expiry and a
/// monotonic fencing token (control-plane §4, agent-protocol §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// Stable lease identifier.
    pub id: LeaseId,
    /// The attempt this lease authorizes.
    pub attempt: AttemptId,
    /// The node the attempt runs on.
    pub node: NodeId,
    /// The fencing token — this lease's version number.
    pub fence: FenceToken,
    /// When the lease was granted.
    pub granted_at: Timestamp,
    /// When the lease lapses without renewal.
    pub expires_at: Timestamp,
    /// Current lease status.
    pub state: LeaseState,
}

impl Lease {
    /// Whether the lease has lapsed by `now` (expiry reached, exclusive right gone).
    #[must_use]
    pub fn is_expired_at(&self, now: Timestamp) -> bool {
        now >= self.expires_at
    }

    /// Extends the expiry to `new_expiry` and marks the lease active (a renewal).
    pub fn renew(&mut self, new_expiry: Timestamp) {
        self.expires_at = new_expiry;
        self.state = LeaseState::Active;
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    fn lease() -> Lease {
        Lease {
            id: LeaseId::new("lease-1"),
            attempt: AttemptId::new("at-1"),
            node: NodeId::new("node-1"),
            fence: FenceToken(7),
            granted_at: Timestamp::from_millis(0),
            expires_at: Timestamp::from_millis(30_000),
            state: LeaseState::Active,
        }
    }

    #[test]
    fn lease_expires_at_or_after_its_deadline() {
        let l = lease();
        assert!(!l.is_expired_at(Timestamp::from_millis(29_999)));
        assert!(l.is_expired_at(Timestamp::from_millis(30_000)));
        assert!(l.is_expired_at(Timestamp::from_millis(30_001)));
    }

    #[test]
    fn renew_pushes_the_deadline_and_reactivates() {
        let mut l = lease();
        l.state = LeaseState::Expired;
        l.renew(Timestamp::from_millis(60_000));
        assert_eq!(l.state, LeaseState::Active);
        assert!(!l.is_expired_at(Timestamp::from_millis(59_999)));
    }
}
