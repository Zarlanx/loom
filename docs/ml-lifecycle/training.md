# Training & Fine-Tuning on Loom

Loom's training offering is built around one uncomfortable truth: our nodes are consumer GPUs on residential internet that can vanish mid-job when their owner reclaims the machine. That constraint is not a footnote — it shapes every design decision below. We do **not** try to be a hyperscaler. We are the best place in the world to run the fine-tunes and small trainings that actually fit on a 24–32GB card, made reliable by a platform that treats interruption as normal rather than exceptional.

This document covers the workload taxonomy we support, the software stack (as of July 2026), our interruption-tolerant checkpointing feature, managed recipes, single-host multi-GPU, and Hugging Face integration. Data ingestion is owned by [data.md](../ml-lifecycle/data.md); the images these jobs run in are owned by [environments.md](../ml-lifecycle/environments.md); how a job survives an owner-eject at the agent level is owned by [host-agent.md](../platform/host-agent.md); and measuring what a trained model is worth is [evaluation.md](../ml-lifecycle/evaluation.md).

---

## 1. Workload taxonomy for this hardware class

A Loom node is a consumer or prosumer GPU: RTX 3090/4090/5090-class, 24–32GB VRAM typical, occasionally a 2–4 GPU rig in one host. That envelope defines what we can and cannot train. Here is what fits and what we explicitly refuse.

### (a) PEFT fine-tuning of LLMs, 1B–70B — the bread and butter

LoRA and QLoRA are why Loom exists as a training platform. QLoRA (4-bit NF4 base weights + LoRA adapters) matches 16-bit LoRA quality on standard benchmarks while collapsing VRAM to something a single consumer card can hold.[^qlora] Rough single-card fit on a 24GB 4090, QLoRA, modest sequence length and gradient checkpointing on:[^vram]

- **7B**: comfortable (~12GB working set)
- **13B**: comfortable (~20GB)
- **34B**: fits with care (careful batch/seq settings, gradient checkpointing mandatory)
- **70B**: **does not fit a single consumer card** — QLoRA 70B needs roughly ~50GB (4-bit base ~35GB plus adapter gradients, optimizer state, and activations), so it clears neither a 24GB nor a 32GB part, and it sits right at the edge of a 2×4090 (48GB) rig — too tight to rely on. On Loom, 70B PEFT is only schedulable on the rare multi-GPU host with enough aggregate VRAM, and even then it is slow. We surface this honestly at recipe selection, not after you've paid for GPU-hours.

### (b) Full fine-tuning of small models (<8B) on multi-GPU rigs

Full-parameter fine-tuning of models up to ~8B is viable **only** when the node has 2–4 GPUs and we can shard the optimizer state across them with FSDP2. A single 24GB card cannot hold weights + gradients + Adam moments for even a 7B in fp16/bf16. This is a multi-GPU-node-only workload and the scheduler enforces that.

### (c) Training small models from scratch

Embedding models, text/vision classifiers, diffusion LoRAs, tabular and small vision nets — these are a genuinely great fit for a single consumer GPU and often finish in minutes to hours. No quantization gymnastics required. This is the least glamorous and most reliable corner of the offering.

### (d) RL fine-tuning (DPO / GRPO) at PEFT scale

Preference optimization (DPO) and reasoning-style RL (GRPO) run on top of PEFT adapters at sizes that fit our nodes. GRPO is more memory-efficient than PPO — it drops the value model — which is exactly why it's tractable on consumer hardware.[^trl] We support DPO/GRPO on PEFT for the same model sizes as (a), minus the reference-model overhead (which pushes the practical ceiling down a tier).

### (e) What we do NOT do

- **Pretraining large models from scratch.** Wrong tool.
- **Multi-node WAN distributed training.** See the boxed rationale below.

