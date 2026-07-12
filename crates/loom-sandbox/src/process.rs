// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`ProcessDriver`] — a real macOS/unix host-process [`SandboxDriver`].
//!
//! This is the **first real driver** ([ADR-0015](../../../docs/adr/0015-pluggable-compute-backends.md),
//! [compute-backends.md §1](../../../docs/platform/compute-backends.md)): on the
//! founder's M3 Max it is the only path to Metal, so MLX (which computes through
//! Metal on unified memory) can only run as a native host process. It provides
//! **no isolation by design** — no namespace, no seccomp, no cgroup wall — which
//! is acceptable *only* because standalone macOS is a single-trusted-user profile
//! ([profiles.md](../../../docs/architecture/profiles.md)); untrusted work stays
//! Linux-only behind the hardened `runc` driver (07c/07d).
//!
//! What it *does* deliver honestly:
//! - **cwd/env scoping** — the child starts from a cleared environment plus only
//!   the spec's entries, in the spec's working directory (defaulting to the
//!   sandbox scratch).
//! - **wall-clock enforcement** — the one [`ResourceLimits`](crate::limits::ResourceLimits)
//!   knob that needs no cgroup; an overrun is force-terminated as
//!   [`Termination::TimedOut`](crate::driver::Termination::TimedOut).
//! - **graceful eject** — `SIGTERM` with a grace window (the checkpoint window),
//!   escalating to `SIGKILL`.
//! - **kill-tree teardown** — the child runs in its own process group, so
//!   teardown signals the whole group, reaps, removes scratch, and verifies no
//!   residue remains.
//!
//! The cgroup knobs (`memory`/`cpu`/`pids`) and the egress policy are *recorded*
//! but not enforced here — the trusted profile is what makes that acceptable.
//! This whole module is `cfg(unix)`; on other platforms there is simply no
//! `ProcessDriver` (the skip-cleanly discipline, at the type level).

use core::time::Duration;
use std::collections::HashMap;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::driver::{
    DriverKind, Outcome, Phase, SandboxDriver, SandboxError, SandboxHandle, SandboxId,
    TeardownReport, Termination,
};
use crate::spec::SandboxSpec;

/// A stdout/stderr capture buffer shared with a draining task.
type SharedBuf = Arc<Mutex<Vec<u8>>>;

/// The mutable, per-sandbox execution state, guarded by its own async mutex so
/// one sandbox's long `watch` never blocks operations on another.
#[derive(Debug)]
struct RunState {
    phase: Phase,
    child: Option<Child>,
    pgid: Option<i32>,
    wall_time: Option<Duration>,
    stdout_buf: SharedBuf,
    stderr_buf: SharedBuf,
    readers: Vec<JoinHandle<()>>,
    outcome: Option<Outcome>,
}

/// A driver's record of one sandbox.
#[derive(Debug)]
struct ProcSandbox {
    spec: SandboxSpec,
    scratch: PathBuf,
    run: Arc<AsyncMutex<RunState>>,
}

/// A [`SandboxDriver`] that runs each workload as a real host process.
#[derive(Debug)]
pub struct ProcessDriver {
    sandboxes: Mutex<HashMap<SandboxId, ProcSandbox>>,
    scratch_root: PathBuf,
}

impl Default for ProcessDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessDriver {
    /// A driver whose per-sandbox scratch lives under the system temp dir.
    #[must_use]
    pub fn new() -> Self {
        Self::with_scratch_root(std::env::temp_dir().join("loom-sandbox"))
    }

