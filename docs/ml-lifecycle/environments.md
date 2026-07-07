# Runtime Environments & Image Catalog

**Status:** Design (July 2026)
**Scope:** The curated runtime-image catalog and the compatibility story that makes it work on a heterogeneous consumer GPU fleet — what images we ship, how they're versioned and built, how a single image runs in both isolation tiers, and how we keep a fast-moving CUDA/ROCm/PyTorch matrix pinned and reproducible.
**Out of scope (see other docs):** How images are *isolated* at runtime lives in [../platform/isolation.md](../platform/isolation.md); how the agent detects hardware and drives sandboxes lives in [../platform/host-agent.md](../platform/host-agent.md); how bytes (layers, weights) physically move to nodes lives in [../platform/networking.md](../platform/networking.md); training-specific and serving-specific stack choices live in [training.md](./training.md) and [serving.md](./serving.md).

---

## 1. The compatibility problem, stated plainly

Loom runs ML workloads on rented consumer GPUs. That crosses two axes that are each individually painful and together are a combinatorial swamp:

- **A heterogeneous fleet we don't own.** Hosts bring whatever they have: RTX 30/40/50-series NVIDIA cards, a spread of driver versions from "installed last week" to "hasn't been touched since 2023," on daily-driver Windows/Linux boxes and dedicated Linux rigs alike. The **host owns the kernel-mode GPU driver**; we cannot force an upgrade, only refuse cards below a floor.
- **A fast-moving software matrix.** CUDA toolkit, cuDNN, PyTorch, Triton, FlashAttention, vLLM, Transformers — each moves monthly, each with its own version interlocks. A `torch` built against CUDA 12.6 does not run on an arbitrary driver, and a FlashAttention wheel built for one PyTorch ABI silently breaks against another.

The naive answer — "let renters bring any Docker image" — multiplies these axes into an untestable, unsupportable, insecure surface. We reject it (settled: [../platform/isolation.md](../platform/isolation.md) §3.4). **Our answer is a small curated catalog with aggressive pinning:**

1. **A handful of images**, not thousands. Every image is a known-good, end-to-end-tested combination.
2. **Everything pinned** — CUDA toolkit, every Python dependency (via `uv` lockfiles), down to layer digests.
3. **A tested driver range per image.** Each image declares the minimum host NVIDIA driver it needs, validated in CI, not guessed.
4. **An agent-side driver floor enforced at enrollment.** A card whose host driver is below the floor of the images it would run is simply not offered for those images — the check happens before any job lands (see [../platform/host-agent.md](../platform/host-agent.md) §4).

The rest of this document is the mechanics of those four commitments.

---

## 2. Image catalog v1

Images form a **layer graph**: each builds `FROM` the one above it, so the expensive CUDA base is shared and node-side caches dedupe hard (§6). Names follow `loom/<image>:<tag>` (§2.2).

| Image | Builds on | Contents | Target workload |
|---|---|---|---|
| `base-cuda` | (distro base) | CUDA user-mode toolkit + cuDNN, Python 3.12, `uv`, minimal OS userland. **No PyTorch.** | Foundation for everything; direct use only for custom CUDA/C++ kernels. |
| `torch` | `base-cuda` | PyTorch 2.x, `torch.compile` + Triton (bundled with torch), FlashAttention, xformers, NCCL. | The default compute image; anything that just needs "PyTorch on a GPU." |
| `train` | `torch` | Transformers, PEFT, TRL, Unsloth, Axolotl, `bitsandbytes`, Liger-Kernel, `datasets`, `accelerate`, DeepSpeed; recipe extras: `diffusers`, `sentence-transformers` (for the diffusion-LoRA / embeddings recipes — see [recipes.md](./recipes.md)). | Fine-tuning / training jobs. Driven by [training.md](./training.md). |
| `serve-vllm` | `torch` | vLLM + its paged-attention/kernel deps, OpenAI-compatible server. | LLM serving. Driven by [serving.md](./serving.md). |
| `serve-onnx` | `base-cuda` | ONNX Runtime with CUDA + TensorRT execution providers; TensorRT libs. | ONNX/TensorRT inference for non-PyTorch or exported models. |
| `data` | `base-cuda` | Polars, DuckDB, HF `datasets`, Ray Data, `pyarrow`. | Dataset prep / ETL. **Spark is a separate JVM image**, not folded in here. |
| `eval` | `torch` | `lm-evaluation-harness`, `lighteval`, scoring/metrics deps. | Model evaluation runs. |
| `notebook` | `torch` | JupyterLab + kernel, on top of the full `torch` stack. | Interactive sessions (reached via the relay, [../platform/networking.md](../platform/networking.md) §4). |