> **Why no multi-node training.** Distributed training synchronizes gradients every step. That works because datacenter GPUs are wired with NVLink (~900GB/s) or InfiniBand (hundreds of Gb/s). A Loom node has ~30Mbps residential upload. Gradient sync over that link would spend orders of magnitude more time communicating than computing — the GPUs would sit idle waiting on the network. There is no algorithm that fixes a five-order-of-magnitude interconnect gap. We scope to **single-node** training (optionally multi-GPU *within* one host, where the interconnect is at least PCIe). This is a physics decision, not a roadmap gap.

### Taxonomy table

| Model size | Method | Min VRAM (approx) | Typical Loom node |
|---|---|---|---|
| 1–7B | QLoRA / LoRA | ~12GB | Single 3090/4090 (24GB) |
| 13B | QLoRA | ~20GB | Single 4090/5090 (24–32GB) |
| 34B | QLoRA | ~24GB (tight) | Single 24GB, grad-ckpt on |
| 70B | QLoRA | ~50GB | Multi-GPU rig only; discouraged |
| <8B | Full FT (FSDP2/ZeRO) | 2–4× GPU aggregate | 2–4 GPU host |
| <1B (from scratch) | Full training | 4–16GB | Any single GPU |
| Diffusion LoRA / classifier / embedding | LoRA / full | 8–24GB | Single GPU |
| 1–13B PEFT | DPO / GRPO | +ref-model overhead vs (a) | Single GPU, one tier smaller |

VRAM figures are order-of-magnitude planning numbers, not guarantees — actuals depend on sequence length, batch size, optimizer, and kernel choices. Our pre-flight estimator (§8) gives per-job numbers.

---

## 2. The stack (July 2026)

Every tool below ships pre-installed and version-pinned in our curated training images ([environments.md](../ml-lifecycle/environments.md)). We pin because a fine-tune that worked last week must work this week.

**PyTorch 2.x.** The foundation. PyTorch 2.10 shipped January 2026, 2.11 in March, and 2.12 (June 2026) is the current latest; `torch.compile` is mature and gives real throughput wins on our workloads.[^pytorch] Loom images pin a known-good 2.x and enable `torch.compile` by default in managed recipes (with a per-recipe escape to disable it when it fights a custom model).

