# Multi-backend compute design

**Status:** Design (July 2026) · owner: platform
**Scope:** The authoritative design for Loom's pluggable compute backends — how the core stays backend-agnostic while executing real ML work on Apple silicon (MLX), NVIDIA (CUDA), CPU, and, later, AMD (ROCm). This document expands the settled decision in [ADR-0015](../adr/0015-pluggable-compute-backends.md); it does **not** relitigate it. It owns the two-axes model, the capability/scheduling contract, lazy runtime materialization (including the macOS venv-bundle format), the per-backend runtime matrix, and how recipes and serving become backend-polymorphic. It leans on [backend.md](./backend.md) for crate layout (`loom-sandbox`, `loom-hostd`, the scheduler), [profiles.md](../architecture/profiles.md) for the trust model, and [environments.md](../ml-lifecycle/environments.md), which **remains authoritative for the CUDA backend's image catalog**.

The through-line from ADR-0015: the platform's purpose is *openness against gatekept lab stacks* — run ML on whatever stack you own — under the hard lightweight constraint ([ADR-0013](../adr/0013-single-binary-self-host-control-plane.md)) that nothing loads until it is used. "Support everything with their own stack, but not everything loads at the same time."

---

## 1. Two orthogonal axes

The single most important idea in this document — and the one that keeps the design from collapsing into a vendor tangle — is that **execution driver** and **compute backend** are independent axes, never conflated.

- **Execution driver** — *how a job is contained.* This is [`loom-sandbox`](./backend.md)'s `SandboxDriver` trait: `process` (a plain host process, macOS), `runc`/gVisor (`runsc`) containers (Linux), and Cloud-Hypervisor microVMs (Tier-A Linux, later). The driver decides isolation, not computation.
- **Compute backend** — *which stack the workload computes on.* The closed enum from ADR-0015: **`mlx` | `cuda` | `cpu` | `rocm`**. The backend decides computation, not isolation.

A node advertises a `(driver, backends[], memory_model)` tuple. The founder's M3 Max is `(process, [mlx, cpu], unified-48GB)`; a 4090 box is `(runc, [cuda, cpu], vram-24GB)`. These compose — but not every cell of the product is physically real. The valid combinations:

| Driver → / Backend ↓ | `process` (macOS) | `runc` / gVisor (Linux) | microVM (Tier-A Linux) |
|---|---|---|---|
| **mlx** | ✅ **v1** — macOS host process, Metal via unified memory | ❌ impossible (see below) | ❌ impossible |
| **cuda** | ⚙️ not used (no CUDA on macOS) | ✅ **v1** — the [environments.md](../ml-lifecycle/environments.md) catalog | ✅ later — driver homogeneity via guest driver |
| **cpu** | ✅ v1 — dev/test, llama.cpp, torch-cpu | ✅ v1 — always-available baseline | ✅ later |
| **rocm** | ❌ n/a | ⚙️ **later** — enum + capability plumbing now, runtime deferred | ✅ later |

Legend: ✅ built, ⚙️ plumbed-but-deferred / not-a-target, ❌ physically impossible.

**Why `(container, mlx)` is impossible — not deferred, impossible.** There are no macOS containers: Linux containers require the Linux kernel, and macOS does not run one natively. Nor can a Linux VM on the Mac reach the GPU — **Metal is invisible to Linux guests**; Apple exposes no GPU passthrough for the M-series to a virtualized Linux kernel. So MLX, which computes through Metal on unified memory, can only run in a **native macOS process**. This is not a roadmap gap we might close; it is a property of the platform. MLX therefore *always* pairs with the `process` driver.

**Why `(process, *)` is trusted-profile-only.** The `process` driver provides **no isolation** — no namespace, no seccomp, no cgroup wall — so a malicious workload running as a host process on macOS can read the operator's files and RAM. Per [profiles.md](../architecture/profiles.md), this is acceptable **only** because standalone macOS is a **single-trusted-user** profile: the operator runs their own code on their own machine, so the "malicious workload vs host" adversary (Direction 1) is not in play. Untrusted work — the marketplace, strangers' code — stays **Linux-only**, where `runc` hardening and gVisor give a real sandbox. macOS is a dogfooding-and-trusted-self-host backend, never an untrusted-supply one.

