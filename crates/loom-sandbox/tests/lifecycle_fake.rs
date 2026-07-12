// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! PR-07a gate: the `FakeDriver` drives the full sandbox lifecycle and passes
//! the shared conformance suite in CI-without-root. The real `ProcessDriver`
//! (PR-07b) runs the *same* [`loom_sandbox::conformance`] suite against real
//! host processes.

// Without both features this crate exposes neither the fake nor the suite, so
// the whole test is empty and still compiles under a plain `cargo test`.
#![cfg(all(feature = "test-support", feature = "conformance"))]

use core::time::Duration;

use loom_sandbox::conformance;
use loom_sandbox::{FakeDriver, FakeScript, SandboxDriver, SandboxError, SandboxSpec};

#[tokio::test]
async fn fake_driver_passes_the_lifecycle_conformance_suite() {
    let driver = FakeDriver::new();
    conformance::run_all(&driver).await;
    // Every sandbox the suite created was torn down; the fake tracks none.
    assert_eq!(
        driver.live_count(),
        0,
        "conformance suite should leak no sandboxes"
    );
}

#[tokio::test]
async fn prepare_failure_is_surfaced() {
    let driver = FakeDriver::with_script(FakeScript::failing_prepare("disk full"));
    let err = driver
        .prepare(SandboxSpec::new("/bin/echo"))
        .await
        .expect_err("prepare should fail");
    assert!(matches!(err, SandboxError::Prepare(_)), "got {err:?}");
    assert_eq!(driver.live_count(), 0);
}

#[tokio::test]
async fn launch_failure_is_surfaced() {
    let driver = FakeDriver::with_script(FakeScript::failing_launch("exec denied"));
    let handle = driver
        .prepare(SandboxSpec::new("/bin/echo"))
        .await
        .expect("prepare should succeed");
    let err = driver.launch(handle).await.expect_err("launch should fail");
    assert!(matches!(err, SandboxError::Launch(_)), "got {err:?}");
    // The sandbox is still prepared and can be torn down cleanly.
    let report = driver.teardown(handle).await.expect("teardown");
    assert!(report.is_clean());
}

#[tokio::test]
async fn a_handle_is_rejected_by_a_different_driver_instance() {
    let owner = FakeDriver::new();
    let stranger = FakeDriver::new();
    let handle = owner
        .prepare(SandboxSpec::new("/bin/echo"))
        .await
        .expect("prepare");

    let err = stranger
        .watch(handle)
        .await
        .expect_err("a foreign handle must be unknown");
    assert!(
        matches!(err, SandboxError::UnknownSandbox(_)),
        "got {err:?}"
    );

    owner.teardown(handle).await.expect("owner teardown");
}

#[tokio::test]
async fn driver_is_object_safe_and_boxable() {
    // A host agent holds whichever driver the platform selected as a trait object.
    let driver: Box<dyn SandboxDriver> = Box::new(FakeDriver::new());
    assert_eq!(driver.kind(), loom_sandbox::DriverKind::Fake);

    let handle = driver
        .prepare(SandboxSpec::new("/bin/echo").arg("boxed"))
        .await
        .expect("prepare");
    driver.launch(handle).await.expect("launch");
    let outcome = driver
        .eject(handle, Duration::from_millis(50))
        .await
        .expect("eject");
    assert_eq!(outcome.termination, loom_sandbox::Termination::Ejected);
    driver.teardown(handle).await.expect("teardown");
}