**FSDP2** (Fully Sharded Data Parallel v2). This is our default for single-host multi-GPU sharding. FSDP2 represents parameters as DTensors (vs FSDP1's flat-parameter approach), giving deterministic memory release, lower per-GPU memory, and clean `torch.compile` composability. FSDP1 is deprecated in recent PyTorch.[^fsdp2] For full FT of small models on a 2–4 GPU rig, FSDP2 is what Loom recommends and wires up for you.

**DeepSpeed ZeRO.** Still shipped, but recommended narrowly. In the regime where both fit memory, FSDP2 delivers meaningfully higher per-iteration throughput than ZeRO-3, mainly from cleaner `torch.compile` integration.[^deepspeed] DeepSpeed still wins on **offload**: it can offload parameters and optimizer separately and target NVMe (ZeRO-Infinity), which FSDP2 cannot. So Loom recommends DeepSpeed only when a job needs aggressive CPU/NVMe offload to fit at all — the "it won't fit any other way" case. Otherwise: FSDP2.

**Hugging Face Transformers / PEFT / TRL.** The core modeling, adapter, and post-training layer. TRL v1.0 (April 2026) unified SFT, reward modeling, DPO, and GRPO into one stable post-training stack; `SFTTrainer`, `DPOTrainer`, and `GRPOTrainer` are all first-class.[^trl] Loom recommends TRL as the default trainer for LLM fine-tuning and RL; it's the substrate our `loom-ckpt` HF callback (§3) hooks into.

**Unsloth.** Hand-derived kernels and custom CUDA that deliver up to ~2× faster training and up to ~70% less VRAM with no accuracy loss on supported architectures; supports LoRA, QLoRA, full FT, and DPO/ORPO/RLHF.[^unsloth] Two things matter for Loom. First, its sweet spot is **single-GPU** — which is most of our fleet, so it's a first-class recommendation for single-card QLoRA. Second, **licensing/limits**: the core package is Apache-2.0 (some optional components like the Studio UI are AGPL-3.0), free for individual and local use. Multi-GPU exists but historically sat behind the Pro/Enterprise tier and is less mature than the single-GPU path.[^unsloth] Loom's stance: recommend Unsloth for single-GPU QLoRA where its accelerated path covers the model; fall back to plain HF+PEFT when it doesn't. **Flag:** verify the exact multi-GPU licensing terms before we bundle Unsloth multi-GPU in a paid Loom recipe.

**Axolotl** and **LLaMA-Factory.** Config-driven recipe layers — you write a YAML instead of Python. Axolotl (now under Axolotl AI, active through v0.29.0 in Feb 2026) emphasizes reproducibility and version-controllable runs; LLaMA-Factory offers the broadest method menu (DoRA, LoRA+, PiSSA, KTO, ORPO).[^recipes] Loom's managed recipes (§4) are essentially a curated, cost-estimated wrapper over this style of config, and users can bring their own Axolotl/LLaMA-Factory YAML on the base image.

**bitsandbytes / QLoRA.** The 4-bit NF4 quantization engine behind QLoRA; NF4 + double-quantization is the format that makes 13–34B tunable on 24GB.[^qlora] Always present in LLM images.

**FlashAttention-3.** Important honesty point: **FA-3 is SM90/Hopper-only and still beta.** On Ada (RTX 4090) and Blackwell consumer (5090), FA-3 does not apply — those cards run **FlashAttention-2**, which is the correct and supported path for the entire Loom fleet.[^fa3] Our images ship FA-2 as the attention kernel for consumer cards and do not advertise FA-3 speedups we can't deliver. (FA-4 targets Hopper/Blackwell datacenter parts and is likewise not our hardware.)

**Liger kernels.** Triton kernels that report ~20% throughput gain and up to ~60% memory reduction vs stock HF, with post-training losses (DPO/ORPO) up to ~80% more memory-efficient; dependencies are just Torch + Triton.[^liger] Because they're pure Triton, they run on our consumer cards without SM90 gating (unlike FA-3). Loom enables Liger by default in recipes for supported model families — it's one of the highest-leverage "free" wins on memory-constrained nodes.

### Recipe-layer comparison

| Layer | Interface | Best for on Loom | Multi-GPU | Notes |
|---|---|---|---|---|
| Raw HF (Transformers+PEFT+TRL) | Python | Max control, custom models | via FSDP2/Accelerate | Our escape hatch & substrate |
| Unsloth | Python API | Single-GPU QLoRA, max speed/VRAM | Pro-tier / less mature | Apache-2.0 core; flag licensing |
| Axolotl | YAML config | Reproducible, version-controlled runs | via Accelerate | Active, production-oriented |
| LLaMA-Factory | YAML/UI | Broadest method menu (DoRA, KTO…) | supported | Widest feature surface |

---

## 3. Interruption-tolerant training (the differentiator)

This is the section that makes Loom a platform rather than a pile of rented GPUs.

**The signal.** When a node owner reclaims their machine, the host agent doesn't just kill the process. It fires a **checkpoint-now signal with a grace window** (mechanism owned by [host-agent.md](../platform/host-agent.md)). Our training images listen for it.

**`loom-ckpt`.** Every training image ships a `loom-ckpt` helper that turns "we're about to lose this node" into "the job resumes elsewhere at the exact same step." It provides three integration surfaces so it drops into whatever you're already using:

- **HF Trainer callback** — a `TrainerCallback` that captures the checkpoint on signal; the default path for TRL/Transformers users.
- **PyTorch Lightning plugin** — for Lightning-based training loops.
- **Raw hook** — a decorator/context manager for bespoke loops, no framework assumed.

**What it captures.** Model weights (or adapter weights), optimizer state, LR-scheduler state, dataloader position, and **RNG state** (Python/NumPy/CUDA). On resume it restores the *exact* global step and RNG state, so the run continues as if the eject never happened — not "restart from last epoch."

**Incremental, async upload.** Checkpoints go to object store, and we upload **only changed shards** since the last checkpoint, asynchronously, so training keeps moving while the previous checkpoint drains over the residential uplink. We keep-last-N and prune older checkpoints.

**Overhead, honestly.** This is where residential bandwidth bites, so we won't pretend it's free:

- **LoRA/QLoRA adapters** are small (tens to low-hundreds of MB). Uploading an adapter checkpoint over ~30Mbps up is **seconds**. Interruption tolerance here is nearly free — this is the happy path, and it's most of our fleet's workload.
- **Full-model checkpoints** (full FT, from-scratch) are the hard case: a multi-GB checkpoint over ~30Mbps up is **minutes**, which can exceed a tight grace window. Mitigations: (1) incremental shard upload so steady-state deltas are small; (2) **async** upload overlapping compute; (3) a local-disk checkpoint written first (fast) with background drain to object store, so an eject during upload still leaves a resumable state on a peer if the node reappears, or forces a one-time full re-upload otherwise. Recipes for full-FT workloads default to a longer checkpoint interval to amortize this.

**Resumable-by-default.** Managed recipes (§4) enable `loom-ckpt` automatically. You don't opt in to surviving an eject; you'd opt *out*. See the requeue/reschedule mechanics in [host-agent.md](../platform/host-agent.md) — checkpoint-and-requeue is a core platform path, not a training-only concern.

---

## 4. Job templates / managed recipes

The UX target:

```
loom train --recipe qlora \
  --model meta-llama/Llama-3.1-8B \
  --data mydata@v2
```

A **recipe** bundles four things:

1. **A pinned image** — exact CUDA/PyTorch/library versions, reproducible ([environments.md](../ml-lifecycle/environments.md)).
2. **A config schema** — typed, validated fields (LR, batch, seq len, LoRA rank, target modules…) with defaults tuned for consumer hardware.
3. **Sane defaults** — gradient checkpointing on, Liger on where supported, `loom-ckpt` on, sequence length that won't OOM a 24GB card.
4. **An upfront cost estimate** — before you spend a cent (§7).

`--data mydata@v2` resolves through the data layer's content-addressed manifests and node-local prefetch cache ([data.md](../ml-lifecycle/data.md)), so the recipe knows exactly what it's training on and can pin it into the model card's lineage (§6).

**Recipe families at launch:** `qlora`, `lora`, `full-ft` (multi-GPU-node-gated), `dpo`, `grpo`, `diffusion-lora`, `classifier`, `embedding`.

**Escape hatch.** `loom train --script train.py --image loom/base-cuda` runs an arbitrary script on the base image. You get `loom-ckpt`, the data cache, and HF integration; you give up the pre-tuned defaults and the upfront estimate. This is how Axolotl/LLaMA-Factory power users and researchers run whatever they want.

---

## 5. Multi-GPU within one host

Some Loom hosts have 2–4 GPUs. Within a single host we support **FSDP2** (sharding) and **DDP** (replication) for full FT and larger PEFT jobs. The scheduler **exposes topology** to the recipe — whether GPUs are linked over NVLink or only PCIe — because it changes what's worth attempting.

**The consumer-hardware reality, stated correctly:**

- **NVLink is dead on consumer cards from the 40-series onward.** The RTX 4090 and 5090 have no NVLink; Blackwell consumer did not bring it back. The *only* consumer card where NVLink is an option is the base RTX 3090 (not the 3090 Ti).[^nvlink]
- Worse, NVIDIA **blocks P2P (peer-to-peer) over PCIe in the driver** on GeForce cards — a product-segmentation decision, not a hardware limit. So even PCIe-connected consumer GPUs generally cannot do direct GPU-to-GPU P2P; traffic routes through host memory.[^nvlink]

**What this means for Loom.** Multi-GPU on our fleet is real but bandwidth-constrained: assume PCIe (often without P2P), not NVLink, unless the node is a 3090-NVLink pair. FSDP2's communication cost is therefore higher on consumer rigs than on datacenter cards. We schedule multi-GPU jobs to prefer 3090-NVLink pairs when available, size expectations to PCIe otherwise, and expose the topology to the cost estimator so users aren't surprised by slow all-gathers. We do not oversell consumer multi-GPU scaling.

---

## 6. Hugging Face integration

Fine-tuning is a round trip to the Hub, and Loom makes both directions clean.

**Auth (sealed secrets).** A user's HF token is stored as a **sealed secret** — encrypted, injected into the job's environment at runtime, never written to disk in plaintext, never logged, never visible to the node owner. (This mirrors the platform's broader secret-handling model; the node running your job is untrusted.) **Flag:** the exact sealing/attestation mechanism is a platform-security detail to be specified alongside [host-agent.md](../platform/host-agent.md).

**Pull via node cache.** Base models are fetched through the data layer's node-local prefetch cache ([data.md](../ml-lifecycle/data.md)), so a popular base (say Llama-3.1-8B) is cached near the node and doesn't re-download over residential bandwidth for every job.

**Push adapters/merged models as job output.** On success, the job can push LoRA adapters, or a merged full model, to the HF Hub as its output artifact.

**Auto-generated model cards with lineage.** Every pushed model gets a model card Loom generates automatically, stamped with **lineage**: the exact **data manifest** (content hash, from [data.md](../ml-lifecycle/data.md)), the **recipe** (image + config), and **eval results** (from [evaluation.md](../ml-lifecycle/evaluation.md)). This makes a Loom-trained model reproducible and auditable by construction — you can always answer "what data, what recipe, how good."

---

## 7. Cost / performance guidance

We will not invent benchmark numbers. Here is the honest posture and the methodology instead.

**The upfront estimate.** At `loom train` time, before you commit, the recipe predicts GPU-hours from `tokens × method` — token count comes from the resolved dataset, and each method has a measured throughput profile per node class. We show a **range**, not a point estimate, because real throughput depends on sequence-length distribution, batch size, checkpointing, kernel availability (Liger/FA-2), and how often the node gets interrupted.

**Representative jobs (shape, not promises):**

| Job | Node class | Honest expectation |
|---|---|---|
| 8B QLoRA, 50k samples | 4090 (24GB) | Hours, single-digit-to-low-double-digit GPU-hours; wide "depends" band |
| 13B QLoRA, 50k samples | 4090/5090 | Longer than 8B; grad-ckpt mandatory |
| <1B classifier from scratch | any GPU | Minutes to low hours |
| 7B full FT | 2–4 GPU rig | Slow on PCIe; FSDP2 comms-bound |

The cells deliberately say "depends" instead of "3.2 hours." **We would rather ship no number than a wrong one.**

**How we'll publish real numbers.** Loom runs a standing **benchmark harness** of fixed recipes on representative node classes, publishes measured GPU-hours with variance bands, and refreshes them as the fleet and stack change. Estimates in the CLI are then backed by *our own measured data*, and we publish the methodology (dataset, seq-len distribution, node spec, interruption rate) alongside every number so it's auditable. Until that harness has run, ranges are marked as modeled, not measured.

---

## 8. Failure modes & determinism

**OOM prediction (pre-flight VRAM estimator).** Before scheduling, we estimate peak VRAM from model size, dtype, optimizer, sequence length, batch size, LoRA config, and kernel choices, and refuse to place a job on a node that won't hold it — turning a mid-run CUDA OOM into a clear pre-flight error with a suggested fix (lower seq len, enable grad-ckpt, drop to QLoRA, request a bigger node). Estimation is approximate; we bias conservative and still catch true OOMs at runtime as a backstop.

**NaN / divergence detection with auto-halt.** The training loop watches loss for NaN/Inf and divergence (blow-ups, sustained increase). On trip, the job **auto-halts** rather than burning paid GPU-hours producing garbage, surfaces the last good checkpoint, and reports the step where it went wrong.

**Seed control.** Recipes accept a seed and set Python/NumPy/CUDA RNG from it; `loom-ckpt` persists and restores RNG state across interruptions (§3), so a resumed run follows the same trajectory it would have without the eject.

**Bitwise-reproducibility limits — stated plainly.** We give you *run-to-run determinism within a node class* under a fixed seed and deterministic-algorithm flags, at some throughput cost. We do **not** promise bitwise-identical results *across different GPUs* (3090 vs 4090 vs 5090) or across nondeterministic CUDA kernels — different hardware and kernel selection legitimately produce different floating-point results. Because checkpoint/requeue can move a job to a *different* node mid-run, a resumed job may cross a hardware boundary; results stay statistically equivalent and seed-controlled, but not bitwise-identical. We document this rather than pretend otherwise.

---

## 9. Open questions

1. **Full-model checkpoint over residential uplink.** Incremental + async + local-first drain reduces the pain, but a multi-GB checkpoint during a short grace window is still the worst case. Do we require a minimum uplink for full-FT recipes? Cap full-FT to nodes with better bandwidth? Peer-to-peer checkpoint handoff to a nearby node?
2. **Unsloth multi-GPU licensing.** Exact terms for bundling Unsloth's multi-GPU path into a paid Loom recipe need legal verification before we ship it. (Flagged in §2.)
3. **Interruption accounting in cost estimates.** How do we fold expected interruption rate (which varies by node and time of day) into the upfront GPU-hour range without making it uselessly wide?
4. **Determinism guarantee tier.** Do we offer an opt-in "pin to one node class, no requeue across hardware" mode for users who need tighter reproducibility, trading away some interruption tolerance?
5. **34B QLoRA on 24GB headroom.** It "fits with care" — do we ship it as a first-class recipe with conservative defaults, or gate it behind larger nodes to avoid a wave of OOMs?
6. **P2P-enabled consumer nodes.** Some node owners run community driver patches that re-enable PCIe P2P on GeForce cards. Do we detect, trust, and schedule against that, or ignore it for support-surface reasons?

---

### Cross-references

- [data.md](../ml-lifecycle/data.md) — content-addressed manifests, node-local prefetch cache, dataset versioning (`mydata@v2`).
- [evaluation.md](../ml-lifecycle/evaluation.md) — eval results that land in auto-generated model cards.
- [environments.md](../ml-lifecycle/environments.md) — curated training images, pinned stack versions.
- [host-agent.md](../platform/host-agent.md) — owner-eject signal, grace window, checkpoint-and-requeue mechanics, sealed secrets.

---

[^qlora]: QLoRA uses 4-bit NF4 quantization of base weights plus LoRA adapters; NF4 + double-quantization matches 16-bit LoRA/full-FT quality on academic benchmarks. QLoRA: Efficient Finetuning of Quantized LLMs (arXiv:2305.14314); HF bitsandbytes 4-bit blog. https://arxiv.org/pdf/2305.14314 , https://huggingface.co/blog/4bit-transformers-bitsandbytes

[^vram]: Per-model QLoRA VRAM and 24GB fit (7B/13B comfortable, 34B with care). QLoRA 70B is ~50GB total (4-bit base ~35GB + adapter grads/optimizer/activations) per the cited Spheron table (~52GB, fits a single 80GB H100, does not fit a single 48GB card) — i.e. it clears no single consumer card and only marginally a 2×4090 rig; the original QLoRA paper demonstrated 65B on a single 48GB GPU. Figures are order-of-magnitude planning estimates. Spheron "GPU VRAM Requirements to Fine-Tune LLMs in 2026"; jarvislabs GPU-for-fine-tuning FAQ. https://www.spheron.network/blog/gpu-vram-requirements-fine-tune-llm-2026/ , https://jarvislabs.ai/ai-faqs/best-gpu-for-fine-tuning-llms

[^trl]: TRL v1.0 (April 2026) unified SFT / reward modeling / DPO / GRPO; GRPO is more memory-efficient than PPO (drops the value model). HF TRL v1.0 blog and MarkTechPost coverage. https://huggingface.co/blog/trl-v1 , https://github.com/huggingface/trl

[^pytorch]: PyTorch 2.10 released Jan 21 2026; 2.11 Mar 23 2026; 2.12 (Jun 17 2026) current latest; `torch.compile` mature with measured speedups over eager. PyTorch releases page. https://github.com/pytorch/pytorch/releases — **Flag:** the exact-latest 2.x point version as of publication should be re-verified before pinning; Loom images pin a specific known-good 2.x line, which need not be the newest.

[^fsdp2]: FSDP2 uses DTensor-based sharding, deterministic memory release, lower per-GPU memory, `torch.compile` composability; FSDP1 deprecated in recent PyTorch. PyTorch FSDP2 tutorial; OSC FSDP2 HOWTO. https://docs.pytorch.org/tutorials/intermediate/FSDP_tutorial.html , https://www.osc.edu/resources/getting_started/howto/howto_pytorch_fully_sharded_data_parallel_fsdp2

[^deepspeed]: FSDP2 ~higher per-iteration throughput than ZeRO-3 where both fit; DeepSpeed retains advantage on parameter/optimizer and NVMe offload (ZeRO-Infinity). VRLA Tech "DeepSpeed vs PyTorch FSDP 2026"; HF accelerate FSDP-vs-DeepSpeed concept guide. https://vrlatech.com/deepspeed-vs-pytorch-fsdp-which-distributed-training-framework-in-2026/ , https://huggingface.co/docs/accelerate/en/concept_guides/fsdp_and_deepspeed

[^unsloth]: Up to ~2× faster / ~70% less VRAM, no accuracy loss on supported archs; LoRA/QLoRA/full-FT + DPO/ORPO/RLHF; single-GPU focus; Apache-2.0 core (some optional components AGPL-3.0); multi-GPU historically Pro/Enterprise-tier. Unsloth README/docs; opentools pricing. https://github.com/unslothai/unsloth , https://unsloth.ai/docs

[^recipes]: Axolotl (Axolotl AI, active through v0.29.0 Feb 2026) config-driven and reproducibility-focused; LLaMA-Factory broadest method set (DoRA, LoRA+, PiSSA, KTO, ORPO). dev.to "Fine-Tuning in 2026" comparison; jenova.ai LLaMA-Factory guide. https://dev.to/ultraduneai/eval-003-fine-tuning-in-2026-axolotl-vs-unsloth-vs-trl-vs-llama-factory-2ohg , https://www.jenova.ai/en/resources/llama-factory-complete-guide-to-llm-fine-tuning

[^fa3]: FlashAttention-3 is SM90/Hopper-only and still beta; Ada (4090) and Blackwell consumer run FlashAttention-2; FA-4 targets Hopper/Blackwell datacenter parts. Dao-AILab flash-attention repo; Spheron FA2-vs-FA3 guide. https://github.com/Dao-AILab/flash-attention , https://www.spheron.network/blog/flashattention-2-vs-flashattention-3-h100-h200-guide/

[^liger]: Liger Kernel: ~20% throughput gain, up to ~60% memory reduction vs HF; post-training losses (DPO/ORPO) up to ~80% more memory-efficient; deps = Torch + Triton only. LinkedIn Liger-Kernel repo; Spheron Liger writeup. https://github.com/linkedin/Liger-Kernel , https://www.spheron.network/blog/liger-kernel-llm-training-gpu-cloud/

[^nvlink]: NVLink removed on consumer from Ada (40-series) onward, not restored on Blackwell consumer; base RTX 3090 is the last consumer card with NVLink (not 3090 Ti); NVIDIA blocks P2P over PCIe on GeForce in-driver as product segmentation. Tom's Hardware "GeForce Cards Lack P2P"; runaihome NVLink-vs-PCIe 2026; smcleod P2P driver patch. https://www.tomshardware.com/news/nvidia-confirms-geforce-cards-lack-p2p-support , https://runaihome.com/blog/multi-gpu-local-ai-nvlink-vs-pcie-2026/