The payoff of splitting the axes: the scheduler reasons about `(driver, backend)` capability metadata without `loomd` ever linking a vendor library or knowing what Metal is. Neither axis leaks into the other.

---

## 2. The capability model

### What a node advertises

A node's capability record — assembled by `loom-hostd` at enrollment and refreshed on change — carries, in addition to the GPU inventory already specified in [agent-protocol.md](./agent-protocol.md):

- **`driver`** — the `SandboxDriver` this node runs (`process` | `runc` | `gvisor` | `microvm`).
- **`backends[]`** — the compute backends this node can actually execute, e.g. `[mlx, cpu]` or `[cuda, cpu]`. A backend appears only if its runtime can be materialized here (§3) — a Linux box with no NVIDIA card never advertises `cuda`.
- **`memory_model`** — `unified` (Apple silicon: one pool shared by CPU and GPU, so "VRAM" and "RAM" are the same 48GB) or `discrete` (NVIDIA/AMD: a fixed VRAM size distinct from host RAM), plus the **size** in MB. This distinction is load-bearing for placement: a 32B QLoRA job that needs ~20GB "GPU memory" fits an M3 Max's 48GB unified pool but not a 24GB discrete card, and the scheduler must know the pool is unified to reason about it.
- **Per-backend versions** — a small map so a job can require a floor:
  - `mlx`: the **MLX/mlx-lm version** and the **macOS + Metal** version (the OS gates which MLX builds and Metal features are available).
  - `cuda`: the **host NVIDIA driver version and the CUDA line** it can serve, exactly as [environments.md §3](../ml-lifecycle/environments.md) already defines (driver floor per image line; `525.60.13` is the CUDA-12.x family floor). This document adds nothing to that mechanism; it names it as the `cuda` backend's version slot.
  - `rocm`: the ROCm version and `gfx` target (`gfx1100` etc.), plumbed now, populated when the runtime lands (§4).
- **Perf fingerprint slot** — a **measured** throughput fingerprint per backend (tok/s or samples/s on a fixed micro-benchmark), not an advertised spec. This is the same "measured, not advertised" discipline the serving replica table uses ([serving.md §3](../ml-lifecycle/serving.md)) and the training estimator wants ([training.md §7](../ml-lifecycle/training.md)). In the trusted self-host profiles the fingerprint is informational; the marketplace's benchmark-fraud defense ([profiles.md](../architecture/profiles.md)) reuses this slot but is out of scope here.

### How the scheduler matches

