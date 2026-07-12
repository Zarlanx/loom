// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`FakeDriver`] — an in-memory [`SandboxDriver`] that runs no real process, so
//! the full lifecycle exercises in CI **without root and without a container
//! runtime**.
//!
//! The fake is not a throwaway mock: it is the versioned stand-in that must pass
//! the *same* [`conformance`](crate::conformance) suite the real
//! [`ProcessDriver`](crate::process::ProcessDriver) passes (workspace-setup.md
//! §6). To do that it faithfully mirrors the observable outcome of a handful of
//! canonical commands — `echo`, `true`, `false`, `sleep` — and enforces the
//! identical phase state machine. Divergence from the real driver is a
//! conformance failure, which is the whole point.

use core::time::Duration;
use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::driver::{
    DriverKind, Outcome, Phase, SandboxDriver, SandboxError, SandboxHandle, SandboxId,
    TeardownReport, Termination,
};
use crate::spec::SandboxSpec;

/// Optional failure injection, so error paths (`prepare`/`launch` failing) are
/// covered without a real driver that can be made to fail.
#[derive(Debug, Clone, Default)]
pub struct FakeScript {
    /// If set, every [`prepare`](FakeDriver::prepare) fails with this message.
    pub prepare_error: Option<String>,
    /// If set, every [`launch`](FakeDriver::launch) fails with this message.
    pub launch_error: Option<String>,
}

impl FakeScript {
    /// Makes `prepare` fail with `message`.
    #[must_use]
    pub fn failing_prepare(message: impl Into<String>) -> Self {
        Self {
            prepare_error: Some(message.into()),
            launch_error: None,
        }
    }

    /// Makes `launch` fail with `message`.
    #[must_use]
    pub fn failing_launch(message: impl Into<String>) -> Self {
        Self {
            prepare_error: None,
            launch_error: Some(message.into()),
        }
    }
}

/// The simulated natural behaviour of a command.
enum Simulation {
    /// Completes at once with this termination and stdout.
    Immediate(Termination, Vec<u8>),
    /// Runs until stopped — only [`eject`](FakeDriver::eject) or a
    /// [`wall_time`](crate::limits::ResourceLimits::wall_time) budget ends it.
    LongRunning,
}

/// Decides how a canonical command would behave, by its basename.
fn simulate(spec: &SandboxSpec) -> Simulation {
    match spec.program_name() {
        "echo" => {
            let mut out = spec.args.join(" ").into_bytes();
            out.push(b'\n');
            Simulation::Immediate(Termination::Exited { code: 0 }, out)
        }
        "false" => Simulation::Immediate(Termination::Exited { code: 1 }, Vec::new()),
        "sleep" => Simulation::LongRunning,
        // "true", ":" and anything else: a generic successful command.
        _ => Simulation::Immediate(Termination::Exited { code: 0 }, Vec::new()),
    }
}

/// Per-sandbox bookkeeping the fake keeps.
#[derive(Debug)]
struct FakeSandbox {
    spec: SandboxSpec,
    phase: Phase,
    outcome: Option<Outcome>,
}

/// An in-memory driver that simulates the sandbox lifecycle.
#[derive(Debug)]
pub struct FakeDriver {
    sandboxes: Mutex<HashMap<SandboxId, FakeSandbox>>,
    script: FakeScript,
}

impl Default for FakeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDriver {
    /// A fake with no failure injection.
    #[must_use]
    pub fn new() -> Self {
        Self::with_script(FakeScript::default())
    }

    /// A fake driven by `script` (for exercising error paths).
    #[must_use]
    pub fn with_script(script: FakeScript) -> Self {
        Self {
            sandboxes: Mutex::new(HashMap::new()),
            script,
        }
    }

