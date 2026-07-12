// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The network-egress policy knob a workload declares.
//!
//! [isolation.md §5](../../../docs/platform/isolation.md) makes **default-deny
//! egress with an allowlist** the control that kills residential-IP abuse, and
//! makes reaching RFC1918 space the one rule that must never break. This module
//! is the *surface* for that policy — the driver that owns a real network
//! namespace (hardened `runc`, 07d) enforces it. The isolation-free
//! [`ProcessDriver`](crate::process::ProcessDriver) **cannot** enforce egress; it
//! records the policy and relies on the trusted-profile assumption instead, and
//! must never be handed untrusted work.

/// A single allowed egress destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRule {
    /// A DNS name or CIDR the workload may reach (e.g. the package-registry
    /// proxy, the Hugging Face cache, the job's object store).
    pub destination: String,
    /// The allowed ports; empty means any port on `destination`.
    pub ports: Vec<u16>,
}

impl EgressRule {
    /// Allows any port on `destination`.
    #[must_use]
    pub fn any_port(destination: impl Into<String>) -> Self {
        Self {
            destination: destination.into(),
            ports: Vec::new(),
        }
    }

    /// Allows only `ports` on `destination`.
    #[must_use]
    pub fn on_ports(destination: impl Into<String>, ports: Vec<u16>) -> Self {
        Self {
            destination: destination.into(),
            ports,
        }
    }
}

/// How much network a sandbox may reach.
///
/// The default is [`DenyAll`](EgressPolicy::DenyAll) — the secure posture
/// isolation.md mandates; a sandbox reaches the network only by explicit opt-in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EgressPolicy {
    /// No egress at all — the default-deny baseline.
    #[default]
    DenyAll,
    /// Only the listed destinations are reachable; RFC1918 space stays blocked
    /// regardless of what a rule names, and everything else is dropped.
    Allow(Vec<EgressRule>),
    /// No network restriction. **Trusted-profile only** — valid for the
    /// `process` driver on single-trusted-user macOS, never for untrusted work.
    Unrestricted,
}

impl EgressPolicy {
    /// Whether this policy blocks all egress.
    #[must_use]
    pub fn denies_all(&self) -> bool {
        matches!(self, Self::DenyAll)
    }

    /// Whether the workload may reach the network at all under this policy.
    #[must_use]
    pub fn permits_any(&self) -> bool {
        match self {
            Self::DenyAll => false,
            Self::Allow(rules) => !rules.is_empty(),
            Self::Unrestricted => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_denies_all() {
        assert_eq!(EgressPolicy::default(), EgressPolicy::DenyAll);
        assert!(EgressPolicy::default().denies_all());
        assert!(!EgressPolicy::default().permits_any());
    }

    #[test]
    fn allowlist_permits_only_when_non_empty() {
        let empty = EgressPolicy::Allow(Vec::new());
        assert!(!empty.permits_any());
        let cache = EgressPolicy::Allow(vec![EgressRule::any_port("cache.loom.internal")]);
        assert!(cache.permits_any());
        assert!(!cache.denies_all());
    }

    #[test]
    fn unrestricted_permits_any() {
        assert!(EgressPolicy::Unrestricted.permits_any());
        assert!(!EgressPolicy::Unrestricted.denies_all());
    }

    #[test]
    fn rules_capture_ports() {
        assert!(EgressRule::any_port("host").ports.is_empty());
        assert_eq!(EgressRule::on_ports("host", vec![443]).ports, vec![443]);
    }
}
