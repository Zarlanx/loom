// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Node capability vocabulary: compute backends, the memory model, isolation
//! tiers, and comparable versions.
//!
//! This is the scheduling-relevant slice of a node's advertised
//! `(driver, backends[], memory_model)` tuple ([compute-backends.md], ADR-0015).
//! A node advertises which [`Backend`]s it can execute and how much memory it
//! exposes ([`MemoryModel`]); a job filters against them.

use core::fmt;

/// A compute backend — *which stack the workload computes on*.
///
/// The closed enum from ADR-0015. Discriminants encode the `auto`-resolution
/// priority order MLX → CUDA → CPU → `ROCm` (lower is higher priority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Backend {
    /// Apple silicon, Metal via unified memory (priority 1).
    Mlx = 0,
    /// NVIDIA CUDA (priority 2).
    Cuda = 1,
    /// The always-available CPU baseline (priority 3).
    Cpu = 2,
    /// AMD `ROCm` — enum slot and capability plumbing only; runtime deferred (priority 4).
    Rocm = 3,
}

impl Backend {
    /// All backends in `auto`-resolution priority order.
    pub const ALL: [Self; 4] = [Self::Mlx, Self::Cuda, Self::Cpu, Self::Rocm];

    /// The priority rank used by `auto` resolution (lower wins).
    #[must_use]
    pub const fn priority(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Mlx => "mlx",
            Self::Cuda => "cuda",
            Self::Cpu => "cpu",
            Self::Rocm => "rocm",
        };
        f.write_str(name)
    }
}

/// A small `Copy` set of [`Backend`]s, backed by a bitset (four possible members).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BackendSet(u8);

// The bitset stores one flag per backend in a `u8`; a ninth backend variant would
// overflow the shift in `with`/`contains`. Enforce the assumption at compile time.
const _: () = assert!(Backend::ALL.len() <= u8::BITS as usize);

impl BackendSet {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// Builds a set from a slice of backends.
    #[must_use]
    pub const fn from_backends(backends: &[Backend]) -> Self {
        let mut set = Self::EMPTY;
        let mut i = 0;
        while i < backends.len() {
            set = set.with(backends[i]);
            i += 1;
        }
        set
    }

    /// Returns the set with `backend` added.
    #[must_use]
    pub const fn with(self, backend: Backend) -> Self {
        Self(self.0 | (1u8 << backend as u8))
    }

    /// Whether `backend` is a member.
    #[must_use]
    pub const fn contains(self, backend: Backend) -> bool {
        self.0 & (1u8 << backend as u8) != 0
    }

    /// The set intersection.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Whether the set has no members.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterates the members in priority order.
    pub fn iter(self) -> impl Iterator<Item = Backend> {
        Backend::ALL.into_iter().filter(move |&b| self.contains(b))
    }

    /// The highest-priority member, if any (the `auto` pick).
    #[must_use]
    pub fn best_by_priority(self) -> Option<Backend> {
        Backend::ALL.into_iter().find(|&b| self.contains(b))
    }
}

impl fmt::Debug for BackendSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl FromIterator<Backend> for BackendSet {
    fn from_iter<I: IntoIterator<Item = Backend>>(iter: I) -> Self {
        iter.into_iter().fold(Self::EMPTY, Self::with)
    }
}

/// How a node's memory is organized — load-bearing for placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MemoryKind {
    /// Apple silicon: one pool shared by CPU and GPU ("VRAM" == "RAM").
    #[default]
    Unified,
    /// NVIDIA/AMD: a fixed VRAM size distinct from host RAM.
    Discrete,
}

/// A node's advertised memory: its organization plus a size in megabytes.
///
/// For a fit check, [`size_mb`](Self::size_mb) is what matters — a unified pool
/// counts the whole pool, a discrete card counts its VRAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MemoryModel {
    /// Whether the pool is unified or discrete.
    pub kind: MemoryKind,
    /// The usable size in megabytes.
    pub size_mb: u64,
}

impl MemoryModel {
    /// Constructs a memory model.
    #[must_use]
    pub const fn new(kind: MemoryKind, size_mb: u64) -> Self {
        Self { kind, size_mb }
    }

