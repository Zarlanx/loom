// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`SandboxSpec`] — the driver-agnostic description of *what to run and how to
//! contain it*.
//!
//! A spec names the command, its arguments, its environment and working
//! directory, and the isolation knobs ([`ResourceLimits`], [`EgressPolicy`]).
//! It says nothing about the compute backend or the driver: the same spec runs
//! under a [`FakeDriver`](crate::fake::FakeDriver), a
//! [`ProcessDriver`](crate::process::ProcessDriver), or a future container
//! driver unchanged.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::egress::EgressPolicy;
use crate::limits::ResourceLimits;

/// What to run inside a sandbox and how to contain it.
///
/// Build one with [`SandboxSpec::new`] and the chaining setters:
///
/// ```
/// use loom_sandbox::SandboxSpec;
///
/// let spec = SandboxSpec::new("/bin/echo")
///     .arg("hi")
///     .env("LOOM_JOB", "demo");
/// assert_eq!(spec.command, "/bin/echo");
/// assert_eq!(spec.args, ["hi"]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    /// The program to execute (an absolute path avoids `PATH` ambiguity).
    pub command: String,
    /// The program's arguments, in order.
    pub args: Vec<String>,
    /// The environment handed to the workload (sorted for determinism). A driver
    /// that scopes the environment starts from *only* these entries.
    pub env: BTreeMap<String, String>,
    /// The working directory; `None` lets the driver default it (e.g. to the
    /// sandbox scratch).
    pub cwd: Option<PathBuf>,
    /// The resource ceiling.
    pub limits: ResourceLimits,
    /// The network-egress policy.
    pub egress: EgressPolicy,
    /// An optional human-readable tag for logs and telemetry.
    pub label: Option<String>,
}

impl SandboxSpec {
    /// Starts a spec for `command` with no args, an empty environment, unbounded
    /// limits, and the default-deny egress policy.
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            limits: ResourceLimits::unbounded(),
            egress: EgressPolicy::DenyAll,
            label: None,
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets one environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Sets the working directory.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Sets the resource limits.
    #[must_use]
    pub fn limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets the egress policy.
    #[must_use]
    pub fn egress(mut self, egress: EgressPolicy) -> Self {
        self.egress = egress;
        self
    }

    /// Sets a human-readable label.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The command's final path component (the basename), used for logs and by
    /// the [`FakeDriver`](crate::fake::FakeDriver) to recognise canonical
    /// commands regardless of their directory.
    #[must_use]
    pub fn program_name(&self) -> &str {
        self.command
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&self.command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::CpuLimit;

    #[test]
    fn new_has_secure_defaults() {
        let spec = SandboxSpec::new("/bin/echo");
        assert!(spec.args.is_empty());
        assert!(spec.env.is_empty());
        assert!(spec.cwd.is_none());
        assert_eq!(spec.egress, EgressPolicy::DenyAll);
        assert_eq!(spec.limits, ResourceLimits::unbounded());
    }

    #[test]
    fn builders_compose() {
        let spec = SandboxSpec::new("/bin/echo")
            .arg("hi")
            .args(["a", "b"])
            .env("K", "V")
            .cwd("/tmp")
            .label("demo")
            .limits(ResourceLimits::unbounded().with_cpu(CpuLimit::from_cores(1)));
        assert_eq!(spec.args, ["hi", "a", "b"]);
        assert_eq!(spec.env.get("K"), Some(&"V".to_string()));
        assert_eq!(spec.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(spec.label.as_deref(), Some("demo"));
        assert_eq!(spec.limits.cpu, Some(CpuLimit::from_cores(1)));
    }

    #[test]
    fn program_name_is_the_basename() {
        assert_eq!(SandboxSpec::new("/bin/echo").program_name(), "echo");
        assert_eq!(SandboxSpec::new("echo").program_name(), "echo");
        assert_eq!(SandboxSpec::new("/usr/bin/sleep").program_name(), "sleep");
    }
}
