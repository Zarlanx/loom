# 0002 — Cloud Hypervisor + VFIO for the microVM tier

**Status:** Accepted — 2026-07-07

## Context

The strong isolation tier (Tier A, dedicated/headless rigs) must give a renter a whole real GPU inside a real VM with its own kernel, so a GPU-driver bug or a guest kernel LPE is contained by the hardware VM boundary and the CPU IOMMU rather than reaching the host kernel. That requires **GPU/PCI passthrough** into the guest. The candidate VMMs were Firecracker, bare QEMU, and Cloud Hypervisor.

Firecracker is the obvious microVM name, but it **omits PCI passthrough by design** — it exposes only a handful of emulated virtio devices to keep its attack surface and oversubscription model small ([`../platform/isolation.md`](../platform/isolation.md) §4.2). No PCI means no GPU passthrough, which disqualifies it for a GPU marketplace regardless of its other merits. QEMU supports VFIO passthrough and is more battle-tested, but carries a large attack surface and a heavier control interface.

## Decision

Use **Cloud Hypervisor + VFIO** for the Tier A microVM. The agent unbinds the GPU from the host driver, binds it to `vfio-pci`, and passes the whole card (and its IOMMU-group siblings) into a Cloud Hypervisor microVM where the vendor driver runs inside the guest ([`../platform/isolation.md`](../platform/isolation.md) §4.1). Cloud Hypervisor is chosen for its `rust-vmm` smaller attack surface, its REST-over-UNIX-socket API that the Rust agent drives cleanly ([`../platform/host-agent.md`](../platform/host-agent.md) §5), and its VFIO support on our target NVIDIA architectures. **Firecracker is rejected and this is settled.** The same VMM path is reused for the future confidential Tier C ([`../platform/security.md`](../platform/security.md) §6, ADR-0008).

## Consequences

- The host kernel never sees renter GPU ioctls at all — the DMA/ioctl attack surface lives inside the guest, contained by the CPU IOMMU. A driver crash is contained in the guest and cannot blank the host console.
- Driver homogeneity: because we ship the guest rootfs and a pinned guest driver, the heterogeneous host-driver problem disappears on Tier A ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §3.5).

**What we give up:**

- Cloud Hypervisor's VFIO passthrough is **less battle-tested than QEMU's**; we accept this for the smaller Rust attack surface and clean agent integration, with Kata/QEMU as the documented fallback ([`../platform/isolation.md`](../platform/isolation.md) §4.2, §8).
- Tier A requires a headless/secondary GPU and a clean IOMMU group, so it is not available on single-GPU daily drivers — those fall back to Tier B (ADR-0003).
- Per-job microVM boot adds low single-digit seconds of GPU/VFIO setup latency ([`../platform/isolation.md`](../platform/isolation.md) §4.1).
- The `cloud-hypervisor-client` crate is unofficial and thin (ADR-0001).

## Revisit when

Cloud Hypervisor VFIO misbehaves on a target card or platform (e.g. IOMMU-group creation edge cases), at which point we switch that path to Kata/QEMU per the documented fallback ([`../platform/isolation.md`](../platform/isolation.md) §9). Firecracker is not revisited unless it gains PCI passthrough, which is contrary to its design.