A job spec gains one field: **`backend: auto | mlx | cuda | cpu | rocm`**, plus an optional **minimum memory** requirement (which the recipe's `resource_claim` already derives — [recipes.md §2](../ml-lifecycle/recipes.md)).

- An **explicit** backend (`backend: cuda`) filters to nodes advertising that backend and meeting the memory/version floor. If none exist, the job is rejected at submit with a clear reason, never queued forever.
- **`auto`** resolves by intersecting the **recipe's supported-backend list** (§5) with **node capability**, then picking by the ADR-0015 **priority order MLX → CUDA → CPU** (ROCm last). For each candidate node: compute `recipe.backends ∩ node.backends`, drop nodes that can't meet `min_memory` (respecting `memory_model` — unified pools count the whole pool), and among survivors prefer the highest-priority backend present, then the scheduler's normal score (load, fingerprint, [backend.md §3](./backend.md) filter→score→commit).

So on the founder's single M3 Max, `auto` resolves to `mlx`; add a 4090 box and a second concurrent job resolves to `cuda` while MLX stays busy; strip both and a tiny test resolves to `cpu`. The renter writes `backend: auto` and the fleet's composition decides.

### Wire / protocol impact — additive only

[agent-protocol.md](./agent-protocol.md) evolves **additively only** ([build/README.md](../build/README.md) "contracts frozen first, additive thereafter"); this design does not rewrite it. It adds fields:

- To the **enrollment inventory** (`EnrollRequest.HardwareInventory` / `AgentConfig`): `driver`, `backends[]`, `memory_model {kind, size_mb}`, and the per-backend version map. These sit alongside the existing `HardwareInventory` (GPU model, VRAM, driver/CUDA) — the `cuda` version data is already there; MLX/ROCm slots are new optional fields.
- To the **heartbeat** (`Heartbeat`): the per-backend perf fingerprint and current per-backend readiness (which runtimes are warm in cache, §3), so the scheduler's warm-set and the gateway's replica table can prefer already-materialized backends.

Because every field is optional and additive, an old agent that omits them is simply read as "CUDA/Linux, discrete memory" by an older control plane, and a new field never breaks the golden vectors ([backend.md §9](./backend.md)). The exact protobuf field numbers are an agent-protocol edit, not restated here.

---

## 3. Lazy runtime materialization

The lightweight guarantee (ADR-0013, ADR-0015 §3): **nothing about an unused backend consumes memory, disk, or startup time.** `loomd` links **no vendor libraries ever** — not CUDA, not Metal, not ROCm. A backend's *runtime* is fetched and installed by the **agent** on first use, verified, warmed, and GC-able like any cached artifact. An idle node pays for zero backends.

A backend runtime takes one of two forms:

- **(Linux) a curated OCI image**, exactly per [environments.md](../ml-lifecycle/environments.md) — unchanged. `base-cuda` → `train` → `serve-vllm`, Nydus-distributed, digest-pinned, driver-floor-gated. This document adds nothing to that mechanism; the CUDA (and later ROCm) backend *is* the environments.md catalog.
- **(macOS) a pinned `uv`-managed venv bundle** — the new sibling mechanism, because there is no container on macOS to hold the runtime.

### The venv-bundle format

MLX runs as a native macOS process, so its "runtime" is a **lockfile-pinned Python environment**, not an image. A venv bundle is a **content-addressed** artifact described by a small manifest:

```yaml
# mlx-runtime bundle manifest
bundle: loom/mlx-runtime
version: 2026.07
python: "3.12.8"                 # the exact interpreter, uv-managed (not the host's)
platform_tag: macosx-14.0-arm64  # OS + arch floor; gates against the node's macOS/Metal version
uv_lock_hash: sha256:4c1f…       # hash of the committed uv.lock — THE contract, like an image digest
packages_digest: sha256:9a02…    # content hash of the resolved wheel set
entry_points:                    # what the agent may launch out of this bundle
  train:  mlx_lm.lora
  serve:  mlx_lm.server
  convert: mlx_lm.convert
```

The `uv_lock_hash` plays exactly the role an image `sha256:` digest plays for OCI (environments.md §2.2): it is the reproducibility contract, not a floating version. A recipe pins a bundle by its lock hash the same way it pins an image by digest ([recipes.md §1](../ml-lifecycle/recipes.md)), and `loom env freeze` (environments.md §8) captures it for a Mac job.

### Lifecycle: fetch → verify → warm → GC

1. **Fetch** — on first placement of an MLX job, the agent pulls the bundle (the wheel set + the pinned interpreter) into its local cache over the same content-addressed distribution path used for weights and layers ([networking.md](./networking.md)). It is bytes in the cache, addressed by hash — no different from an image layer at the transport level.
2. **Verify** — the agent checks `packages_digest` and, like signed images (environments.md §6), the bundle is **signed** and signature-verified before anything executes. A curated bundle is only as trustworthy as its provenance; the bar matches the OCI catalog's.
3. **Warm** — `uv` materializes the venv from the locked wheels (fast: no resolution, install-from-lock only). The bundle is now ready; the node's heartbeat advertises `mlx` as warm (§2). The first warm pays install cost once; subsequent jobs reuse the cached venv.
4. **GC** — bundles are LRU-evicted from the agent cache exactly like image layers ([backend.md §6](./backend.md), host-agent cache manager). An unused bundle is reclaimed; a re-fetch is a cache miss, not a rebuild.

### Honest weaknesses vs OCI

The venv bundle is a genuine second distribution mechanism and it is **weaker than an OCI image in two specific ways** we state plainly (ADR-0015 "we pay"):

- **No filesystem isolation.** An OCI image is a whole rootfs; a venv bundle is a Python environment layered on the host's macOS filesystem. The job sees the host FS. This is the *same* limitation as the `process` driver's lack of isolation (§1) — and acceptable for the *same* reason: macOS is single-trusted-user. The venv bundle does not pretend to sandbox; isolation on macOS is "you trust your own code," full stop.
- **Host-Python / host-OS coupling.** An OCI image carries its own userland; a venv bundle rides on the host's macOS and system libraries. A macOS or Metal-framework update on the host can, in principle, shift behavior under a pinned bundle in a way an image would absorb. **Mitigation:** the bundle pins its **own** interpreter via `uv` (not `/usr/bin/python3`), so Python-level reproducibility holds; the residual coupling is to macOS/Metal itself, which the `platform_tag` floor and the per-backend macOS/Metal version in the capability record (§2) surface rather than hide. We accept this coupling as the cost of running natively on Metal — the only way to use the GPU on the founder's hardware at all.

Two mechanisms, one discipline: **fetch-verify-warm-GC, content-addressed, signed, lock-pinned** — whether the artifact is a Nydus OCI image on Linux or a uv venv bundle on macOS. `loomd` never links either.

---

## 4. Per-backend runtime matrix

Backends are explicitly tiered (ADR-0015 §4). This section states each backend's real 2026 maturity, cited where it makes a load-bearing claim.

### MLX (priority 1 — built and verified first)

MLX is the only backend the founder can run on real hardware today, so it is built first and against real silicon.

- **Fine-tuning.** `mlx-lm` supports **LoRA, QLoRA, and DoRA** via `mlx_lm.lora --train`. QLoRA is automatic — pointing `--model` at a **quantized** base makes training use QLoRA with no extra flag; a non-quantized base gives plain LoRA.[^mlxlora] This is a direct analogue to the CUDA path's TRL+bitsandbytes, which matters for the polymorphic recipe (§5).
- **Serving.** `mlx_lm.server` exposes an **OpenAI-compatible** endpoint (`/v1/chat/completions`) on localhost, so any OpenAI-protocol client points at it unmodified.[^mlxserver] This is the MLX engine behind Loom's one gateway (§5).
- **Model conversion + quantization.** `mlx_lm.convert` downloads a Hugging Face model, converts it, and quantizes it (4-bit by default, with **mixed-precision** support — e.g. keeping sensitive embedding/projection layers at 6-bit while the body is 4-bit) in one step, optionally uploading to the `mlx-community` Hub org.[^mlxconvert] So an HF model becomes an MLX-servable/trainable artifact without leaving the toolchain.
- **What 48GB unified memory realistically does.** The founder's M3 Max (48GB unified) sits in a comfortable band for the platform's bread-and-butter work. Community practice in 2026: a 32GB Mac fine-tunes **7–8B** LoRA comfortably and handles **14B** LoRA training; a 48GB machine has real headroom for **14B** and can do **32B QLoRA** (~20–25GB), which no single 24GB discrete consumer card can hold — the unified-memory advantage the training doc already notes for inference applies to training too.[^mlxfit] **70B QLoRA needs ~96GB** and is out of reach on 48GB.[^mlx70b] The honest caveat, from the same sources: **training throughput on Apple silicon is far below a discrete datacenter GPU** (community reports on the order of a few tok/s of training throughput vs tens on an H100),[^mlxperf] so MLX's value is *fits-and-runs-on-hardware-you-own and dogfoods-the-platform*, not raw speed. We surface this the same way training.md surfaces every number: a measured range, never an invented constant.

### CUDA (priority 2 — written now, verified when hardware exists)

**Unchanged.** The CUDA backend is the existing design, in full: [environments.md](../ml-lifecycle/environments.md) (image catalog, driver-floor mechanics, Nydus distribution, Tier-A rootfs), [training.md](../ml-lifecycle/training.md) (PyTorch/TRL/PEFT/bitsandbytes/FSDP2/Liger stack), and [serving.md](../ml-lifecycle/serving.md) (vLLM primary). This document does not restate any of it. CUDA is **hardware-gated**: developed in parallel behind the same trait, exercised in CI only where NVIDIA hardware is available, per the existing hardware-gated discipline ([backend.md §9](./backend.md) real-GPU smoke suite). Nothing here changes the CUDA story except reframing it as *one backend among several* rather than *the* stack.

### CPU (priority 3 — the always-available baseline)

CPU is the backend every node has, used for tests, echo jobs, tiny models, and the walking skeleton ([build/README.md](../build/README.md) M1 `echo`). Two runtimes:

- **`llama.cpp` (server mode, GGUF)** — the real CPU serving path: a mature OpenAI-compatible server over GGUF quantized models, with CPU/GPU hybrid offload, the narrow niche serving.md already scopes it to.
- **plain `torch-cpu`** — for tests and small classifier/embedding jobs that need PyTorch semantics without a GPU.

**A subtlety worth stating: `llama.cpp` also runs on Metal**, so it is a *second* Apple-silicon inference path alongside `mlx_lm.server`. Which is v1's **primary macOS serving engine**? **Decision: `mlx_lm.server` is primary; `llama.cpp`/Metal is a fallback.** MLX now **leads llama.cpp in steady-state decode** on Apple silicon for models under ~14B (reported 20–87% faster; converging above ~27B where memory bandwidth dominates), and Apple's M5 Neural Accelerators widen the gap for MLX.[^mlxvsllama] For Loom's 7–14B band, MLX is faster — and, decisively, MLX is *already* the macOS training engine (the only one with the LoRA/QLoRA story above), so making it the serving engine too keeps the `mlx` backend coherent end-to-end: train with `mlx_lm.lora`, serve with `mlx_lm.server`, same bundle, same capability advertisement. Splitting train=MLX / serve=llama.cpp would fracture the backend for no throughput gain in our size band. `llama.cpp`/Metal stays the honest fallback (a model MLX doesn't support well, or GGUF-specific CPU/GPU offload) under the `cpu` backend's llama.cpp runtime, selected explicitly, not as the macOS default.

