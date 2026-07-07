# 0003 — Tiered isolation instead of one mechanism for all hosts

**Status:** Accepted — 2026-07-07

## Context

Loom invites arbitrary, potentially malicious renter code onto machines it does not own. The isolation boundary is the whole product's insurance policy: an escape onto a host's home LAN, a bricked display, or a residential IP turned into a spam cannon drives hosts away and the marketplace dies. But supply is not uniform. A daily-driver gaming PC cannot surrender its only GPU (that blanks the owner's screen) and has no spare card; a dedicated or headless rig can hand over a whole GPU cleanly. A single isolation mechanism cannot fit both: microVMs need a VFIO-passable headless/secondary GPU that most daily drivers lack, and plain containers leave the host kernel and raw GPU driver ioctl surface exposed.

## Decision

Offer **two shipping tiers**, and advertise a card only in a tier the agent has proven the host can support ([`../platform/isolation.md`](../platform/isolation.md) §2):

- **Tier B (daily-driver):** container via `nvidia-container-toolkit`, default-hardened with **gVisor `runsc` + nvproxy** — a userspace kernel plus a vetted ioctl subset in front of the host driver. Plain `runc` is a disclosed, weaker fallback only. Applied unconditionally on top: `no-new-privileges`, user namespaces, seccomp, dropped capabilities, read-only rootfs, cgroup v2 limits.
- **Tier A (dedicated rig):** Cloud Hypervisor microVM + VFIO passthrough (ADR-0002), a hardware VM boundary with a unique guest kernel per tenant.

A future **Tier C** (confidential, ADR-0008) reuses the Tier A VMM path. Egress controls (default-deny, no RFC1918, byte caps) apply identically to both tiers ([`../platform/isolation.md`](../platform/isolation.md) §5). Tier is labeled honestly to renters ([`../platform/security.md`](../platform/security.md) §3.4).

## Consequences

- We can accept the huge daily-driver supply pool at all, at an honestly-labeled medium isolation strength, while offering strong isolation where the hardware permits.
- The scheduler filters on tier (node tier ≥ job's minimum), so renters who need strength get it and hosts who can only offer Tier B still earn ([`../platform/control-plane.md`](../platform/control-plane.md) §4).

**What we give up:**

- **Two isolation stacks to build, test, and harden**, plus a per-card tier-decision probe — more surface than a single mechanism.
- Tier B consciously accepts a **shared host kernel**: a bug reachable through a permitted nvproxy ioctl still reaches the host driver, and a renter workload can crash the host's GPU driver/display ([`../platform/isolation.md`](../platform/isolation.md) §8). We compensate with strict driver currency, curated images (ADR-0010), and tight seccomp/caps.
- gVisor/nvproxy has coverage gaps on consumer cards (RTX 30/40-series "likely work, not officially supported"), forcing per-card qualification and occasional plain-`runc` fallback ([`../platform/isolation.md`](../platform/isolation.md) §9).

## Revisit when

Consumer silicon gains practical fractional-GPU isolation (there is no SR-IOV/vGPU today), or gVisor/nvproxy coverage shifts enough to change the Tier B default vs. fallback policy. The two-tier split itself holds as long as daily-driver and dedicated supply coexist.
