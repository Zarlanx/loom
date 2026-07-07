# 0007 — Ephemeral-everything teardown in the agent

**Status:** Accepted — 2026-07-07

## Context

Identity-stripping (ADR-0006) protects serverless inference against a host learning *whose* data it holds, but both products still leave residue on the host: residual VRAM between tenants (NVIDIA does not clear VRAM — `cudaMalloc` returns uninitialized memory, and a decade of research shows real recovery of weights/KV-caches from a prior process), scratch on disk, keys in memory, page-cache remnants. A host who idly greps their disk after a job, resells the machine, or cold-boots and picks through storage must find nothing. This is a distinct threat from an active adversary dumping live RAM/VRAM *during* a job — which no software trick on consumer hardware can stop ([`../platform/security.md`](../platform/security.md) §3.1).

## Decision

The agent runs an **ordered, fail-closed teardown** after every job, scoped explicitly to **hygiene, not anti-active-dumping** ([`../platform/host-agent.md`](../platform/host-agent.md) §6 teardown; [`../platform/security.md`](../platform/security.md) §5):

1. Kill the sandbox (container terminate or microVM shutdown).
2. **Scrub VRAM** — allocate-and-zero sweep across free/just-freed memory, plus a GPU reset (`nvidia-smi -r` / NVML device reset) where no other consumer holds the card; verify by reading back sampled pages.
3. Unmount `tmpfs` scratch (all renter scratch is RAM-backed; nothing renter-owned is written to host disk in plaintext).
4. Cgroup/namespace/netns/loop-device/VFIO cleanup.
5. Zeroize in-memory tenant keys (keys live only in guest RAM for the job's lifetime).
6. **Verify-clean checklist** reported to the control plane; a node that cannot verify marks itself **dirty**, refuses new claims, and escalates rather than silently re-renting.

Swap is disabled in the sandbox; any genuine spill goes to an encrypted ephemeral volume whose key dies with the job ([`../platform/security.md`](../platform/security.md) §5).

## Consequences

- Raises the attacker's cost from "trivial, after the fact" to "must actively instrument a running job." A host who snoops post-job, resells, or cold-boots finds nothing.
- The fail-closed dirty-node path prevents a botched scrub from ever re-letting a card with residue.

**What we give up:**

- **This is explicitly a floor, not a ceiling.** It does *not* defend against the active adversary of §3.1 who dumps live RAM/VRAM during the job — we say so in the UX and here. Only Tier C (ADR-0008) changes the guarantee class.
- Software VRAM scrubbing on consumer GPUs is **best-effort, not firmware-attested**; we cannot offer an H100-CC-grade cryptographic zero-residue guarantee on a 4090, and verify-clean checks free-VRAM/process-absence rather than reading back all of VRAM ([`../platform/host-agent.md`](../platform/host-agent.md) §11).
- On a card the owner also games on, a full GPU reset may be unavailable mid-session, leaving only the allocate-and-zero sweep — a possibly lower assurance level for shared cards.
- Teardown adds latency and a dirty-node availability hit when scrub verification fails.

## Revisit when

A per-card empirical residue study shows allocate-and-zero + reset is insufficient for the non-TEE tiers, or shared-card reset limitations force a lower advertised assurance for those configs ([`../platform/host-agent.md`](../platform/host-agent.md) §11). Renters needing a cryptographic guarantee use Tier C, not a stronger scrub.
