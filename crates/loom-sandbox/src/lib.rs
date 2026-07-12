// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-sandbox` — the [`SandboxDriver`] trait and its drivers.
//!
//! A sandbox driver is the *execution-driver* axis of Loom's compute model
//! ([compute-backends.md], [ADR-0015]): it decides **how a job is contained**,
//! never what it computes on. One driver-agnostic [`SandboxSpec`] — command,
//! args, env, cwd, [`ResourceLimits`], [`EgressPolicy`] — flows through the same
//! five-phase lifecycle (**prepare → launch → watch → eject → teardown**) under
//! any driver:
//!
//! - [`FakeDriver`] (behind the `test-support` feature) simulates the whole
//!   lifecycle in-memory, so CI exercises it **without root or a container
//!   runtime**.
//! - [`ProcessDriver`] (PR-07b) runs a real macOS host process — the trusted,
//!   isolation-free path that is the only way to reach Metal for MLX.
//! - A hardened `runc` driver (PR-07c/07d) and future container/microVM drivers
//!   add real isolation on Linux behind the same trait.
//!
//! Both a fake and a real driver are held to the *same* [`conformance`] suite
//! (behind the `conformance` feature), which is how a fake stays honest
//! (workspace-setup.md §6).
//!
//! [compute-backends.md]: ../../../docs/platform/compute-backends.md
//! [ADR-0015]: ../../../docs/adr/0015-pluggable-compute-backends.md

pub mod driver;
pub mod egress;
pub mod limits;
pub mod spec;

#[cfg(feature = "test-support")]
pub mod fake;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use driver::{
    DriverKind, Outcome, Phase, SandboxDriver, SandboxError, SandboxHandle, SandboxId,
    TeardownReport, Termination,
};
pub use egress::{EgressPolicy, EgressRule};
pub use limits::{CpuLimit, ResourceLimits};
pub use spec::SandboxSpec;

#[cfg(feature = "test-support")]
pub use fake::{FakeDriver, FakeScript};
