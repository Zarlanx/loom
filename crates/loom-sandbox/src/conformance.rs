// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The shared lifecycle conformance suite every [`SandboxDriver`] must pass.
//!
//! This is the Mirror-lesson discipline made concrete (workspace-setup.md §6): a
//! fake is only trustworthy if the real implementation is exercised against the
//! *same* seam. The [`FakeDriver`](crate::fake::FakeDriver) proves these cases in
//! CI-without-root; the real [`ProcessDriver`](crate::process::ProcessDriver)
//! proves the identical cases against real host processes. If the fake ever
//! diverges from the real driver, one of them fails this suite.
//!
//! Every case is written against a `&dyn SandboxDriver`, which also keeps the
//! trait provably object-safe. The canonical commands are addressed absolutely
//! (`/bin/echo`, `/bin/sleep`, `/bin/false`) so no `PATH` is assumed.

#![allow(clippy::missing_panics_doc)] // the suite *is* assertions; panics are the report.

use core::time::Duration;

use crate::driver::{Phase, SandboxDriver, SandboxError, Termination};
use crate::limits::ResourceLimits;
use crate::spec::SandboxSpec;

/// A short grace/budget used throughout the suite; long enough for a real
/// process to react, short enough to keep the suite fast.
const SHORT: Duration = Duration::from_millis(250);

/// Runs the entire conformance suite against `driver`, panicking on the first
/// violated invariant.
pub async fn run_all(driver: &dyn SandboxDriver) {
    echo_runs_to_success(driver).await;
    nonzero_exit_is_reported(driver).await;
    rewatch_replays_the_outcome(driver).await;
    watch_before_launch_is_illegal(driver).await;
    eject_stops_a_running_workload(driver).await;
    wall_time_budget_times_out(driver).await;
    teardown_kills_and_verifies_clean(driver).await;
    torn_down_handle_is_unknown(driver).await;
}

/// `echo hi` runs to a clean success with its output captured, then tears down clean.
pub async fn echo_runs_to_success(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/echo")
        .arg("hi")
        .label("conformance-echo");
    let handle = driver.prepare(spec).await.expect("prepare echo");
    driver.launch(handle).await.expect("launch echo");

    let outcome = driver.watch(handle).await.expect("watch echo");
    assert_eq!(
        outcome.termination,
        Termination::Exited { code: 0 },
        "echo should exit 0"
    );
    assert!(outcome.is_success(), "echo should be a success");
    assert!(
        outcome.stdout_lossy().contains("hi"),
        "echo stdout should contain 'hi', got {:?}",
        outcome.stdout_lossy()
    );

    let report = driver.teardown(handle).await.expect("teardown echo");
    assert!(report.scratch_removed, "teardown should remove scratch");
    assert!(report.is_clean(), "teardown should verify clean");
}

/// A failing command's non-zero exit is reported faithfully.
pub async fn nonzero_exit_is_reported(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/false");
    let handle = driver.prepare(spec).await.expect("prepare false");
    driver.launch(handle).await.expect("launch false");

    let outcome = driver.watch(handle).await.expect("watch false");
    assert_eq!(
        outcome.termination,
        Termination::Exited { code: 1 },
        "false should exit 1"
    );
    assert!(!outcome.is_success(), "false should not be a success");

    driver.teardown(handle).await.expect("teardown false");
}

/// Watching an already-terminated sandbox replays its recorded outcome.
pub async fn rewatch_replays_the_outcome(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/echo").arg("again");
    let handle = driver.prepare(spec).await.expect("prepare echo");
    driver.launch(handle).await.expect("launch echo");

    let first = driver.watch(handle).await.expect("first watch");
    let second = driver.watch(handle).await.expect("second watch");
    assert_eq!(
        first.termination, second.termination,
        "re-watch should replay the termination"
    );
    assert_eq!(
        first.stdout, second.stdout,
        "re-watch should replay the captured stdout"
    );

    driver.teardown(handle).await.expect("teardown echo");
}

/// Watching before launching is rejected as an illegal phase transition.
pub async fn watch_before_launch_is_illegal(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/echo").arg("premature");
    let handle = driver.prepare(spec).await.expect("prepare echo");

    let err = driver
        .watch(handle)
        .await
        .expect_err("watching a prepared-but-unlaunched sandbox must fail");
    assert!(
        matches!(
            err,
            SandboxError::IllegalPhase {
                expected: Phase::Running,
                ..
            }
        ),
        "expected IllegalPhase(expected: running), got {err:?}"
    );

    driver.teardown(handle).await.expect("teardown echo");
}

/// Owner-eject stops a running workload within its grace window and reports `Ejected`.
pub async fn eject_stops_a_running_workload(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/sleep").arg("30");
    let handle = driver.prepare(spec).await.expect("prepare sleep");
    driver.launch(handle).await.expect("launch sleep");

    let outcome = driver.eject(handle, SHORT).await.expect("eject sleep");
    assert_eq!(
        outcome.termination,
        Termination::Ejected,
        "an ejected workload reports Ejected"
    );

    let report = driver.teardown(handle).await.expect("teardown sleep");
    assert!(report.is_clean(), "teardown after eject should be clean");
}

/// A workload that overruns its wall-clock budget is force-terminated as `TimedOut`.
pub async fn wall_time_budget_times_out(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/sleep")
        .arg("30")
        .limits(ResourceLimits::unbounded().with_wall_time(SHORT));
    let handle = driver.prepare(spec).await.expect("prepare sleep");
    driver.launch(handle).await.expect("launch sleep");

    let outcome = driver.watch(handle).await.expect("watch sleep");
    assert_eq!(
        outcome.termination,
        Termination::TimedOut,
        "a workload past its wall_time is TimedOut"
    );

    let report = driver.teardown(handle).await.expect("teardown sleep");
    assert!(report.is_clean(), "teardown after timeout should be clean");
}

/// Tearing down a still-running sandbox kills its process and verifies clean.
pub async fn teardown_kills_and_verifies_clean(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/sleep").arg("30");
    let handle = driver.prepare(spec).await.expect("prepare sleep");
    driver.launch(handle).await.expect("launch sleep");

    let report = driver
        .teardown(handle)
        .await
        .expect("teardown running sleep");
    assert!(
        report.processes_killed >= 1,
        "teardown should have killed the running workload, killed={}",
        report.processes_killed
    );
    assert!(report.scratch_removed, "teardown should remove scratch");
    assert!(report.is_clean(), "teardown should verify clean");
}

/// After teardown the handle is invalid: every further operation is `UnknownSandbox`.
pub async fn torn_down_handle_is_unknown(driver: &dyn SandboxDriver) {
    let spec = SandboxSpec::new("/bin/echo").arg("bye");
    let handle = driver.prepare(spec).await.expect("prepare echo");
    driver.launch(handle).await.expect("launch echo");
    driver.watch(handle).await.expect("watch echo");
    driver.teardown(handle).await.expect("teardown echo");

    let watch_err = driver
        .watch(handle)
        .await
        .expect_err("watching a torn-down sandbox must fail");
    assert!(
        matches!(watch_err, SandboxError::UnknownSandbox(_)),
        "expected UnknownSandbox, got {watch_err:?}"
    );

    let teardown_err = driver
        .teardown(handle)
        .await
        .expect_err("double teardown must fail");
    assert!(
        matches!(teardown_err, SandboxError::UnknownSandbox(_)),
        "expected UnknownSandbox, got {teardown_err:?}"
    );
}