### ROCm (priority 4 — enum slot + plumbing only)

ROCm is in the enum and the capability model from day one — a node *could* advertise `rocm` with a `gfx` target, the scheduler *could* match it, the recipe map (§5) *has* a slot for it — but the **runtime is deferred**. No ROCm venv bundle or image ships in this phase. This is deliberately consistent with the reality check in [environments.md §5](../ml-lifecycle/environments.md): AMD support in 2026 is real but gated (allowlist of qualified SKUs, per-SKU qualification, `gfx1201`/RDNA4 vLLM kernels still maturing). We plumb the capability so ROCm is a config-and-runtime addition later, not an architecture change — exactly the ADR-0015 promise that ROCm's "enum slot and capability plumbing exist from day one; its runtime implementation is deferred."

---

## 5. Recipes and serving as backend-polymorphic contracts

### Recipes

A recipe is **one renter-facing contract with per-backend implementations** (ADR-0015 §5). The recipe manifest ([recipes.md §2](../ml-lifecycle/recipes.md)) gains a **`backends:`** map — backend → implementation reference:

```yaml
recipe: qlora-sft
version: 4
backends:
  mlx:
    runtime: { bundle: loom/mlx-runtime, uv_lock_hash: sha256:4c1f… }
    impl: mlx-lm-lora                 # mlx_lm.lora --train, QLoRA via quantized base
  cuda:
    runtime: { image: loom/train, digest: sha256:d91f4c… }   # environments.md catalog
    impl: trl-peft-bnb                # TRL SFTTrainer + PEFT + bitsandbytes NF4
  # rocm: (slot reserved; runtime deferred — §4)
supported_backends: [mlx, cuda]       # what `backend: auto` intersects against (§2)
config_schema: { … }                  # unchanged, backend-independent knobs
outputs: [adapter_safetensors, model_card, eval_report, lineage_record]
```

