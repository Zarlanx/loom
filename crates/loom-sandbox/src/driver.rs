// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The `SandboxDriver` trait and its lifecycle vocabulary.
//!
//! A driver contains one workload through five phases — **prepare → launch →
//! watch → eject → teardown** — and reports what happened. The trait is the
//! *execution-driver* axis of [compute-backends.md](../../../docs/platform/compute-backends.md)
//! (`process` / `runc` / `gvisor` / `microvm`): it decides **isolation**, never
//! computation. Every method takes a cheap [`SandboxHandle`]; the driver owns the
//! real state internally, keyed by [`SandboxId`], so the trait stays
//! object-safe (`Box<dyn SandboxDriver>`) for a host agent that selects a driver
//! at runtime.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use crate::spec::SandboxSpec;

/// Which execution driver contains a workload — the *isolation* axis, orthogonal
/// to the compute backend ([ADR-0015](../../../docs/adr/0015-pluggable-compute-backends.md)).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverKind {
    /// In-memory simulation for CI-without-root; runs no real process.
    Fake,
    /// A plain host child process (macOS/dev). **No isolation** — trusted profile only.
    Process,
    /// A hardened `runc` container (Linux). Lands in PR-07c/07d.
    Runc,
    /// A gVisor (`runsc`) container (Linux). Future.
    Gvisor,
    /// A Cloud-Hypervisor microVM (Tier-A Linux). Future.
    Microvm,
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Fake => "fake",
            Self::Process => "process",
            Self::Runc => "runc",
            Self::Gvisor => "gvisor",
            Self::Microvm => "microvm",
        };
        f.write_str(name)
    }
}

/// A driver-scoped, globally-unique identifier for one sandbox instance.
///
/// Ids are minted from a process-wide monotonic counter, so a handle issued by
/// one driver never collides with — nor is accepted by — another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SandboxId(u64);

impl SandboxId {
    /// Mints the next id.
    // Consumed by every driver; at this slice the only driver (`FakeDriver`) is
    // behind `test-support`, so this is dead only in a driver-less default build.
    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    pub(crate) fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// The raw numeric value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SandboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sandbox-{}", self.0)
    }
}

/// A cheap, `Copy` reference to a prepared/running sandbox.
///
/// Meaningful only to the driver that issued it (from [`SandboxDriver::prepare`]);
/// a downstream crate cannot fabricate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SandboxHandle {
    id: SandboxId,
    kind: DriverKind,
}

impl SandboxHandle {
    /// Issues a fresh handle for `kind` (drivers only).
    // Consumed by every driver; at this slice the only driver (`FakeDriver`) is
    // behind `test-support`, so this is dead only in a driver-less default build.
    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    pub(crate) fn new(kind: DriverKind) -> Self {
        Self {
            id: SandboxId::next(),
            kind,
        }
    }

    /// The sandbox's id.
    #[must_use]
    pub const fn id(self) -> SandboxId {
        self.id
    }

    /// The driver that issued this handle.
    #[must_use]
    pub const fn kind(self) -> DriverKind {
        self.kind
    }
}

/// The lifecycle phase of a sandbox, as tracked by its driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Scratch is staged; the workload has not been launched.
    Prepared,
    /// The workload is running.
    Running,
    /// The workload has reached a terminal state; scratch still exists.
    Terminated,
    /// The sandbox has been torn down; the handle is no longer valid.
    TornDown,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Prepared => "prepared",
            Self::Running => "running",
            Self::Terminated => "terminated",
            Self::TornDown => "torn-down",
        };
        f.write_str(name)
    }
}

/// How a workload's run ended.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Termination {
    /// Ran to completion with this exit code.
    Exited {
        /// The process exit code (`0` == success).
        code: i32,
    },
    /// Killed by this (unix) signal number before exiting on its own.
    Signaled {
        /// The delivering signal (e.g. `15` for `SIGTERM`).
        signal: i32,
    },
    /// Exceeded its [`wall_time`](crate::limits::ResourceLimits::wall_time) budget
    /// and was force-terminated.
    TimedOut,
    /// Stopped by an owner-eject ([`SandboxDriver::eject`]) — the owner reclaimed
    /// the machine; the workload's own exit is subsumed by the eject.
    Ejected,
}

impl Termination {
    /// Whether the workload succeeded (exited `0`).
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Exited { code: 0 })
    }
}

/// The terminal result of a run: how it ended plus its captured output.
///
/// Output is captured in full for a job of PR-07's size (`echo`); streaming log
/// return with resume tokens is a later concern (PR-13 / PR-20).
#[derive(Clone, PartialEq, Eq)]
pub struct Outcome {
    /// How the run ended.
    pub termination: Termination,
    /// Everything the workload wrote to stdout.
    pub stdout: Vec<u8>,
    /// Everything the workload wrote to stderr.
    pub stderr: Vec<u8>,
}

impl Outcome {
    /// Builds an outcome with no captured output.
    #[must_use]
    pub fn new(termination: Termination) -> Self {
        Self {
            termination,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    /// Sets the captured stdout.
    #[must_use]
    pub fn with_stdout(mut self, stdout: Vec<u8>) -> Self {
        self.stdout = stdout;
        self
    }

    /// Sets the captured stderr.
    #[must_use]
    pub fn with_stderr(mut self, stderr: Vec<u8>) -> Self {
        self.stderr = stderr;
        self
    }

    /// Whether the run succeeded (exited `0`).
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.termination.is_success()
    }

    /// stdout decoded as UTF-8, lossily — for assertions and human display.
    #[must_use]
    pub fn stdout_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }
}