    /// A driver whose per-sandbox scratch lives under `root`.
    #[must_use]
    pub fn with_scratch_root(root: impl Into<PathBuf>) -> Self {
        Self {
            sandboxes: Mutex::new(HashMap::new()),
            scratch_root: root.into(),
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<SandboxId, ProcSandbox>> {
        self.sandboxes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn run_arc(&self, id: SandboxId) -> Result<Arc<AsyncMutex<RunState>>, SandboxError> {
        self.map()
            .get(&id)
            .map(|s| Arc::clone(&s.run))
            .ok_or(SandboxError::UnknownSandbox(id))
    }
}

/// Sends `sig` to the whole process group led by `pgid`.
///
/// A missing target (`ESRCH`) means the group already exited and is treated as
/// success. Non-positive group ids are refused: signalling group `0`/`-0` would
/// hit the *caller's* own group, which must never happen.
#[allow(unsafe_code)] // reviewed FFI: kill(2) is the only way to signal a process group.
fn signal_group(pgid: i32, sig: i32) -> std::io::Result<()> {
    if pgid <= 1 {
        return Ok(());
    }
    // SAFETY: `libc::kill` is a plain syscall wrapper with no memory-safety
    // preconditions; a negative pid targets the process group. Errors surface
    // via errno, which we read immediately.
    let rc = unsafe { libc::kill(-pgid, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err)
    }
}

/// Whether any process in the group led by `pgid` is still present.
#[allow(unsafe_code)] // reviewed FFI: signal 0 checks liveness without delivering a signal.
fn group_alive(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    // SAFETY: signal `0` performs the permission/existence check of kill(2)
    // without delivering a signal; no memory is touched.
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// Maps a process exit status onto a [`Termination`].
fn classify(status: std::process::ExitStatus) -> Termination {
    status.code().map_or_else(
        || Termination::Signaled {
            signal: status.signal().unwrap_or(0),
        },
        |code| Termination::Exited { code },
    )
}

/// Drains `reader` fully into `buf` (best-effort capture).
async fn drain<R: AsyncRead + Unpin>(mut reader: R, buf: SharedBuf) {
    let mut tmp = Vec::new();
    let _ = reader.read_to_end(&mut tmp).await;
    buf.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend_from_slice(&tmp);
}

/// Awaits the drain tasks (so all output is captured) and snapshots the buffers.
async fn collect_output(run: &mut RunState) -> (Vec<u8>, Vec<u8>) {
    for handle in run.readers.drain(..) {
        let _ = handle.await;
    }
    let stdout = run
        .stdout_buf
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let stderr = run
        .stderr_buf
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    (stdout, stderr)
}

#[async_trait]
impl SandboxDriver for ProcessDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Process
    }

    async fn prepare(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        let handle = SandboxHandle::new(DriverKind::Process);
        let scratch = self.scratch_root.join(handle.id().to_string());
        std::fs::create_dir_all(&scratch).map_err(|e| {
            SandboxError::Prepare(format!("create scratch {}: {e}", scratch.display()))
        })?;
        let run = RunState {
            phase: Phase::Prepared,
            child: None,
            pgid: None,
            wall_time: spec.limits.wall_time,
            stdout_buf: Arc::new(Mutex::new(Vec::new())),
            stderr_buf: Arc::new(Mutex::new(Vec::new())),
            readers: Vec::new(),
            outcome: None,
        };
        self.map().insert(
            handle.id(),
            ProcSandbox {
                spec,
                scratch,
                run: Arc::new(AsyncMutex::new(run)),
            },
        );
        Ok(handle)
    }

    // `pid` and `pgid` are the process id and its process-group id — genuinely
    // related terms whose similarity is the point, not a naming slip.
    #[allow(clippy::similar_names)]
    async fn launch(&self, handle: SandboxHandle) -> Result<(), SandboxError> {
        let id = handle.id();
        let (spec, scratch, run_arc) = {
            let map = self.map();
            let sandbox = map.get(&id).ok_or(SandboxError::UnknownSandbox(id))?;
            (
                sandbox.spec.clone(),
                sandbox.scratch.clone(),
                Arc::clone(&sandbox.run),
            )
        };

        let mut run = run_arc.lock().await;
        if run.phase != Phase::Prepared {
            return Err(SandboxError::IllegalPhase {
                id,
                actual: run.phase,
                expected: Phase::Prepared,
            });
        }

        // A cleared environment plus only the spec's entries; the child runs in
        // its own process group so teardown can signal the whole tree.
        let mut command = std::process::Command::new(&spec.command);
        command.args(&spec.args);
        command.env_clear();
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        command.current_dir(spec.cwd.clone().unwrap_or(scratch));
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.process_group(0);

        let mut command: tokio::process::Command = command.into();
        command.kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| SandboxError::Launch(format!("spawn {}: {e}", spec.command)))?;

        let pid = child
            .id()
            .ok_or_else(|| SandboxError::Launch("child exited before its pid was read".into()))?;
        let pgid =
            i32::try_from(pid).map_err(|_| SandboxError::Launch("pid out of i32 range".into()))?;

        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(tokio::spawn(drain(out, Arc::clone(&run.stdout_buf))));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(tokio::spawn(drain(err, Arc::clone(&run.stderr_buf))));
        }

        run.child = Some(child);
        run.pgid = Some(pgid);
        run.readers = readers;
        run.phase = Phase::Running;
        Ok(())
    }

