// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The resource-limits surface a workload declares.
//!
//! These are the knobs [isolation.md §3.2](../../../docs/platform/isolation.md)
//! names — memory, CPU, PID count, wall-clock — expressed once, driver-agnostic.
//! A driver enforces what its mechanism allows and is honest about the rest: a
//! hardened `runc` driver maps them onto **cgroup v2** (`memory.max`, CPU quota,
//! `pids.max`); the [`ProcessDriver`](crate::process::ProcessDriver) provides
//! **no isolation** (trusted profile) and enforces only [`wall_time`](ResourceLimits::wall_time),
//! recording the cgroup knobs without enforcing them. VRAM is capped separately
//! by the GPU runtime, not here.

use core::time::Duration;

/// A hard CPU cap, expressed in millicores (`1000` == one full core).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CpuLimit {
    /// The cap in millicores; `2000` allows two fully-saturated cores.
    pub millicores: u32,
}

impl CpuLimit {
    /// Builds a CPU limit from a millicore count.
    #[must_use]
    pub const fn from_millicores(millicores: u32) -> Self {
        Self { millicores }
    }

    /// Builds a CPU limit from a whole-core count.
    #[must_use]
    pub const fn from_cores(cores: u32) -> Self {
        Self {
            millicores: cores.saturating_mul(1000),
        }
    }
}

/// The resource ceiling a sandbox declares. Every field is optional; `None`
/// means "unbounded" (the driver applies no cap for that dimension).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceLimits {
    /// Maximum resident memory in bytes (cgroup `memory.max`).
    pub memory_bytes: Option<u64>,
    /// Maximum CPU (cgroup CPU quota).
    pub cpu: Option<CpuLimit>,
    /// Maximum number of live processes/threads (cgroup `pids.max`; defangs fork bombs).
    pub pids_max: Option<u32>,
    /// Maximum wall-clock runtime before the workload is force-terminated.
    ///
    /// The one limit the isolation-free [`ProcessDriver`](crate::process::ProcessDriver)
    /// enforces, because it needs no cgroup.
    pub wall_time: Option<Duration>,
}

impl ResourceLimits {
    /// An unbounded limit set (all dimensions `None`).
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            memory_bytes: None,
            cpu: None,
            pids_max: None,
            wall_time: None,
        }
    }

    /// Sets the memory ceiling in bytes.
    #[must_use]
    pub const fn with_memory_bytes(mut self, bytes: u64) -> Self {
        self.memory_bytes = Some(bytes);
        self
    }

    /// Sets the CPU ceiling.
    #[must_use]
    pub const fn with_cpu(mut self, cpu: CpuLimit) -> Self {
        self.cpu = Some(cpu);
        self
    }

    /// Sets the maximum live-process count.
    #[must_use]
    pub const fn with_pids_max(mut self, pids: u32) -> Self {
        self.pids_max = Some(pids);
        self
    }

    /// Sets the wall-clock runtime budget.
    #[must_use]
    pub const fn with_wall_time(mut self, wall_time: Duration) -> Self {
        self.wall_time = Some(wall_time);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unbounded() {
        assert_eq!(ResourceLimits::default(), ResourceLimits::unbounded());
        let limits = ResourceLimits::default();
        assert!(limits.memory_bytes.is_none());
        assert!(limits.wall_time.is_none());
    }

    #[test]
    fn builders_set_each_dimension() {
        let limits = ResourceLimits::unbounded()
            .with_memory_bytes(1 << 30)
            .with_cpu(CpuLimit::from_cores(2))
            .with_pids_max(128)
            .with_wall_time(Duration::from_secs(60));
        assert_eq!(limits.memory_bytes, Some(1 << 30));
        assert_eq!(limits.cpu, Some(CpuLimit::from_millicores(2000)));
        assert_eq!(limits.pids_max, Some(128));
        assert_eq!(limits.wall_time, Some(Duration::from_secs(60)));
    }

    #[test]
    fn cpu_cores_convert_to_millicores() {
        assert_eq!(CpuLimit::from_cores(4).millicores, 4000);
        assert_eq!(CpuLimit::from_millicores(500).millicores, 500);
    }
}