    /// The number of sandboxes the fake is currently tracking (for tests).
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SandboxId, FakeSandbox>> {
        // A poisoned lock means a prior holder panicked; there is no sane
        // recovery for a test double, so surface it loudly.
        self.sandboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Looks a sandbox up and asserts it is in `expected`, mapping the misses to the
/// same errors every driver owes.
fn require_phase(
    map: &mut HashMap<SandboxId, FakeSandbox>,
    handle: SandboxHandle,
    expected: Phase,
) -> Result<(), SandboxError> {
    let id = handle.id();
    let sandbox = map.get(&id).ok_or(SandboxError::UnknownSandbox(id))?;
    if sandbox.phase == expected {
        Ok(())
    } else {
        Err(SandboxError::IllegalPhase {
            id,
            actual: sandbox.phase,
            expected,
        })
    }
}

#[async_trait]
impl SandboxDriver for FakeDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Fake
    }

    async fn prepare(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        if let Some(message) = &self.script.prepare_error {
            return Err(SandboxError::Prepare(message.clone()));
        }
        let handle = SandboxHandle::new(DriverKind::Fake);
        self.lock().insert(
            handle.id(),
            FakeSandbox {
                spec,
                phase: Phase::Prepared,
                outcome: None,
            },
        );
        Ok(handle)
    }

    async fn launch(&self, handle: SandboxHandle) -> Result<(), SandboxError> {
        if let Some(message) = &self.script.launch_error {
            return Err(SandboxError::Launch(message.clone()));
        }
        let mut map = self.lock();
        require_phase(&mut map, handle, Phase::Prepared)?;
        if let Some(sandbox) = map.get_mut(&handle.id()) {
            sandbox.phase = Phase::Running;
        }
        Ok(())
    }

    async fn watch(&self, handle: SandboxHandle) -> Result<Outcome, SandboxError> {
        let mut map = self.lock();
        let id = handle.id();
        let sandbox = map.get_mut(&id).ok_or(SandboxError::UnknownSandbox(id))?;
        match sandbox.phase {
            // A terminated sandbox replays its recorded outcome.
            Phase::Terminated => Ok(sandbox
                .outcome
                .clone()
                .unwrap_or_else(|| Outcome::new(Termination::Exited { code: 0 }))),
            Phase::Running => {
                let outcome = match simulate(&sandbox.spec) {
                    Simulation::Immediate(term, stdout) => Outcome::new(term).with_stdout(stdout),
                    Simulation::LongRunning => {
                        // A long-running command ends on its wall-clock budget; with
                        // none it never completes on its own — the conformance suite
                        // only watches such a command *with* a budget.
                        if sandbox.spec.limits.wall_time.is_some() {
                            Outcome::new(Termination::TimedOut)
                        } else {
                            Outcome::new(Termination::Exited { code: 0 })
                        }
                    }
                };
                sandbox.phase = Phase::Terminated;
                sandbox.outcome = Some(outcome.clone());
                Ok(outcome)
            }
            other => Err(SandboxError::IllegalPhase {
                id,
                actual: other,
                expected: Phase::Running,
            }),
        }
    }

    async fn eject(
        &self,
        handle: SandboxHandle,
        _grace: Duration,
    ) -> Result<Outcome, SandboxError> {
        let mut map = self.lock();
        let id = handle.id();
        let sandbox = map.get_mut(&id).ok_or(SandboxError::UnknownSandbox(id))?;
        match sandbox.phase {
            Phase::Terminated => Ok(sandbox
                .outcome
                .clone()
                .unwrap_or_else(|| Outcome::new(Termination::Ejected))),
            Phase::Running => {
                let outcome = Outcome::new(Termination::Ejected);
                sandbox.phase = Phase::Terminated;
                sandbox.outcome = Some(outcome.clone());
                Ok(outcome)
            }
            other => Err(SandboxError::IllegalPhase {
                id,
                actual: other,
                expected: Phase::Running,
            }),
        }
    }

    async fn teardown(&self, handle: SandboxHandle) -> Result<TeardownReport, SandboxError> {
        let mut map = self.lock();
        let id = handle.id();
        let sandbox = map.remove(&id).ok_or(SandboxError::UnknownSandbox(id))?;
        // A still-running sandbox had one live process that teardown kills.
        let processes_killed = u32::from(sandbox.phase == Phase::Running);
        Ok(TeardownReport {
            scratch_removed: true,
            processes_killed,
            verified_clean: true,
        })
    }
}
