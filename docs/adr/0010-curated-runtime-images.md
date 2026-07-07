# 0010 — Curated runtime images only at v1; Nydus/EROFS lazy pull

**Status:** Accepted — 2026-07-07

## Context

Loom runs ML workloads on rented consumer GPUs across two painful axes at once: a heterogeneous fleet we don't own (RTX 30/40/50-series, driver versions from last-week to 2023, Windows and Linux) where the **host owns the kernel-mode driver**, and a fast-moving software matrix (CUDA, cuDNN, PyTorch, Triton, FlashAttention, vLLM) with monthly version interlocks. The naive answer — let renters bring any Docker image — multiplies these into an untestable, unsupportable, insecure surface, and it maximizes supply-chain and escape risk on untrusted hosts ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §1). Separately, multi-GB CUDA images make cold-start latency a first-order problem, and Tier A microVMs need a rootfs format.

## Decision

Ship **only a curated catalog of runtime images at v1 — no arbitrary Dockerfiles** ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §2; [`../platform/isolation.md`](../platform/isolation.md) §3.4; [`../platform/security.md`](../platform/security.md) §7):

- A handful of images in a `FROM`-chained layer graph (`base-cuda` → `torch` → `train`/`serve-vllm`/`eval`/…), everything pinned via `uv` lockfiles down to layer digests, each declaring a **CI-measured host driver floor** enforced at enrollment. **Placement pins by digest, not tag.**
- Images are vulnerability-scanned, ship a signed SBOM, and are cosign-signed; the agent verifies the signature/digest before running.
- The escape hatch within the walls is **pip/uv installs on top of a base image from operator mirrors only** (default-deny egress), plus `loom env freeze` for reproducibility — **not** arbitrary base images or arbitrary egress.
- **Lazy pull uses Nydus (RAFS + EROFS)**, chosen because EROFS eliminates FUSE from the read path *and* **doubles as the Tier A microVM read-only rootfs** — one format, two consumers. Distribution reuses the Dragonfly P2P content-addressed fabric already chosen for weights; no second distribution path ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §6–§7).

## Consequences

- A small, end-to-end-tested image set makes tight seccomp profiles, pinned known-good CUDA/driver combos, and a scannable supply chain tractable — a defense-in-depth multiplier for Tier B (ADR-0003).
- The Nydus/EROFS choice collapses cold-start latency and gives Tier A its rootfs for free, with P2P distribution collapsing origin traffic when a popular base goes hot.

**What we give up:**

- **Renters cannot bring arbitrary images or `apt`/`curl | bash` from the open internet** — the pip/uv-on-base hatch covers most needs but not all; power users wanting a fully custom base are unserved at v1 ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §8).
- We own a monthly-cut catalog with a 6-month support / 9-month security window — real ongoing build, test, scan, and deprecation work ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §2.2).
- Compatibility leans on **backward-compat only** (consumer GeForce cards get no forward-compat), so we refuse cards below an image's driver floor rather than paper over old drivers ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §3.3).
- Lazy pull's win shrinks for jobs that touch most of the image (big training images), so it helps cold-start latency, not total bytes.

## Revisit when

A **verified custom-image build service** (scan + sign renter Dockerfiles-on-base) clears its risk bar, possibly gated to the attested Tier C where the measurement proves what ran (ADR-0008), or ROCm allowlist expansion and per-SKU qualification change the catalog shape ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §10).