**ROCm mirrors.** We ship ROCm variants of `base-cuda` (→ `base-rocm`), `torch`, `train`, and `serve-vllm` **where upstream support is real** — see the reality check in §5. In 2026 PyTorch and vLLM both have genuine ROCm builds (ROCm 7.2 ships PyTorch wheels for RDNA3/RDNA4 targets, and vLLM treats ROCm as a first-class platform with Triton-based attention on RDNA3/4 [[AMD ROCm compat matrix](https://rocm.docs.amd.com/en/latest/compatibility/compatibility-matrix.html); [vLLM/ROCm first-class](https://rocm.blogs.amd.com/software-tools-optimization/vllm-omni/README.html)]). `serve-onnx` gets a ROCm variant later (ONNX Runtime MIGraphX/ROCm EP), and `eval`/`notebook`/`data` follow only once the ROCm `torch` base is stable. We do **not** claim ROCm parity on day one and we gate which AMD cards may even enroll (§5).

### 2.2 Naming, versioning, immutability

**Calendar-versioned, matrix-encoded tags.** Every mutable capability that affects compatibility is in the tag:

```
loom/train:2026.07-cu126-torch2.12       # NVIDIA, CUDA 12.6, PyTorch 2.12
loom/serve-vllm:2026.07-cu128-torch2.12  # a newer CUDA line, same month
loom/torch:2026.07-rocm7.2-torch2.12     # ROCm mirror
```

- `2026.07` — the **catalog release**, cut monthly. Groups a coherent, co-tested set of all images.
- `cu126` / `rocm7.2` — the CUDA/ROCm toolkit line (drives the **driver floor**, §3).
- `torch2.12` — the framework line, because a torch ABI bump ripples through FlashAttention, xformers, vLLM.

**Tags are immutable.** Once `loom/train:2026.07-cu126-torch2.12` is published it is never rebuilt in place. A security patch mints a new tag (`2026.07.1-…`), never mutates the old one.

**Placement pins by digest, not tag.** The control plane records the `sha256:` digest of the exact image a job ran on. Tags are for humans; **digests are the contract** (this is what reproducibility, §9, and content-addressed distribution, §6, both key on). A `loom env freeze` (§8) captures the digest.

**Deprecation policy.** Each catalog release is **supported for 6 months** and **security-patched for 9**. At 9 months a tag is frozen (no new placements) but existing pinned reruns still resolve by digest as long as any host can meet the driver floor. Renters get a deprecation notice one release before their pinned image ages out. We publish an EOL calendar so long-running training pins aren't surprised.

---

## 3. CUDA driver / toolkit compatibility mechanics

This is the section experts will check, so it is precise.

### 3.1 The split: host kernel driver vs. injected user-mode driver

An NVIDIA GPU stack has two halves:

- **Kernel-mode driver** (`nvidia.ko` and friends) — a host-owned kernel module bound to the hardware. **Loom does not ship or control this**; it's whatever the owner installed. On Tier B (containers) it is *shared* with the host.
- **User-mode driver** (`libcuda.so`, the CUDA driver API) plus the CUDA **runtime/toolkit** (`libcudart`, cuDNN, etc.).

In a Tier B container, the **nvidia-container-toolkit** is what bridges the two: at container start it mounts the host's GPU device nodes (`/dev/nvidia*`) and **injects the host's user-mode driver components** (`libcuda.so` matched to the host kernel driver) into the container, while the CUDA *toolkit/runtime* (`libcudart`, cuDNN) comes from **our image** [[NVIDIA/nvidia-container-toolkit](https://github.com/NVIDIA/nvidia-container-toolkit)]. This is the crucial design fact: **our image supplies the CUDA runtime; the host supplies the driver.** The image must therefore be built against a CUDA toolkit that the host's driver can serve.

### 3.2 Minor-version (backward) compatibility — what we rely on

Since CUDA 11, **minor-version compatibility** lets an application built against any toolkit in a major family (all of 12.x) run on a **sufficiently new driver** in that family, with some feature caveats [[CUDA Minor Version Compatibility](https://docs.nvidia.com/deploy/cuda-compatibility/minor-version-compatibility.html)]. Concretely: **the minimum driver for the entire CUDA 12.x family on Linux is `525.60.13`** [[NVIDIA dev forums](https://forums.developer.nvidia.com/t/minimum-required-driver-version-for-cuda-12-6/318333)]. A host on driver `≥ 525.60.13` can run *any* CUDA 12.x image — 12.2, 12.4, 12.6, 12.8 — because a newer driver is **backward-compatible** with older toolkits within the same major line.

This is the mechanism our whole model leans on. It means:

- We can ship a `cu126` image and it runs on a wide band of host drivers, from the family floor up to the newest.
- The **driver floor per image** is a function of its CUDA line: `cu126` and `cu128` images both sit within CUDA 12.x, so the *family* floor is `525.60.13`, but we set a **higher practical floor** per image because newer toolkits and cuDNN builds want newer drivers for full feature/perf and to avoid the tail of known-bad old-driver bugs. Each image's exact floor is measured in CI (§6) and shipped as metadata — not hand-waved.

### 3.3 Forward compatibility — why we do NOT rely on it

Forward compatibility (the `cuda-compat-<major>-<minor>` package) lets a **newer** user-mode CUDA stack run on an **older** kernel driver, even across major families — e.g. `cuda-compat-12-8` bundles R570 user-mode libs so a CUDA 12.8 app runs on a 12.2-era (R535) kernel driver [[Forward Compatibility](https://docs.nvidia.com/deploy/cuda-compatibility/forward-compatibility.html)]. Tempting for a fleet with old drivers — but there is a hard catch:

> **Forward compatibility is supported only on data-center GPUs (Tesla/Instinct-class) and NVIDIA cloud, NOT on desktop/GeForce GPUs.** Consumer cards get **backward compatibility only** [[Forward Compatibility](https://docs.nvidia.com/deploy/cuda-compatibility/forward-compatibility.html)].

Our fleet is overwhelmingly GeForce. **So forward-compat is off the table for us**, and the entire compatibility story reduces to: *build against a CUDA line, require a host driver new enough to serve it, enforce that floor at enrollment.* We do not try to paper over old host drivers with `cuda-compat`; we refuse cards below the floor. This is cleaner and it removes a whole class of "works on the Tesla in CI, fails on the renter's 3090" bugs.

### 3.4 What the agent checks at enrollment

At enrollment and on driver change, the agent reads the host driver version (via NVML) and computes, for each catalog image line, whether the host meets that image's **published driver floor**. The result is a per-card capability set: *"this 4070 on driver 550.x can run cu126 and cu128 images; this 3060 on 525.x can run cu126 but not cu128."* The control plane only schedules a job onto a card advertised as meeting that image's floor. The agent also enforces a **hard global minimum driver** below which the card isn't enrolled at all (it can't run *any* current image and can't satisfy gVisor's driver window either — [../platform/isolation.md](../platform/isolation.md) §3.3). Detection mechanics live in [../platform/host-agent.md](../platform/host-agent.md) §4.

### 3.5 How Tier A changes the picture — and why it's an advantage

Everything above is the Tier B (container, shared host driver) story. **Tier A inverts the hardest constraint.** In a Cloud Hypervisor + VFIO microVM the whole GPU is passed to the guest and **the vendor driver runs inside the guest** ([../platform/isolation.md](../platform/isolation.md) §4). That guest is **Loom-controlled**: we ship the VM rootfs (§7), so we ship a **known, pinned kernel-mode driver** inside it, per GPU generation. The heterogeneous-host-driver problem *disappears* on Tier A — the guest driver is homogeneous across our fleet regardless of what the owner installed on the host.

This is worth stating loudly: **Tier A gives us driver homogeneity for free.** The only host-side requirement is that the card is VFIO-passable (headless/secondary, clean IOMMU group — [../platform/isolation.md](../platform/isolation.md) §4.3); the host's *own* driver version becomes irrelevant to the workload. Where a card qualifies for Tier A, we prefer it partly for this reason: the compatibility matrix collapses to "our guest driver × the image," which we test exhaustively.

---

## 4. Compiler / runtime stack positioning

Curated images are also where we make opinionated compiler/runtime choices, so renters get performance without a research project.

- **Triton** ships **inside `torch`** (it's PyTorch's default backend for generated kernels). `torch.compile` emits Triton for fused elementwise/attention kernels; renters get it transparently. We don't ship Triton as a standalone image.
- **`torch.compile` modes.** We enable `torch.compile` opt-in per job. Default is the standard mode; `max-autotune` (which searches kernel configs and can emit CUDA graphs) is offered for serving where the one-time compile cost amortizes. **Caveat we state honestly:** compile artifacts are cached, but the cache is keyed on GPU arch — a compile done on a 4090 (Ada, `sm_89`) is not reused on a 3090 (Ampere, `sm_86`). On a heterogeneous fleet this means compile cost recurs per arch, not once globally (§4 build-cache note below).
- **TensorRT / TensorRT-LLM (in `serve-onnx` and optionally serving).** TensorRT is **ahead-of-time**: it builds an optimized engine for a *specific* GPU architecture, and **engines are not portable across architectures** without the (slower, less-optimized) hardware-compatibility mode [[TensorRT support matrix](https://docs.nvidia.com/deeplearning/tensorrt/latest/getting-started/support-matrix.html)]. Builds cost **10–90 minutes per (model, GPU-arch, dtype)** [[TensorRT-LLM deployment guide](https://www.spheron.network/blog/tensorrt-llm-production-deployment-guide/)]. **Honest assessment:** this fights our fleet. A per-SM-arch **engine build cache** (keyed on `model × sm_arch × dtype × TRT version`) is mandatory if we offer TRT at all — build once per arch, reuse across all cards of that arch. Given the build cost and the arch fan-out (30/40/50-series span at least three SM targets), **TensorRT-LLM is a serving-side opt-in for high-QPS pinned deployments, not a default.** vLLM (JIT/Triton kernels, no AOT engine) is the default serving path precisely because it doesn't have this problem. This is [serving.md](./serving.md)'s call to finalize; the image plumbing is here.
- **ONNX Runtime EPs (`serve-onnx`).** ORT with the **CUDA EP** (portable, JIT), **TensorRT EP** (AOT, same per-arch caveat as above), and later **ROCm/MIGraphX EP** for AMD. Default to the CUDA EP for portability; TensorRT EP behind the same per-arch cache discipline.
- **CUDA graphs.** Used where the serving engine supports them (vLLM, TRT-LLM) to cut launch overhead; they're an engine-internal optimization, not a separate image concern.
- **JAX — verdict: not in catalog v1.** PyTorch dominates our target workloads (research + fine-tuning + serving), and JAX's real home is **TPU**, which we have none of; on NVIDIA GPUs JAX is viable but a distinctly smaller ecosystem, and its best-supported targets are data-center Ampere/Hopper/Blackwell, not consumer cards [[JAX on NVIDIA GPUs](https://lambda.ai/blog/pytorch-to-jax-on-lambda-for-enterprise-ml); [OpenXLA](https://openxla.org/)]. We defer a `jax` image until there's real demand, at which point we'd derive it from NVIDIA's JAX release containers rather than build XLA ourselves. Flagged as an open question (§10).

---

## 5. ROCm reality check

We want AMD in the fleet — but honestly, not blindly. The 2026 situation:

- **Officially supported consumer AMD in ROCm 7.2 (March 2026):** RDNA3 (`gfx1100/1101/1102` — RX 7000 series incl. RX 7900 XTX/XT) and, newly, RDNA4 (`gfx1200/1201` — RX 9070/9070 XT). RDNA2 (`gfx1030`, RX 6000) is also covered [[ROCm compat matrix](https://rocm.docs.amd.com/en/latest/compatibility/compatibility-matrix.html); [Phoronix: ROCm 7.2](https://www.phoronix.com/news/AMD-ROCm-7.2-Released)]. Official **PyTorch wheels** exist for `gfx1100/1101/1102` and `gfx1200/1201` [[ROCm compat matrix](https://rocm.docs.amd.com/en/latest/compatibility/compatibility-matrix.html)].
- **The gfx-target mess is real.** Even popular cards aren't cleanly in the official table — the RX 7900 XTX (`gfx1100`) frequently "works in practice" because it shares an LLVM target with the officially-listed PRO W7900, but *shares-a-target* is not *is-supported*. And **vLLM lacked native RDNA4 (`gfx1201`) attention kernels as of May 2026** even though the card is "supported," so RDNA4 serving throughput trailed RDNA3 [search result, [vLLM/ROCm](https://rocm.blogs.amd.com/software-tools-optimization/vllm-omni/README.html)]. *(Flag: the "works in practice via shared gfx target" claim is community-reported, not an AMD support guarantee — we must qualify each SKU ourselves.)*
- **vLLM-on-ROCm maturity:** genuinely first-class for CDNA3 (MI300, CK attention kernels) and *working* on RDNA3/4 via Triton attention, but with a longer tail of missing fused kernels than the CUDA path [[vLLM/ROCm first-class](https://rocm.blogs.amd.com/software-tools-optimization/vllm-omni/README.html)].

**Our staged plan, gated deliberately:**

1. **NVIDIA ships first.** ROCm is a fast-follow, not a launch dependency.
2. **Allowlist, not open enrollment.** Only an explicit set of qualified AMD SKUs may enroll — starting with **RX 7900 XTX / XT (`gfx1100`)**, then RDNA4 once vLLM kernels land. The agent refuses any AMD card not on the allowlist. This is the same posture isolation.md takes on AMD reset quirks ([../platform/isolation.md](../platform/isolation.md) §6) — the AMD long-tail is a support swamp and we gate it rather than pretend.
3. **Per-SKU qualification** (a card runs our `torch`/`train`/`serve-vllm` ROCm images through a real workload + reset probe) before that SKU joins the allowlist.
4. **Honest capability labels.** A ROCm serving node advertises which engines/kernels actually work on its gfx target, so scheduling never lands a job the card can't really run.

---

## 6. Image build & distribution

**Monorepo of Dockerfiles + Bake.** All images live in one repo as a `docker buildx bake` graph, so the shared-layer structure (§2) is explicit and CI builds the whole matrix from one source of truth.

**Pinned lockfiles.** Every image's Python deps are resolved with `uv` and committed as an exported lockfile (`uv.lock` → `uv pip compile` export). Builds install *only* from the lockfile — no floating `pip install torch`. A catalog release is a set of lockfiles; bumping any dependency is a reviewed diff, not a build-time surprise. **This is what makes "aggressive pinning" (§1) real** rather than aspirational.

**Security: scan + SBOM.** Every built image is vulnerability-scanned (Trivy/Grype) and ships a signed **SBOM** (SPDX/CycloneDX). Scanning gates publish; a critical CVE blocks the tag until patched (and mints a `.N` patch tag, §2.2). Images are **signed** (cosign) and the agent verifies signatures before running — a curated catalog is only as trustworthy as its provenance.

**Registry + content-addressed distribution.** Images live in a registry on **operator infra**. Distribution to nodes reuses the **P2P content-addressed chunk store** already specified for weights in [../platform/networking.md](../platform/networking.md) §5 — we do **not** build a second distribution path. Networking already settled on **Dragonfly (CNCF-graduated)** for P2P image/model distribution; its native image format is **Nydus**, a content-addressable RAFS format that layers directly onto Dragonfly's P2P fabric [[Dragonfly](https://d7y.io/); [Nydus (Dragonfly image service)](https://github.com/dragonflyoss/nydus)]. So our images are **built as Nydus images**, shared node-to-node over the WG mesh exactly like weights, and origin traffic collapses when many nodes want the same popular base layer.

**Lazy pulling — decision: Nydus (RAFS + EROFS).** Multi-GB CUDA images make lazy pulling worth it: start the container while the image streams, fetching layer contents on first access instead of pulling the whole tarball up front. Of the 2026 options — eStargz (OCI-compatible, simplest), SOCI (AWS, external zTOC index), Nydus (purpose-built RAFS) — **we pick Nydus**, for one decisive reason beyond raw speed: **Nydus's EROFS backend eliminates FUSE from the read path** (kernel VFS → EROFS driver → page cache) [[lazy-pull deep dive](https://blog.zmalik.dev/p/lazy-pulling-container-images-a-deep)], and **EROFS is exactly the read-only rootfs format we want for Tier A microVMs (§7)** — so one format serves both container lazy-pull *and* VM rootfs. Nydus is also already the Dragonfly-native format, so it's zero extra infrastructure. Datadog-scale reports of 5-minute pulls dropping to seconds validate the approach for AI images [[Dragonfly v2.4/Nydus](https://www.cncf.io/blog/2026/02/05/dragonfly-v2-4-0-is-released/)]. *(Flag: lazy-pull's win shrinks when a job touches most of the image anyway — big training images that import half the world — so we measure per-image benefit and don't over-promise; lazy pull helps cold-start latency, not total bytes for full-image jobs.)*

**Image-size discipline.** CUDA + cuDNN + torch is multi-GB and that's normal. Mitigations, in priority order: (1) **shared base layers** — every image `FROM base-cuda`/`torch`, so a node caching the base pays it once; (2) **node-side layer cache** (agent cache manager, [../platform/host-agent.md](../platform/host-agent.md)) — layers persist across jobs; (3) **prefetch on placement** — when the scheduler assigns a job, the agent starts pulling the target digest before the job is released, hiding pull latency behind scheduling; (4) lazy pull (above) for the cold-start tail. We track image size as a CI metric and treat unexplained growth as a regression.

---

## 7. VM rootfs derivation for Tier A

Tier A needs the **same image** to boot as a microVM rootfs — the settled constraint: *the container image doubles as the VM rootfs* ([../platform/isolation.md](../platform/isolation.md)). We don't maintain two artifacts; we **derive** the VM form from the OCI image.

**Standard, pragmatic path:**

1. **Materialize the OCI image to a read-only rootfs.** The established approach is to flatten the image layers into a filesystem image — historically `mkfs.ext4` on a file-backed device + export the rootfs into it [[Firecracker rootfs setup](https://github.com/firecracker-microvm/firecracker/blob/main/docs/rootfs-and-kernel-setup.md)] (cited as prior art only — our VMM is Cloud Hypervisor, see [ADR-0002](../adr/0002-cloud-hypervisor-vfio-microvm-tier.md)). We instead materialize to **EROFS**, the read-only format Nydus already produces (§6): host-side, each OCI layer becomes an EROFS artifact and the merged metadata gives the guest one read-only rootfs, with a writable ext4 upper for scratch [[microVM/EROFS approach](https://microsandbox.dev/blog/oci-filesystem-47x-faster)]. **One format, two consumers** — the payoff called out in §6.
2. **Loom guest kernel.** The microVM boots our **minimal pinned guest kernel** (current LTS, trimmed config, only the virtio + GPU-passthrough drivers needed), *not* the host's kernel — this is the Tier A isolation win ([../platform/isolation.md](../platform/isolation.md) §4.1).
3. **Minimal init.** A small init at `/sbin/init` brings up the guest, mounts scratch, and launches the job — the standard microVM requirement that init never exits [[Firecracker rootfs setup](https://github.com/firecracker-microvm/firecracker/blob/main/docs/rootfs-and-kernel-setup.md)].
4. **Guest driver bundle per GPU generation.** Because Tier A runs the vendor driver *in the guest* (§3.5), we ship a **pinned kernel-mode driver bundle keyed to the GPU generation** (Ampere/Ada/Blackwell; RDNA3/4 for AMD). The rootfs is generation-agnostic; the driver bundle is overlaid at boot based on the passed-through card. This is where our "homogeneous guest driver" advantage is concretely delivered.

Keeping this pragmatic: we are **not** inventing a rootfs format. EROFS + a minimal LTS kernel + a tiny init is the boring, well-trodden path; the only Loom-specific piece is the per-generation guest driver overlay.

---

## 8. Customization within the walls

Curated images don't mean a straitjacket. The escape hatch (settled: [../platform/isolation.md](../platform/isolation.md) §3.4) is **bring-your-own script + package installs on top of a base image, within egress-allowlisted mirrors.**

- **`pip`/`uv` installs at job start** from **our PyPI mirror** ([../platform/networking.md](../platform/networking.md) §5 default-deny egress → allowlisted package mirror only). Installs are **cached per node**, so the second job needing the same wheel doesn't refetch. `requirements.txt` and inline `uv pip install` are both supported.
- **No arbitrary base images, no arbitrary egress.** A renter can add `some-lib==1.2.3` from our mirror; they cannot `apt-get` from a random PPA or `curl | bash` from the open internet — the egress firewall drops it.
- **`loom env freeze`.** After a job's environment is assembled (base image digest + the exact resolved versions of everything the renter added), `loom env freeze` snapshots a **reproducible env manifest**: `{ image: sha256:…, extra_packages: [pinned], mirror_snapshot: … }`. Rerunning with that manifest reproduces the environment bit-for-bit (§9).
- **Future: verified custom images.** The obvious next step is a **build service** that accepts a renter's Dockerfile-on-top-of-base, **scans + signs** the result, and admits it to the catalog as a private image. That trades support surface for flexibility and is an **open question** (§10), not a v1 commitment — we ship the pip/uv escape hatch first because it covers most real needs at a fraction of the risk.

---

## 9. Reproducibility

Reproducibility is the payoff of all the pinning above. A Loom run is reproducible because **everything that affects it is pinned:**

- **Image digest** (`sha256:`, §2.2) — not a floating tag.
- **Env manifest** (`loom env freeze`, §8) — the base plus every renter-added package, pinned.
- **Seed policy** — seeds recorded per run so sampling/initialization is replayable.

**The honest caveat: same-image-same-code is necessary, not sufficient, for bit-identical results on GPUs.** GPU nondeterminism (non-deterministic reductions/atomics, autotuner-selected kernels, cross-arch kernel differences, `torch.compile` picking different kernels on a 4090 vs a 3090) means two runs of the identical pinned environment on *different* cards can diverge numerically. We pin the environment; we cannot pin the silicon a job lands on unless the renter constrains placement to one arch. The full determinism story — deterministic algorithm flags, arch-pinned placement, the accuracy/throughput trade — lives in **[training.md](./training.md)'s determinism section**; this doc guarantees the *environment* is reproducible and is explicit that the *numerics* have a hardware floor we don't hide.

---

## 10. Open questions

1. **Per-image driver floors for RTX 50-series (Blackwell).** The CUDA 12.x family floor is `525.60.13`, but the practical floor for `cu128`+ images on the newest consumer silicon needs measurement, and it interacts with gVisor's runsc driver-support window ([../platform/isolation.md](../platform/isolation.md) §9). Verify before enrolling 50-series broadly. *(Driver floors for 2026 Blackwell consumer cards are not yet independently verified here.)*
2. **TensorRT engine-cache economics.** Is a per-`sm_arch` TRT-LLM engine cache worth the operational weight given our arch fan-out, or does vLLM's JIT path make TRT-LLM not worth offering at all? [serving.md](./serving.md) to decide with real QPS data.
3. **Verified custom-image build service (§8).** What's the scan/sign bar, and does it ever justify accepting renter Dockerfiles — or is pip/uv-on-base permanently sufficient?
4. **JAX demand.** Do we ever ship a `jax` image, and if so derived from NVIDIA's JAX containers — or is consumer-NVIDIA JAX demand too thin to support?
5. **ROCm allowlist expansion cadence.** Concrete per-SKU qualification suite for AMD, and the trigger to move RDNA4 from "enrolled but caveated" to "first-class serving" once vLLM `gfx1201` kernels land.
6. **Lazy-pull real-world benefit per image.** Measure which catalog images actually cold-start faster with Nydus lazy pull vs. which touch most of the image anyway (big training images) and gain little.
7. **Catalog release cadence vs. churn.** Monthly may be too fast (churn, test cost) or too slow (renters want the newest torch day-one). Tune once we see real upgrade pressure.

---

*Cross-references: [../platform/isolation.md](../platform/isolation.md) (sandbox tiers, curated-image rationale, driver-window gating) · [../platform/host-agent.md](../platform/host-agent.md) (driver detection, layer/weight cache, prefetch) · [../platform/networking.md](../platform/networking.md) (Dragonfly P2P distribution, egress-allowlisted mirrors) · [training.md](./training.md) (train image stack, determinism) · [serving.md](./serving.md) (serve images, vLLM vs TensorRT-LLM, engine determinism).*
