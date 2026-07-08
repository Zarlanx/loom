# The Managed Recipe Catalog

**Status:** Design (July 2026)
**Scope:** The concrete contract that makes "managed everything about training" real. A recipe is the versioned artifact a renter picks when they run `loom train --recipe`; this document specifies what a recipe *is*, its manifest format, the v1 catalog, how recipes compose into the train→eval→deploy pipeline, the cost-estimation contract, and how recipes are QA'd and published.

This doc is downstream of [training.md](./training.md), which owns the *stack* (PyTorch, TRL, Unsloth, FSDP2, Liger, `loom-ckpt`), the VRAM reality, and the interruption-tolerance mechanics. Recipes are a curated, cost-estimated, version-pinned wrapper over that stack — the "job templates / managed recipes" idea in [training.md](./training.md) §4, specified. It leans on [data.md](./data.md) for dataset manifests, [evaluation.md](./evaluation.md) for the terminal eval pass, [environments.md](./environments.md) for the pinned images a recipe references by digest, and [../product/deployment.md](../product/deployment.md) for the CLI narrative. It does **not** re-derive any of those; it composes them.

> **Backend scope (ADR-0015).** The manifests and stack referenced here describe the **CUDA backend's** implementation. A recipe is one renter-facing contract with per-backend implementations (MLX: `mlx-lm` LoRA; CPU: llama.cpp); the backend-polymorphic contract and its `backends:` manifest field live in [../platform/compute-backends.md](../platform/compute-backends.md) ([ADR-0015](../adr/0015-pluggable-compute-backends.md)).

---

## 1. What a recipe is

A **recipe** is a versioned, immutable artifact — not a script and not a preset blob. It is the complete contract for one class of training job, and it bundles seven things:

