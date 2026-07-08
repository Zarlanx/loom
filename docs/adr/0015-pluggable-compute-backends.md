# ADR-0015 — Pluggable compute backends: mlx | cuda | cpu | rocm

**Status:** Accepted, 2026-07-08. Amends the Linux+NVIDIA-first framing of [ADR-0011](0011-single-node-scope.md)'s context and the CUDA-only assumptions threaded through [environments.md](../ml-lifecycle/environments.md), [training.md](../ml-lifecycle/training.md), and [serving.md](../ml-lifecycle/serving.md); those docs remain authoritative *for the CUDA backend specifically*.

## Context

The design docs were written CUDA-on-Linux-first with ROCm as a fast-follow. Two forces broke that framing. First, the founder's only development hardware is an M3 Max MacBook (48GB unified memory) — Apple silicon, Metal, no CUDA, no Linux containers with GPU visibility — so the platform must execute real ML work on Apple silicon or it cannot be dogfooded at all. Second, and more fundamental, the project's stated purpose is openness against gatekept lab stacks: Loom should let people run ML on *whatever stack they own*, not privilege one vendor. A single-backend platform contradicts the reason the project exists.

At the same time, the lightweight constraint (ADR-0013) forbids the obvious failure mode: a runtime that links every vendor's libraries and loads them all at startup.

## Decision

1. **The core is backend-agnostic.** Compute backends form a closed enum — **`mlx` | `cuda` | `cpu` | `rocm`** — behind a `ComputeBackend` capability model. `loomd` (scheduler, API, gateway) never links vendor libraries; it schedules on *capability metadata*.
2. **Two orthogonal axes, never conflated.** The *execution driver* (how a job is contained: `process` on macOS, `runc`/gVisor containers on Linux, microVMs on Tier-A Linux) is independent of the *compute backend* (which stack the workload computes on). A node advertises `(driver, backends[], memory_model)`: an M3 Max is `(process, [mlx, cpu], unified-48GB)`; a 4090 box is `(runc, [cuda, cpu], vram-24GB)`.
3. **Runtimes are lazily materialized.** A backend's runtime (OCI image on Linux; pinned `uv` venv bundle on macOS) is fetched/installed by the agent on first use, never preloaded. Nothing about an unused backend consumes memory, disk, or startup time on any node.
4. **Priority order: MLX → CUDA → CPU, ROCm last.** MLX is built and verified first (it is the only backend the founder can run on real hardware today). CUDA is developed in parallel behind the same trait — written now, verified when NVIDIA hardware exists, enforced by the existing hardware-gated CI discipline. CPU is the always-available baseline (tests, echo jobs, small models via llama.cpp). ROCm's enum slot and capability plumbing exist from day one; its runtime implementation is deferred.
5. **Recipes and serving are backend-polymorphic contracts.** A recipe (`qlora-sft`) is one renter-facing contract with per-backend implementations (MLX: `mlx-lm` LoRA; CUDA: TRL/bitsandbytes per [training.md](../ml-lifecycle/training.md)). Serving keeps one OpenAI-compatible gateway surface with per-backend engines (MLX: `mlx-lm` server; CUDA: vLLM; CPU: llama.cpp). The gateway does not know which engine answered.

## Consequences

- **We buy:** dogfooding on the founder's real hardware from the first milestone; a platform whose architecture matches its stated purpose (bring your own stack); Apple-silicon users as a first-class audience vLLM-style platforms ignore; the CUDA design docs survive intact as the CUDA backend's spec.
- **We pay:** an N×M test matrix (backends × recipes/engines) that must be tamed by the capability model and per-backend CI gating; per-backend recipe implementations that can drift in quality (MLX LoRA ≠ bit-identical to CUDA QLoRA — outputs are *contract*-equivalent, not numerically identical, and the docs must say so); a `ProcessDriver` on macOS that provides **no isolation** (acceptable only because standalone macOS is a single-trusted-user profile — untrusted work stays Linux-only, per [profiles.md](../architecture/profiles.md)); and venv bundles as a second runtime-distribution mechanism alongside OCI images.
- **What we give up:** the simplicity of one blessed stack; any claim that all backends are equally supported (they are explicitly tiered by the priority order).

## Revisit when

A backend outside the enum matters (TPU, Intel Gaudi); or MLX's training ecosystem stalls and Apple-silicon demand doesn't materialize; or the venv-bundle mechanism proves unmaintainable next to OCI images.