The renter picks `qlora-sft@4` and (optionally) `backend: auto`; the platform resolves the backend per §2 and dispatches to that backend's `impl`. `qlora-sft` maps to **`mlx-lm` LoRA on `mlx`** and **TRL/bitsandbytes on `cuda`** — the same instruction-tuning contract, two implementations.

**Outputs are contract-equivalent, NOT numerically identical across backends.** Both backends emit the same *shape* — an adapter (safetensors), a model card, an eval report, and a lineage record — and the adapter is portable (§6). But an MLX LoRA run and a CUDA QLoRA run of the "same" config will **not** produce bit-identical weights: different kernels, different quantization arithmetic, different RNG. This is the same honesty [training.md §8](../ml-lifecycle/training.md) and [environments.md §9](../ml-lifecycle/environments.md) already apply *within* CUDA across GPU archs, extended across backends. Consequently:
- The **lineage record** (recipes.md §4) records the **backend and its runtime ref** (bundle lock-hash or image digest) as part of the reproduction key. "Reproduce this run" means "same backend, same runtime, same seed," not "any backend."
- The **eval report is tagged by backend.** A score from an MLX run and a score from a CUDA run are comparable as *contract-equivalent* results but are labeled with the backend that produced them, so a renter (or the marketplace) never silently compares across a backend boundary as if it were one.