1. **A pinned image digest.** Not a tag — a `sha256:` digest resolved from the [image catalog](./environments.md) (e.g. the digest behind `loom/train:2026.07-cu126-torch2.12`). Digests are the reproducibility contract (`environments.md` §2.2); a recipe never floats on a mutable tag.
2. **A config schema (JSON Schema).** Every knob a renter may set — model ref, dataset ref, LoRA rank, sequence length, LR schedule — with types, bounds, and enums. The schema is the wall: renters override config *within schema bounds*, and out-of-bounds input is rejected at submit time, not at CUDA-OOM time.
3. **Defaults.** Every field has a value tuned for consumer hardware (grad-checkpointing on, Liger on where supported, `loom-ckpt` on, a sequence length that won't OOM a 24GB card — [training.md](./training.md) §4). A renter who sets nothing gets a job that runs.
4. **A resource-requirements function.** A pure function `config → resource claim` that derives peak VRAM, GPU count, and a driver/arch floor from the config (model params × method), so the scheduler can refuse to place a job on a node that won't hold it *before* spending money ([training.md](./training.md) §8).
5. **A cost estimator.** A pure function `config → GPU-hour range → dollar range`, surfaced at `--dry-run` (§5).
6. **Eval defaults.** The suite ref + judge config the terminal eval node runs against the final checkpoint ([evaluation.md](./evaluation.md) §1). No recipe produces a "trained but never scored" artifact.
7. **An output contract.** The exact artifacts a successful run emits: the trained weights (adapter or merged), a model card, an eval report, and a lineage record (§4).

**Immutability and refs.** A published recipe version is frozen forever. Refs look like `qlora-sft@3` — name plus integer version. `qlora-sft@3` resolves to one image digest, one schema, one set of defaults, one estimator — none of which can change under a rerun. Bumping any of them (new image, new default, widened bound) mints `qlora-sft@4`; `@3` still resolves identically a year later. An unversioned `qlora-sft` resolves to the latest published version *at submit time* and is pinned into the run's lineage as the concrete `@N` it resolved to, so the record is never ambiguous.

**The escape hatch.** Recipes are the paved road, not a fence. A renter who needs something the catalog doesn't cover uses bring-your-own-script on a base image — `loom train --script train.py --image loom/train:...` — and keeps `loom-ckpt`, the data cache, and HF integration while giving up the tuned defaults and the upfront estimate. This is how Axolotl/LLaMA-Factory power users and researchers run arbitrary configs; it is specified in [training.md](./training.md) §4 and is the intended path off the managed catalog.

---

## 2. Recipe manifest format

A recipe is authored as a YAML manifest, validated and frozen at publish time. Below is `qlora-sft` in full. (JSON Schema is shown in YAML for readability; it is stored as JSON Schema Draft 2020-12.) Per [ADR-0015](../adr/0015-pluggable-compute-backends.md), the manifest also carries a `backends:` map — one entry per supported compute backend (`mlx` | `cuda` | `cpu`), each naming the backend's image or venv-bundle ref and its implementation — so a single recipe ref resolves to the right per-backend runtime at placement time; its shape is specified in [../platform/compute-backends.md](../platform/compute-backends.md) and omitted from the CUDA-only example below.

```yaml
recipe: qlora-sft
version: 3
description: >
  Supervised fine-tuning of a decoder LLM with QLoRA (4-bit NF4 base +
  LoRA adapters). The flagship single-GPU recipe. Trains an adapter, not
  a merged model; deploy it cheaply on a shared base (serving.md).
image:
  # Resolved to a digest at publish; tag shown for humans only.
  ref: loom/train:2026.07-cu126-torch2.12
  digest: sha256:d91f4c…                       # the contract (environments.md §2.2)

config_schema:                                  # JSON Schema Draft 2020-12
  type: object
  required: [base_model, dataset]
  additionalProperties: false
  properties:
    base_model:
      type: string
      description: HF repo id or loom model ref, resolved via node cache (data.md).
    dataset:
      type: string
      description: Loom dataset manifest ref, e.g. my-sft@v2 (data.md).
    seq_len:        { type: integer, default: 2048, minimum: 256, maximum: 8192 }
    epochs:         { type: number,  default: 3,    minimum: 0,   maximum: 20 }
    max_steps:      { type: integer, default: null, minimum: 1 }   # overrides epochs if set
    lora:
      type: object
      properties:
        rank:       { type: integer, default: 16, enum: [8, 16, 32, 64, 128] }
        alpha:      { type: integer, default: 32, minimum: 1, maximum: 256 }
        dropout:    { type: number,  default: 0.05, minimum: 0.0, maximum: 0.3 }
        target_modules:
          type: string
          default: all-linear
          enum: [all-linear, attn-only, qkv-only]
    quantization:
      type: string
      default: nf4-double                       # bitsandbytes 4-bit (training.md §2)
      enum: [nf4-double, nf4, none]              # `none` = plain LoRA, not QLoRA
    lr:
      type: object
      properties:
        peak:       { type: number, default: 2.0e-4, minimum: 1.0e-6, maximum: 1.0e-2 }
        schedule:   { type: string, default: cosine, enum: [cosine, linear, constant] }
        warmup_ratio: { type: number, default: 0.03, minimum: 0.0, maximum: 0.2 }
    batch:
      type: object
      properties:
        micro_batch:  { type: [integer, string], default: auto } # int, or "auto"
        grad_accum:   { type: [integer, string], default: auto } # solved to hit target tokens/step
        target_tokens_per_step: { type: integer, default: 65536 }
    seed:           { type: integer, default: 42 }

# Peak-VRAM estimate feeds the scheduler's placement gate (training.md §8).
# Inputs: base-model param count (resolved from base_model), method, seq_len,
# micro_batch, quantization, kernel availability (Liger/FA-2).
resource_claim:
  vram_estimator: qlora_v3          # references training.md's honest VRAM table
  min_vram_gb_formula: >
    quantized_base(params, quantization)
      + lora_state(rank, params)
      + activations(seq_len, micro_batch, grad_checkpointing=true)
  gpu_count: 1                       # single-GPU recipe; never requests a multi-GPU rig
  grad_checkpointing: true           # mandatory default on 24GB cards

checkpoint_policy:                    # loom-ckpt defaults (training.md §3)
  enabled: true                      # resumable-by-default; opt out, don't opt in
  interval_steps: 200
  keep_last_n: 3
  on_signal: capture-and-requeue     # host owner-eject → resume elsewhere at exact step

eval_defaults:                        # terminal eval node (evaluation.md §1)
  suite: instruction-following        # a versioned suite ref
  judge:
    mode: rubric                      # or pairwise
    model: loom/judge-default         # reached via Loom inference / sealed key
    dual_ordering: true               # position-bias mitigation on by default (evaluation.md §3)
  gate: null                          # renter may set min_score / no-regression

outputs:
  - adapter_safetensors               # the trained LoRA adapter (megabytes)
  - model_card                        # auto-generated, lineage-stamped (training.md §6)
  - eval_report                       # signed report (evaluation.md §7)
  - lineage_record                    # the reproducibility record (§4)

failure_policy:
  oom_retry:
    enabled: true
    strategy: halve-micro-batch-then-grad-accum   # preserve tokens/step
    max_attempts: 2                                # bounded auto-remediation
    floor: { micro_batch: 1 }                      # below this, fail with actionable msg
  divergence_halt:
    enabled: true                                  # NaN/Inf/blow-up → auto-halt (training.md §8)
    action: surface-last-good-checkpoint
```

**Notes on the load-bearing fields:**

- **`batch.*.auto`.** When `micro_batch` and `grad_accum` are `auto`, the recipe solves them at placement time from the node's real VRAM headroom and the `target_tokens_per_step`, so the effective batch (and thus the loss trajectory) is stable across node classes even as the micro-batch that physically fits varies. A renter who pins integers overrides the solver.
- **`resource_claim`.** The VRAM estimator is honest, not optimistic. It references the same order-of-magnitude table in [training.md](./training.md) §1 (7B QLoRA ~12GB, 13B ~20GB, 34B ~24GB tight, 70B ~50GB → multi-GPU-only). A 70B config on a single-GPU recipe is rejected at submit with "70B QLoRA needs ~50GB; not schedulable on a single consumer card — see training.md."
- **`failure_policy.oom_retry`.** Bounded auto-remediation, defined precisely: on a runtime CUDA OOM the recipe halves `micro_batch` and compensates with `grad_accum` to keep `target_tokens_per_step` constant (so the run stays comparable), resumes from the last checkpoint, and retries **at most twice**. If it hits the `micro_batch: 1` floor and still OOMs, it stops and returns the actionable message pattern from [../product/deployment.md](../product/deployment.md) §6 (name the knobs, point at a bigger GPU). It never silently changes the effective batch size or churns forever.

---

## 3. The v1 catalog

All v1 recipes are Loom-authored (§6). GPU-class guidance follows the taxonomy in [training.md](./training.md) §1; **no throughput numbers appear here** — per [training.md](./training.md) §7, we ship a range from the benchmark harness or nothing, never an invented constant.

> **Phase 1 ships a subset.** This full catalog is the *target state*. The self-hostable core ([roadmap.md](../product/roadmap.md) Phase 1) ships exactly **one** managed recipe — `qlora-sft` — plus the `loom run` escape hatch. `full-ft-small`, `dpo`, `grpo`, `diffusion-lora`, `whisper-ft`, `classifier-ft`, and `embeddings-ft` land in Phase 2+.

| Recipe | Base image | Typical GPU | Produces | Method basis |
|---|---|---|---|---|
| `qlora-sft` | `loom/train` | single 24GB (3090/4090) | LoRA adapter | QLoRA 4-bit NF4 + LoRA (TRL `SFTTrainer`) |
| `lora-sft` | `loom/train` | single 24–32GB | LoRA adapter | 16-bit LoRA (no quant) |
| `full-ft-small` | `loom/train` | 2–4 GPU host | merged model | Full FT, FSDP2 sharding |
| `dpo` | `loom/train` | single, one tier smaller | LoRA adapter | TRL `DPOTrainer` on PEFT |
| `grpo` | `loom/train` | single, one tier smaller | LoRA adapter | TRL `GRPOTrainer` on PEFT |
| `embeddings-ft` | `loom/train` | single 8–24GB | encoder + pooling | sentence-transformers style |
| `classifier-ft` | `loom/train` | single 8–16GB | classifier head | BERT-class sequence classification |
| `diffusion-lora` | `loom/train` | single 16–24GB | diffusion LoRA | SDXL/FLUX-class LoRA (`diffusers`/PEFT) |
| `whisper-ft` | `loom/train` | single 8–24GB | ASR model | Whisper fine-tune |

**Image note (honesty over convenience).** All nine recipes pin the `loom/train` lineage ([environments.md](./environments.md) §2), whose cataloged contents are the LLM/PEFT training stack (Transformers, PEFT, TRL, Unsloth, bitsandbytes, Liger, DeepSpeed). The four non-LLM recipes — `embeddings-ft` (sentence-transformers), `classifier-ft` (BERT-class, covered by Transformers), `diffusion-lora` (`diffusers`), and `whisper-ft` (Whisper via Transformers) — require libraries not all of which appear in that catalog manifest today; `diffusion-lora` and `embeddings-ft` in particular need `diffusers` / `sentence-transformers` added to the `train` image or shipped as a `train`-derived variant. This is an [environments.md](./environments.md) catalog gap to close before these recipes publish, not a claim that today's `loom/train` already bundles every dependency.

**`qlora-sft` — the flagship.** QLoRA is why Loom exists as a training platform ([training.md](./training.md) §1a): 4-bit NF4 base weights + LoRA adapters match 16-bit quality while collapsing VRAM onto a single consumer card. It produces a small adapter (tens to low-hundreds of MB), which is the happy path for interruption tolerance (checkpoints upload in seconds over residential uplink, [training.md](./training.md) §3) *and* the cheapest deploy path ([serving.md](./serving.md) adapters-on-shared-base). Choose it for the overwhelming majority of instruction-tuning / domain-adaptation jobs on 1B–34B models. This is the default a new fine-tuner should reach for.

**`lora-sft`.** Identical shape to `qlora-sft` but with `quantization: none` — 16-bit base, LoRA adapters. Choose it when the model comfortably fits unquantized on the target card and you want to avoid any quantization-induced quality risk, or when the base architecture isn't well-served by bitsandbytes 4-bit. Slightly higher VRAM for the same model; still a single-GPU adapter job.

**`full-ft-small`.** Full-parameter fine-tuning of models up to ~8B, viable **only** on a 2–4 GPU host where FSDP2 can shard optimizer state ([training.md](./training.md) §1b, §5). A single 24GB card cannot hold weights + gradients + Adam moments for a 7B in bf16; the resource-claim function requests a multi-GPU rig and the scheduler enforces it. It produces a *merged* model, not an adapter. Its checkpoint policy defaults to a longer interval, because full-model checkpoints are the hard case over residential uplink ([training.md](./training.md) §3). Choose it when adapters genuinely aren't enough and you have — or will rent — a multi-GPU node. Expect PCIe-bound comms on consumer rigs; we don't oversell it.

**`dpo` and `grpo`.** Preference optimization and reasoning-style RL on top of PEFT adapters, both on TRL v1.0's unified post-training stack (`DPOTrainer` / `GRPOTrainer`, [training.md](./training.md) §2). GRPO is tractable on consumer hardware precisely because it drops the value model that makes PPO memory-hungry. Both carry reference-model overhead, so their resource claim sits **one tier smaller** than the equivalent SFT ([training.md](./training.md) §1d): a model that QLoRA-SFTs comfortably on 24GB may need a smaller model or tighter settings under DPO/GRPO. Choose `dpo` for preference-pair alignment (often after an SFT pass), `grpo` for verifiable-reward RL (math, code, format-constrained tasks). Both take a preference/reward dataset and produce an adapter.

**`embeddings-ft`.** Fine-tuning a sentence-embedding model (sentence-transformers style: a base encoder + pooling, trained with contrastive/triplet or a matryoshka objective). Small, fast, single-GPU, no quantization gymnastics — the reliable corner of the offering ([training.md](./training.md) §1c). Produces an encoder deployable through the [serving.md](./serving.md) embeddings path. Choose it for retrieval, semantic dedup, or clustering models tuned to your domain.

**`classifier-ft`.** BERT-class sequence/token classification — fine-tune an encoder with a classification head. Tiny by LLM standards, finishes in minutes to low hours on any single GPU. Emits classic-ML metrics (accuracy, F1, ROC-AUC) into the eval framework's scalar-metric contract ([evaluation.md](./evaluation.md) §4) rather than an LLM benchmark. Choose it for intent detection, moderation, routing, or any supervised text-classification task.

**`diffusion-lora`.** LoRA fine-tuning of an SDXL/FLUX-class diffusion model via `diffusers` + PEFT. Fits a single consumer GPU with LoRA (full diffusion FT is out of scope for v1). Produces a diffusion LoRA. **Consistency note:** this is deployable — [serving.md](./serving.md) ships a `diffusers` engine image serving `/v1/images/generations`, so a `diffusion-lora` output has a first-class serving path. Choose it for style/subject adaptation of image generators. Eval uses FID / CLIP-score via the scalar-metric contract ([evaluation.md](./evaluation.md) §4), not an LLM suite.

**`whisper-ft`.** Fine-tuning a Whisper ASR model (language/domain/accent adaptation). Small models fit easily on any single GPU. **Consistency note:** deployable via [serving.md](./serving.md)'s `whisper`/`whisper.cpp` engine on `/v1/audio/transcriptions`. Eval reports WER/CER through the scalar-metric contract ([evaluation.md](./evaluation.md) §4). Choose it to adapt ASR to a domain vocabulary or a low-resource language.

---

## 4. The pipeline contract

Recipes are the composable unit of the managed lifecycle. A `loom train --recipe` invocation is a DAG whose terminal node is an eval job ([evaluation.md](./evaluation.md) §1), and whose output is directly deployable ([../product/deployment.md](../product/deployment.md) §3b):

```
loom train --recipe qlora-sft@3   →  ckpt@<hash>          (adapter + model card)
                                   →  auto eval run         (recipe eval_defaults)
                                   →  signed eval report    (attached to model card)
loom deploy adapter:<run-id>       →  OpenAI-compatible endpoint (serving.md)
```

The renter never wires these together; declaring the recipe wires them. `loom train` returns a checkpoint ref, the platform launches the recipe's declared eval suite against the final checkpoint automatically, and the resulting adapter is what `loom deploy` takes. The three commands are one narrative in [../product/deployment.md](../product/deployment.md) §3(b).

**The lineage record** is the artifact that makes runs reproducible and model cards honest. It extends the lightweight provenance triple from [data.md](./data.md) §5 (`dataset manifest hash + image digest + base-model hash → checkpoint hash`) with the recipe ref, seed, and eval report — everything needed to answer "what data, what recipe, how good, and can I reproduce it." It is stamped into the job ledger and travels with the model card on HF push ([training.md](./training.md) §6, [evaluation.md](./evaluation.md) §7):

```json
{
  "checkpoint": "ckpt@a17e9f",
  "recipe": "qlora-sft@3",
  "image_digest": "sha256:d91f4c…",
  "base_model": { "ref": "meta-llama/Llama-3.1-8B", "revision": "0e9e39f" },
  "dataset": { "ref": "my-sft@v2", "manifest_hash": "sha256:9f3c…" },
  "config_resolved": { "seq_len": 2048, "lora": { "rank": 16, "alpha": 32 }, "…": "…" },
  "seed": 42,
  "eval_report_hash": "sha256:7b1a…",
  "produced_at": "2026-07-07T14:41:03Z"
}
```

Because the recipe is immutable and every input is content-addressed, the record is a reproduction key: same recipe ref + same manifest hashes + same seed reproduces the run up to the hardware-determinism floor ([training.md](./training.md) §8, [environments.md](./environments.md) §9 — bitwise identity is *not* promised across GPU archs, and a requeue can cross an arch boundary; the record captures the seed so trajectories stay seed-controlled and statistically equivalent).

---

## 5. Cost estimation contract

Every recipe carries a pure estimator that runs before a cent is spent. Its inputs and its honesty policy are fixed by contract.

**Inputs → output.** `(token_count, method, model_size, gpu_class) → GPU-hour range → dollar range`. Token count comes from the resolved dataset manifest ([data.md](./data.md)); each method (QLoRA/LoRA/full-FT/DPO/GRPO/…) has a throughput profile per node class from the standing benchmark harness ([training.md](./training.md) §7). The output is always a **range**, never a point estimate, because real throughput depends on sequence-length distribution, batch, checkpointing, kernel availability (Liger/FA-2), and interruption rate.

`loom train --dry-run` prints the plan and the estimate and exits `0` without spending ([../product/deployment.md](../product/deployment.md) §4):

```
$ loom train --recipe qlora-sft@3 \
    --base meta-llama/Llama-3.1-8B --data my-sft@v2 \
    --gpu rtx4090 --epochs 3 --dry-run

Recipe        qlora-sft@3   (image sha256:d91f4c…)
Base model    meta-llama/Llama-3.1-8B  (8B params, QLoRA nf4-double)
Dataset       my-sft@v2   41,207 examples · ~34M tokens (3 epochs)
Placement     1× rtx4090 (24GB) · est. peak VRAM ~13.5/24 GB  ✓ fits
Eval          instruction-following (runs automatically after train)

Estimate      6 – 11 GPU-hr   →   $2.05 – $3.75   (modeled, not yet fleet-measured)
Cap           --max-price not set. Recommend setting one.

Dry run only — nothing scheduled. Re-run without --dry-run to proceed.
```

**Honest-variance policy.** Ranges are labeled `modeled` until the benchmark harness has measured that (recipe × node-class) cell, then `measured` with a variance band ([training.md](./training.md) §7). We would rather show a wide honest band than a precise wrong number.

**The `max_price` cap is always enforced.** Independent of the estimate, a renter sets `--max-price` (a hard dollar cap, same budget-reservation mechanism as every Loom job, [data.md](./data.md) §3). When accrued spend reaches the cap, the job **does not get silently killed mid-step**: it captures a checkpoint (`loom-ckpt`, [training.md](./training.md) §3) and **pauses at that checkpoint**, then the renter chooses — top up and resume from the exact step, or stop and keep the partial adapter + everything trained so far. This makes the cap a safety rail, not a data-loss cliff. (Estimate-vs-actual drift tolerance — do we cap liability at estimate × 1.25? — is an open product question, [../product/deployment.md](../product/deployment.md) §10.)

---

## 6. Recipe QA & publishing

**Loom-authored at v1.** All v1 recipes are authored, tested, and signed by Loom. Community-contributed recipes are an **open question** (§8) — the QA and trust bar for a third-party recipe (which pins an image, sets defaults, and quotes a cost estimate people rely on) is real and not yet designed.

**CI smoke-test on every release.** Each recipe release is smoke-tested on **every supported GPU class** ([../product/deployment.md](../product/deployment.md) §9 support matrix) before it publishes: a nightly canary train of a tiny model through the full recipe path — train → checkpoint → resume-after-injected-eject → terminal eval → emit all four outputs. This validates that the pinned image digest, the schema defaults, the resource-claim function, and the estimator all still hold on real silicon, not just in a mock. A recipe that OOMs, diverges, or fails to produce its output contract on any supported class does not publish.

**These canaries double as fleet canary jobs.** A tiny-model recipe smoke-test is indistinguishable to a host from a real job, so the same nightly runs serve as the **result-integrity canaries** in [../platform/security.md](../platform/security.md) §3.5 — jobs whose correct output is operator-known, used to catch a host that fabricates results. One mechanism, two jobs: recipe QA and fleet trust.

**Changelog discipline.** Every version bump ships a changelog entry stating exactly what changed (image digest, a widened bound, a new default, an estimator recalibration) and whether it is behavior-affecting. Because refs are immutable, a renter on `qlora-sft@3` is never surprised; they read the `@4` changelog and choose when to move. Deprecation follows the image EOL calendar ([environments.md](./environments.md) §2.2) — a recipe whose pinned image ages out gets a deprecation notice one release ahead, and pinned reruns still resolve by digest while any host can meet the driver floor.

---

## 7. Worked end-to-end example

A renter has a cleaned instruction corpus and wants an adapter serving behind an OpenAI-compatible endpoint. The full transcript, consistent with [../product/deployment.md](../product/deployment.md)'s narrative style:

```bash
# 1. Push the dataset — chunked, content-addressed, immutable manifest (data.md).
$ loom data push ./sft_data.jsonl --name my-sft:v2
  scanning ./sft_data.jsonl ..... 41,207 examples, 47 MB
  chunking + hashing ............ 52 chunks (0 already in store)
  uploading 52 new chunks ....... done
  manifest: my-sft@v2  sha256:9f3c… (immutable)

# 2. Dry-run the fine-tune to see the plan and cost before committing.
$ loom train --recipe qlora-sft@3 \
    --base meta-llama/Llama-3.1-8B --data my-sft@v2 \
    --gpu rtx4090 --epochs 3 --lora-r 16 --max-price 5.00 --dry-run

  Recipe     qlora-sft@3   (image sha256:d91f4c…)
  Base       meta-llama/Llama-3.1-8B  (8B, QLoRA nf4-double)
  Dataset    my-sft@v2  ~34M tokens (3 epochs)
  Placement  1× rtx4090 (24GB) · est. peak VRAM ~13.5/24 GB  ✓
  Eval       instruction-following (auto, after train)
  Estimate   6 – 11 GPU-hr  →  $2.05 – $3.75  (modeled)   cap $5.00 ✓
  Dry run only — nothing scheduled.

# 3. Run it for real. Resumable-by-default; streams loss + accrued cost.
$ loom train --recipe qlora-sft@3 \
    --base meta-llama/Llama-3.1-8B --data my-sft@v2 \
    --gpu rtx4090 --epochs 3 --lora-r 16 --max-price 5.00 --yes

  [prepare]  node den-4b1c selected · my-sft@v2 warm from peer  (CPU-rate)
  [prepare]  image sha256:d91f4c… resident · manifest validated  ✔
  [run]      GPU meter START — QLoRA · micro_batch=8 (auto) · grad_ckpt on
  [step 200] loss 1.412 · ckpt@… saved · $0.31 · 00:18 elapsed
  [step 3600] loss 0.887 · $1.94 · 01:52 elapsed
  [14:22:07] ⚠ node lost (host ejected — owner reclaimed machine)
  [14:22:09] ↻ resuming from checkpoint step 3600 on new node (rtx4090, us-east)
  [14:22:44]   resumed at exact step (RNG restored). ETA +14 min. no action needed.
  [step 6100] loss 0.731 · $3.12 · 02:41 elapsed
  [done]     checkpoint ckpt@a17e9f  ·  adapter (74 MB) + model card
  [eval]     instruction-following → report ev@7b1a (score 0.68)  ✔
  [lineage]  my-sft@v2 + qlora-sft@3 + loom/train@sha256:d91f… + base:llama-3.1-8b → ckpt@a17e9f

# 4. Deploy the adapter on a shared base — cheapest dedicated endpoint (serving.md).
$ loom deploy adapter:a17e9f --name my-model
  adapter placed on base-model replicas (llama-3.1-8b) · scale-to-zero
  → https://inference.loom.dev/v1   (model = "my-model")

# 5. First call — one line different from OpenAI.
$ curl https://inference.loom.dev/v1/chat/completions \
    -H "Authorization: Bearer loom_sk_..." \
    -d '{"model":"my-model","messages":[{"role":"user","content":"hi"}]}'
  {"choices":[{"message":{"role":"assistant","content":"Hello! …"}}], …}
```

The owner-eject at step 3600 is a footnote, not a disaster: `loom-ckpt` had captured the exact step and RNG state ([training.md](./training.md) §3), the requeue re-ran only the cheap CPU-rate prepare phase against a warm cache ([data.md](./data.md) §4), and the renter is not billed for re-running already-checkpointed work ([../product/deployment.md](../product/deployment.md) §6). The eval ran automatically, the lineage record was stamped, and the adapter deployed with a one-line client change.

---

## 8. Open questions

1. **Community recipes.** v1 is Loom-authored only. What is the QA/trust/signing bar for a third-party recipe that pins an image, sets defaults, and quotes a cost estimate others rely on? Does it get a separate namespace and a "modeled by author, not Loom-measured" cost label?
2. **Estimator calibration cadence.** Estimates start `modeled` and become `measured` once the benchmark harness covers a (recipe × node-class) cell. How often do we recalibrate as the fleet composition shifts, and does a recalibration that moves the range materially warrant a version bump (so pinned reruns keep the old estimate) or a silent metadata update?
3. **34B QLoRA as a first-class `qlora-sft` target.** [training.md](./training.md) §9 flags 34B on 24GB as "fits with care." Do we expose it through `qlora-sft` with conservative auto-batch defaults, or gate it behind a separate `qlora-sft-large` recipe that only claims bigger nodes, to avoid a wave of OOM retries?
4. **OOM-retry bound tuning.** The default is halve-micro-batch, max 2 attempts, floor micro_batch=1. Is 2 the right cap across recipes, or should full-FT (where a retry is expensive) get a lower bound and single-GPU QLoRA a higher one?
5. **Diffusion/ASR eval defaults.** LLM recipes get a suite ref by default; `diffusion-lora` and `whisper-ft` emit scalar metrics (FID/CLIP, WER/CER) that need a reference set to be meaningful. Do we ship default reference sets per recipe, or require the renter to supply one and skip auto-eval when absent?
6. **Merged-model output for adapters.** `qlora-sft` outputs an adapter by default (cheapest to serve). Do we offer a one-flag "merge on completion" for renters who want a standalone model to push to HF, accepting the larger checkpoint upload cost ([training.md](./training.md) §3)?

---

*Cross-references: [training.md](./training.md) (stack, VRAM table, `loom-ckpt`, determinism, escape hatch) · [data.md](./data.md) (dataset manifests, lineage triple, prepare-phase billing) · [evaluation.md](./evaluation.md) (terminal eval node, suites, judge config, signed reports, scalar metrics) · [environments.md](./environments.md) (image catalog, digest pinning, EOL calendar) · [serving.md](./serving.md) (adapter deploy, diffusion/whisper engines) · [../product/deployment.md](../product/deployment.md) (CLI narrative, dry-run, failure UX) · [../platform/security.md](../platform/security.md) (canary jobs).*