    /// The usable size in megabytes.
    #[must_use]
    pub const fn size_mb(self) -> u64 {
        self.size_mb
    }
}

/// Sandbox isolation strength. Ordered weakest → strongest (isolation.md §2):
/// Tier B (daily-driver container) < Tier A (dedicated-rig microVM) < Tier C
/// (confidential, future). A node satisfies a job when its tier strength is at
/// least the job's minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum IsolationTier {
    /// Daily-driver hardened container — the baseline.
    #[default]
    B,
    /// Dedicated-rig microVM with GPU passthrough — stronger.
    A,
    /// Confidential microVM (future) — strongest.
    C,
}

impl IsolationTier {
    /// Whether this tier is strong enough to satisfy `minimum`.
    ///
    /// Strength is the enum's declaration order (`B < A < C`), matching the derived
    /// [`Ord`]. Comparing the discriminants directly keeps that ordering the single
    /// source of truth — there is no separate strength table to fall out of sync.
    #[must_use]
    pub const fn satisfies(self, minimum: Self) -> bool {
        (self as u8) >= (minimum as u8)
    }
}

/// A comparable `major.minor.patch` version (driver / CUDA floors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Version {
    /// Major component (compared first).
    pub major: u32,
    /// Minor component.
    pub minor: u32,
    /// Patch component.
    pub patch: u32,
}

impl Version {
    /// Constructs a version.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[test]
    fn backend_priority_order_is_mlx_cuda_cpu_rocm() {
        assert!(Backend::Mlx.priority() < Backend::Cuda.priority());
        assert!(Backend::Cuda.priority() < Backend::Cpu.priority());
        assert!(Backend::Cpu.priority() < Backend::Rocm.priority());
        assert_eq!(Backend::ALL.len(), 4);
    }

    #[test]
    fn backend_set_membership_and_intersection() {
        let node = BackendSet::from_backends(&[Backend::Mlx, Backend::Cpu]);
        assert!(node.contains(Backend::Mlx));
        assert!(node.contains(Backend::Cpu));
        assert!(!node.contains(Backend::Cuda));
        assert!(!node.is_empty());
        assert!(BackendSet::EMPTY.is_empty());

        let recipe = BackendSet::from_backends(&[Backend::Cuda, Backend::Cpu]);
        let common = node.intersect(recipe);
        assert!(common.contains(Backend::Cpu));
        assert!(!common.contains(Backend::Mlx));
    }

    #[test]
    fn best_by_priority_picks_highest_priority_member() {
        let set = BackendSet::from_backends(&[Backend::Cpu, Backend::Cuda]);
        assert_eq!(set.best_by_priority(), Some(Backend::Cuda));
        assert_eq!(BackendSet::EMPTY.best_by_priority(), None);
    }

    #[test]
    fn backend_set_iterates_in_priority_order() {
        let set = BackendSet::from_backends(&[Backend::Rocm, Backend::Mlx, Backend::Cpu]);
        let collected: Vec<Backend> = set.iter().collect();
        assert_eq!(collected, vec![Backend::Mlx, Backend::Cpu, Backend::Rocm]);
    }

    #[test]
    fn isolation_tiers_order_weakest_to_strongest() {
        assert!(IsolationTier::B < IsolationTier::A);
        assert!(IsolationTier::A < IsolationTier::C);
        assert!(IsolationTier::A.satisfies(IsolationTier::B));
        assert!(!IsolationTier::B.satisfies(IsolationTier::A));
        assert!(IsolationTier::C.satisfies(IsolationTier::C));
    }

    #[test]
    fn versions_compare_component_wise() {
        assert!(Version::new(1, 2, 3) < Version::new(1, 3, 0));
        assert!(Version::new(2, 0, 0) > Version::new(1, 9, 9));
        assert_eq!(Version::new(525, 60, 13).to_string(), "525.60.13");
    }

    #[test]
    fn memory_model_reports_size() {
        let unified = MemoryModel::new(MemoryKind::Unified, 48_000);
        assert_eq!(unified.size_mb(), 48_000);
        assert_eq!(unified.kind, MemoryKind::Unified);
    }
}
