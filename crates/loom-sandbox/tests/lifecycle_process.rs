// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! PR-07b gate: the real `ProcessDriver` runs `echo hi` end-to-end on macOS with
//! clean teardown, and passes the *same* [`loom_sandbox::conformance`] suite the
//! `FakeDriver` passes. The whole file is `cfg(unix)` — on any other platform
//! there is no `ProcessDriver`, so the suite skips cleanly at compile time.

#![cfg(all(unix, feature = "conformance"))]

use core::time::Duration;

use loom_sandbox::{
    ProcessDriver, ResourceLimits, SandboxDriver, SandboxSpec, Termination, conformance,
};

/// A per-test scratch/work directory outside any sandbox scratch, cleaned up on drop.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        // Unique without a wall-clock read (SystemTime::now is disallowed
        // repo-wide): process id plus a monotonic counter.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "loom-sandbox-test-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn process_driver_passes_the_lifecycle_conformance_suite() {
    let driver = ProcessDriver::new();
    conformance::run_all(&driver).await;
}

/// The headline PR-07b proof: a real `echo hi` process runs to success with its
/// output captured, then teardown verifies the host is left clean.
#[tokio::test]
async fn echo_hi_runs_end_to_end_with_clean_teardown() {
    let driver = ProcessDriver::new();
    let handle = driver
        .prepare(SandboxSpec::new("/bin/echo").arg("hi"))
        .await
        .expect("prepare");
    driver.launch(handle).await.expect("launch");

    let outcome = driver.watch(handle).await.expect("watch");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    assert_eq!(outcome.stdout_lossy(), "hi\n");
    assert!(outcome.stderr.is_empty(), "echo should write no stderr");

    let report = driver.teardown(handle).await.expect("teardown");
    assert!(report.scratch_removed, "scratch must be gone");
    assert!(report.is_clean(), "teardown must verify clean");
}

/// Env and cwd are plumbed into the child: it writes `$LOOM_VAR` to a file
/// resolved relative to its working directory.
#[tokio::test]
async fn env_and_cwd_are_plumbed_into_the_workload() {
    let work = TempDir::new("cwd");
    let driver = ProcessDriver::new();

    let spec = SandboxSpec::new("/bin/sh")
        .arg("-c")
        .arg("printf '%s' \"$LOOM_VAR\" > out.txt")
        .env("LOOM_VAR", "hello-from-env")
        .cwd(&work.path);
    let handle = driver.prepare(spec).await.expect("prepare");
    driver.launch(handle).await.expect("launch");
    let outcome = driver.watch(handle).await.expect("watch");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });

    // The relative `out.txt` landed in the cwd we set (proves cwd), and its
    // contents are the env var we injected (proves env).
    let written = std::fs::read_to_string(work.path.join("out.txt")).expect("read out.txt");
    assert_eq!(written, "hello-from-env");

    driver.teardown(handle).await.expect("teardown");
}

/// A workload past its wall-clock budget is force-terminated as `TimedOut`.
#[tokio::test]
async fn wall_time_budget_force_terminates() {
    let driver = ProcessDriver::new();
    let spec = SandboxSpec::new("/bin/sleep")
        .arg("30")
        .limits(ResourceLimits::unbounded().with_wall_time(Duration::from_millis(250)));
    let handle = driver.prepare(spec).await.expect("prepare");
    driver.launch(handle).await.expect("launch");

    let started = std::time::Instant::now();
    let outcome = driver.watch(handle).await.expect("watch");
    assert_eq!(outcome.termination, Termination::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "watch should return at the budget, not after the full sleep"
    );

    let report = driver.teardown(handle).await.expect("teardown");
    assert!(report.is_clean());
}

/// Teardown kills the whole process *tree*: a backgrounded grandchild in the
/// sandbox's process group is killed before it can touch its marker file.
#[tokio::test]
async fn teardown_kills_the_process_tree() {
    let work = TempDir::new("tree");
    let marker = work.path.join("grandchild-touched");
    let driver = ProcessDriver::new();

    // The shell backgrounds a grandchild that would touch the marker after 1s,
    // then sleeps. Both the shell and the grandchild share the sandbox's
    // process group, so a group kill must reach the grandchild too.
    let script = format!("(sleep 1 && touch '{}') & sleep 30", marker.display());
    let spec = SandboxSpec::new("/bin/sh").arg("-c").arg(script);
    let handle = driver.prepare(spec).await.expect("prepare");
    driver.launch(handle).await.expect("launch");

    // Tear down immediately, well before the grandchild's 1s timer.
    let report = driver.teardown(handle).await.expect("teardown");
    assert!(
        report.processes_killed >= 1,
        "teardown should have killed a running workload"
    );

    // Give the grandchild's timer time to fire had it survived.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !marker.exists(),
        "the grandchild survived teardown — the process tree was not killed"
    );
}

/// Ejecting a running workload stops it within grace and reports `Ejected`; the
/// sandbox then tears down clean.
#[tokio::test]
async fn eject_stops_a_real_process() {
    let driver = ProcessDriver::new();
    let handle = driver
        .prepare(SandboxSpec::new("/bin/sleep").arg("30"))
        .await
        .expect("prepare");
    driver.launch(handle).await.expect("launch");

    let started = std::time::Instant::now();
    let outcome = driver
        .eject(handle, Duration::from_millis(250))
        .await
        .expect("eject");
    assert_eq!(outcome.termination, Termination::Ejected);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "eject should stop the process promptly, not wait out the sleep"
    );

    let report = driver.teardown(handle).await.expect("teardown");
    assert!(report.is_clean());
}