    async fn watch(&self, handle: SandboxHandle) -> Result<Outcome, SandboxError> {
        let id = handle.id();
        let run_arc = self.run_arc(id)?;
        let mut run = run_arc.lock().await;
        match run.phase {
            Phase::Terminated => {
                return Ok(run
                    .outcome
                    .clone()
                    .unwrap_or_else(|| Outcome::new(Termination::Exited { code: 0 })));
            }
            Phase::Running => {}
            other => {
                return Err(SandboxError::IllegalPhase {
                    id,
                    actual: other,
                    expected: Phase::Running,
                });
            }
        }

        let wall_time = run.wall_time;
        let pgid = run.pgid.unwrap_or_default();
        let mut child = run.child.take().ok_or(SandboxError::UnknownSandbox(id))?;

        let termination = match wall_time {
            Some(budget) => match tokio::time::timeout(budget, child.wait()).await {
                Ok(status) => classify(status?),
                Err(_elapsed) => {
                    signal_group(pgid, libc::SIGKILL)?;
                    let _ = child.wait().await;
                    Termination::TimedOut
                }
            },
            None => classify(child.wait().await?),
        };

        let (stdout, stderr) = collect_output(&mut run).await;
        let outcome = Outcome::new(termination)
            .with_stdout(stdout)
            .with_stderr(stderr);
        run.phase = Phase::Terminated;
        run.outcome = Some(outcome.clone());
        Ok(outcome)
    }

    async fn eject(&self, handle: SandboxHandle, grace: Duration) -> Result<Outcome, SandboxError> {
        let id = handle.id();
        let run_arc = self.run_arc(id)?;
        let mut run = run_arc.lock().await;
        match run.phase {
            Phase::Terminated => {
                return Ok(run
                    .outcome
                    .clone()
                    .unwrap_or_else(|| Outcome::new(Termination::Ejected)));
            }
            Phase::Running => {}
            other => {
                return Err(SandboxError::IllegalPhase {
                    id,
                    actual: other,
                    expected: Phase::Running,
                });
            }
        }

        let pgid = run.pgid.unwrap_or_default();
        let mut child = run.child.take().ok_or(SandboxError::UnknownSandbox(id))?;

        // Graceful stop first (the checkpoint window), then escalate.
        signal_group(pgid, libc::SIGTERM)?;
        if let Err(_elapsed) = tokio::time::timeout(grace, child.wait()).await {
            signal_group(pgid, libc::SIGKILL)?;
            let _ = child.wait().await;
        }

        let (stdout, stderr) = collect_output(&mut run).await;
        let outcome = Outcome::new(Termination::Ejected)
            .with_stdout(stdout)
            .with_stderr(stderr);
        run.phase = Phase::Terminated;
        run.outcome = Some(outcome.clone());
        Ok(outcome)
    }

    async fn teardown(&self, handle: SandboxHandle) -> Result<TeardownReport, SandboxError> {
        let id = handle.id();
        let sandbox = self
            .map()
            .remove(&id)
            .ok_or(SandboxError::UnknownSandbox(id))?;
        let mut run = sandbox.run.lock().await;

        let pgid = run.pgid;
        let was_running = run.phase == Phase::Running;
        if let Some(pg) = pgid.filter(|_| was_running) {
            let _ = signal_group(pg, libc::SIGKILL);
        }
        // Reap our direct child either way, so it is never left a zombie.
        if let Some(mut child) = run.child.take() {
            let _ = child.wait().await;
        }
        for handle in run.readers.drain(..) {
            handle.abort();
        }
        run.phase = Phase::TornDown;

        let _ = std::fs::remove_dir_all(&sandbox.scratch);
        let scratch_removed = !sandbox.scratch.exists();
        let residue_alive = pgid.is_some_and(group_alive);
        let processes_killed = u32::from(was_running);

        Ok(TeardownReport {
            scratch_removed,
            processes_killed,
            verified_clean: scratch_removed && !residue_alive,
        })
    }
}