The recipe QA discipline ([recipes.md §6](../ml-lifecycle/recipes.md)) extends naturally: the nightly canary smoke-test runs **per supported backend** — a `qlora-sft` canary on MLX (on real Apple silicon, which the founder has) and on CUDA (hardware-gated). A recipe that OOMs, diverges, or breaks its output contract on any *supported* backend does not publish for that backend.

### Serving

**One gateway, per-backend engines** ([serving.md §2–3](../ml-lifecycle/serving.md), unchanged in shape). The gateway speaks only the OpenAI-compatible wire protocol and does not know which engine answered. The engines, by backend:

| Backend | Serving engine |
|---|---|
| `mlx` | `mlx_lm.server` (§4, macOS primary) |
| `cuda` | vLLM (serving.md primary), SGLang/TensorRT-LLM per serving.md |
| `cpu` | `llama.cpp` server (GGUF) |
| `rocm` | vLLM-ROCm (deferred, §4) |

- The engine that serves a deployment is **recorded in the deployment metadata**, alongside the model/adapter refs — so observability, the replica table, and lineage all know it was, e.g., `mlx_lm.server@2026.07` vs `vllm@…`.
- **Failover is same-backend only.** The gateway's failover spec ([serving.md §3](../ml-lifecycle/serving.md)) re-dispatches a dropped stream to another warm replica **of the same backend**: an MLX replica fails over to another MLX replica, a vLLM replica to another vLLM replica. The seeded-deterministic-continuation optimization is *already* gated on "same engine build" in serving.md, and different backends fail that trivially; even restart-from-scratch across backends raises unresolved questions (which node's tokenizer, which quant). **Cross-backend failover is an open question (§8), not a v1 promise.** In practice, in the trusted profiles the fleet is usually homogeneous per model anyway (one Mac, or one set of NVIDIA boxes), so same-backend failover is the common and correct case.

---

## 6. Checkpoint / resume across backends

Checkpoint/resume ([training.md §3](../ml-lifecycle/training.md), `loom-ckpt`) is the platform's differentiator, and it interacts with backends as *documented behavior, not magic*:

- **Adapters (safetensors) are portable.** A LoRA adapter is a set of tensors in a backend-neutral format; an adapter trained on MLX can be loaded by a CUDA engine and vice versa (modulo the same-model, same-rank compatibility any adapter needs). This is why the recipe output contract (§5) can promise a portable adapter.
- **Optimizer state is generally NOT portable across backends.** The Adam moments, LR-scheduler state, and RNG state `loom-ckpt` captures for exact-step resume are laid out and computed per-backend (MLX's optimizer vs PyTorch's are different objects with different numerics). There is no sound way to hand a half-trained MLX optimizer state to a CUDA trainer and continue as if nothing happened.
- **Therefore: resume happens on the SAME backend by default.** On owner-eject ([host-agent.md](./host-agent.md)), checkpoint-and-requeue prefers a **same-backend** node so full optimizer state restores and the run continues at the exact step with RNG intact — the seamless behavior training.md promises.
- **Cross-backend resume is an adapter-only restart.** If the only free node is a different backend (the Mac is reclaimed, only a CUDA box is free), resume degrades honestly: the portable **adapter** is restored but the optimizer state is **not** — the run restarts the optimizer on the new backend from the adapter weights, not from the exact step. This is a real, surfaced trajectory discontinuity — the multi-backend analogue of training.md's "a requeue can cross a hardware boundary" caveat, here across a *backend* boundary.