// Debug is hand-written so captured output prints readably (as text, not a byte
// array) without the derive dumping raw `Vec<u8>` contents.
impl fmt::Debug for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outcome")
            .field("termination", &self.termination)
            .field("stdout", &String::from_utf8_lossy(&self.stdout))
            .field("stderr", &String::from_utf8_lossy(&self.stderr))
            .finish()
    }
}

/// What a teardown reclaimed — the ephemeral-everything verification
/// ([ADR-0007](../../../docs/adr/0007-ephemeral-everything-teardown.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeardownReport {
    /// Whether the sandbox's scratch was removed (nothing persisted to the host).
    pub scratch_removed: bool,
    /// How many still-live processes teardown had to kill.
    pub processes_killed: u32,
    /// Whether the driver verified no residue remains (no live process, scratch gone).
    pub verified_clean: bool,
}

impl TeardownReport {
    /// Whether teardown left the host clean — the contract every driver owes.
    #[must_use]
    pub const fn is_clean(self) -> bool {
        self.verified_clean
    }
}

/// A driver operation failed.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SandboxError {
    /// The handle names no sandbox this driver knows (never issued, or torn down).
    #[error("unknown sandbox {0}")]
    UnknownSandbox(SandboxId),

    /// The sandbox is in the wrong phase for this operation.
    #[error("sandbox {id} is {actual}, but this operation requires {expected}")]
    IllegalPhase {
        /// The sandbox in question.
        id: SandboxId,
        /// Its actual phase.
        actual: Phase,
        /// The phase the operation required.
        expected: Phase,
    },

    /// This driver cannot run on the current platform (skip-cleanly discipline).
    #[error("driver {0} is not supported on this platform")]
    UnsupportedPlatform(DriverKind),

    /// Staging the sandbox (scratch, env) failed.
    #[error("failed to prepare sandbox: {0}")]
    Prepare(String),

    /// Spawning the workload failed.
    #[error("failed to launch workload: {0}")]
    Launch(String),

    /// An underlying I/O operation failed.
    #[error("sandbox I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Drives one workload through the sandbox lifecycle.
///
/// The five phases are strictly ordered: [`prepare`](Self::prepare) →
/// [`launch`](Self::launch) → [`watch`](Self::watch) (or
/// [`eject`](Self::eject)) → [`teardown`](Self::teardown). A driver rejects
/// out-of-order calls with [`SandboxError::IllegalPhase`] and rejects a
/// torn-down or unknown handle with [`SandboxError::UnknownSandbox`], so the
/// state machine is enforced, not assumed.
///
/// Callers must serialize lifecycle calls **per sandbox** (a host agent drives
/// one sandbox from one task); the driver itself is `Send + Sync` and may hold
/// many sandboxes concurrently.
#[async_trait]
pub trait SandboxDriver: Send + Sync {
    /// The kind of driver this is.
    fn kind(&self) -> DriverKind;

    /// Stage a sandbox from `spec` (scratch, env). Returns its handle; the
    /// workload is not yet running.
    async fn prepare(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError>;

    /// Launch the prepared workload. Requires [`Phase::Prepared`].
    async fn launch(&self, handle: SandboxHandle) -> Result<(), SandboxError>;

    /// Wait for the workload to reach a terminal state and report its outcome.
    /// Requires [`Phase::Running`]; a second call on a terminated sandbox
    /// returns the same recorded outcome.
    async fn watch(&self, handle: SandboxHandle) -> Result<Outcome, SandboxError>;

    /// Owner-eject: signal the workload to stop within `grace` (its checkpoint
    /// window), escalating to a hard kill if it overruns. Always reports
    /// [`Termination::Ejected`]. Requires [`Phase::Running`].
    async fn eject(&self, handle: SandboxHandle, grace: Duration) -> Result<Outcome, SandboxError>;

    /// Tear the sandbox down: kill any survivors, remove scratch, verify clean.
    /// Invalidates the handle. Valid from any live phase.
    async fn teardown(&self, handle: SandboxHandle) -> Result<TeardownReport, SandboxError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_success_only_on_zero_exit() {
        assert!(Termination::Exited { code: 0 }.is_success());
        assert!(!Termination::Exited { code: 1 }.is_success());
        assert!(!Termination::Signaled { signal: 15 }.is_success());
        assert!(!Termination::TimedOut.is_success());
        assert!(!Termination::Ejected.is_success());
    }

    #[test]
    fn sandbox_ids_are_unique_and_monotonic() {
        let a = SandboxId::next();
        let b = SandboxId::next();
        assert_ne!(a, b);
        assert!(b > a);
    }

    #[test]
    fn handles_carry_their_driver_kind() {
        let handle = SandboxHandle::new(DriverKind::Fake);
        assert_eq!(handle.kind(), DriverKind::Fake);
        assert_eq!(handle.id().get(), handle.id().get());
    }

    #[test]
    fn outcome_captures_output_and_reports_success() {
        let outcome = Outcome::new(Termination::Exited { code: 0 }).with_stdout(b"hi\n".to_vec());
        assert!(outcome.is_success());
        assert_eq!(outcome.stdout_lossy(), "hi\n");
    }

    #[test]
    fn displays_are_stable() {
        assert_eq!(DriverKind::Process.to_string(), "process");
        assert_eq!(Phase::Running.to_string(), "running");
        assert!(SandboxId::next().to_string().starts_with("sandbox-"));
    }
}