The scheduler encodes this as a placement preference: **same-backend requeue first; cross-backend requeue only as an explicit, flagged degradation.** For the founder's single-Mac reality this is moot (everything is MLX); it matters the moment a mixed fleet exists.

---

## 7. What this changes in the existing docs

A pointer section, not a rewrite. The multi-backend framing **reframes** several docs as *CUDA-backend-specific* without invalidating them (ADR-0015 says so):

- **[environments.md](../ml-lifecycle/environments.md)** — remains **authoritative for the CUDA backend's image catalog** (and later ROCm images), untouched; it gains the **venv-bundle sibling** (§3) as the macOS/MLX runtime-distribution mechanism.
- **[training.md](../ml-lifecycle/training.md)** / **[serving.md](../ml-lifecycle/serving.md)** — read as the **CUDA backend's** stack and engine specifics (PyTorch/TRL/FSDP2/Liger; vLLM/SGLang/TensorRT-LLM); MLX (§4) is the parallel backend spec. Their VRAM tables and determinism caveats generalize via the unified-vs-discrete memory model (§2) and the cross-backend numeric-equivalence honesty (§5).
- **[recipes.md](../ml-lifecycle/recipes.md)** — the manifest gains the `backends:` map (§5); flagship `qlora-sft` becomes the two-implementation contract above. Immutability, estimator, lineage, and QA compose unchanged.
- **[build/README.md §7](../build/README.md#7-backend-first-revision-2026-07-08-mlx-on-the-founders-metal)** — the Phase-1 plan is **reworked** to sequence MLX-first (the only backend on real hardware) with CUDA behind the hardware-gated seam: the `backends[]` capability plumbing and venv-bundle fetch land in PR-16, the `ProcessDriver` in PR-07b, the polymorphic recipe legs in PR-17/18/19 — see the revised [pr-breakdown.md](../build/pr-breakdown.md).

---

## 8. Open questions

- **Cross-backend failover (serving).** Same-backend failover is v1 (§5). Is there ever a sound cross-backend continuation — even restart-from-scratch — given tokenizer/quant/engine differences, and how would the gateway journal it? Pairs with serving.md §9's deterministic-continuation question, now across a backend boundary.
- **MLX distributed.** `mx.distributed` exists for distributed inference and fine-tuning across Apple-silicon machines.[^mlxdist] It is **out of scope** for this phase (single-node, per [training.md](../ml-lifecycle/training.md)'s single-node scoping and ADR-0015's priority), but it is worth watching: a multi-Mac LAN fleet is a plausible future the way a multi-GPU NVIDIA rig is. Not a commitment.
- **Windows / DirectML — explicitly out.** No Windows compute backend. Windows hosts, if ever supported, run Linux containers (WSL2/Hyper-V) and land on the `cuda`/`cpu` backends via the Linux driver path, never a native DirectML backend.
- **TPU / Gaudi — out per ADR-0015.** Backends outside the `mlx | cuda | cpu | rocm` enum (Google TPU, Intel Gaudi) are explicitly not in scope; adding one is a "revisit when" trigger on ADR-0015, not an extension point we build toward now.
- **Cross-backend eval comparability.** Eval reports are backend-tagged (§5), but *how comparable* is an MLX-produced score to a CUDA-produced score on the same suite, given numeric divergence? Do we need a per-backend eval calibration note, or is contract-equivalence enough for the trusted profiles?

---

*Related: [ADR-0015](../adr/0015-pluggable-compute-backends.md) (the settled decision this expands) · [backend.md](./backend.md) (`loom-sandbox`/`SandboxDriver`, `loom-hostd`, scheduler) · [agent-protocol.md](./agent-protocol.md) (additive inventory/heartbeat fields) · [../architecture/profiles.md](../architecture/profiles.md) (trust model, `process`-driver acceptability) · [../ml-lifecycle/environments.md](../ml-lifecycle/environments.md) (CUDA image catalog + venv-bundle sibling) · [../ml-lifecycle/training.md](../ml-lifecycle/training.md) · [../ml-lifecycle/serving.md](../ml-lifecycle/serving.md) · [../ml-lifecycle/recipes.md](../ml-lifecycle/recipes.md) · [../build/README.md](../build/README.md) (multi-backend build sequencing, §7).*

[^mlxlora]: `mlx-lm` supports LoRA, QLoRA, and DoRA fine-tuning via `mlx_lm.lora --train`; QLoRA is selected automatically when `--model` points at a quantized model (no extra flag), otherwise plain LoRA. [mlx-lm LORA.md](https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/LORA.md); [Run and Fine-Tune LLMs on Mac with MLX-LM 2026 (Markaicode)](https://markaicode.com/run-fine-tune-llms-mac-mlx-lm/).

[^mlxserver]: `mlx_lm.server` runs an OpenAI-compatible HTTP server exposing `/v1/chat/completions` on localhost, usable as a drop-in local API by any OpenAI-protocol client. [mlx-lm README](https://github.com/ml-explore/mlx-lm); [Run and Fine-Tune LLMs on Mac with MLX-LM 2026 (Markaicode)](https://markaicode.com/run-fine-tune-llms-mac-mlx-lm/).

[^mlxconvert]: `mlx_lm.convert` downloads a Hugging Face model, converts precision, and quantizes (4-bit default, with mixed-precision support such as 6-bit embeddings/projection over a 4-bit body) in one step, optionally uploading to the `mlx-community` Hub org. [MLX at Hugging Face](https://huggingface.co/docs/hub/en/mlx); [Model Conversion & Quantization (DeepWiki)](https://deepwiki.com/ml-explore/mlx-lm/2.2-model-conversion-and-quantization).

[^mlxfit]: Community practice (2026): a 32GB Mac fine-tunes 7–8B LoRA comfortably and handles 14B LoRA; a 48GB machine has headroom for 14B and can run 32B QLoRA (~20–25GB), which exceeds a single 24GB discrete consumer card. [Fine-Tuning on Mac: LoRA & QLoRA with MLX (InsiderLLM)](https://insiderllm.com/guides/fine-tuning-mac-lora-mlx/); [MLX Apple Silicon AI Dev Stack (buildmvpfast)](https://www.buildmvpfast.com/blog/mlx-apple-silicon-ai-development-mac-fine-tune-llm-2026). *(Flag: exact per-model VRAM figures are community-reported planning numbers, not Loom-measured; a Loom benchmark-harness pass per training.md §7 must replace them before they are quoted as measured.)*

[^mlx70b]: 70B QLoRA is reported as requiring ~96GB unified memory (Mac Studio Ultra class), out of reach on a 48GB machine. [Run and Fine-Tune LLMs on Mac with MLX-LM 2026 (Markaicode)](https://markaicode.com/run-fine-tune-llms-mac-mlx-lm/). *(Flag: community-reported; not independently verified.)*

[^mlxperf]: Training throughput on Apple silicon is far below discrete datacenter GPUs — community reports on the order of a few tok/s of training throughput vs tens on an H100. Treated as order-of-magnitude context, not a Loom-published number. [Fine-Tuning on Mac: LoRA & QLoRA with MLX (InsiderLLM)](https://insiderllm.com/guides/fine-tuning-mac-lora-mlx/). *(Flag: community-reported figures, wide variance; Loom must measure per training.md §7 before publishing.)*

[^mlxvsllama]: On Apple silicon MLX leads llama.cpp in steady-state decode for models under ~14B (reported ~20–87% faster), converging above ~27B where memory bandwidth dominates; Apple's M5 Neural Accelerators further favor MLX (reported ~4x TTFT advantage on M5 for a 14B-4bit model vs M4). [MLX vs llama.cpp on Apple Silicon (yage.ai)](https://yage.ai/share/mlx-apple-silicon-en-20260331.html); [llama.cpp vs MLX vs Ollama vs vLLM (Contra Collective)](https://contracollective.com/blog/llama-cpp-vs-mlx-ollama-vllm-apple-silicon-2026). *(Flag: third-party benchmarks; specific percentages vary by model/context and are not Loom-measured.)*

[^mlxdist]: `mlx-lm` includes distributed inference and fine-tuning via `mx.distributed`. [mlx-lm README](https://github.com/ml-explore/mlx-lm). Noted as out-of-scope-but-watch, not a v1 feature.
