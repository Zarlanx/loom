# Data on Loom

This document covers the first stage of Loom's managed ML lifecycle: how renters **collect, process, version, and stage** data before it feeds [training](training.md), [evaluation](evaluation.md), or [synthetic-data loops](#3-synthetic-data-generation-as-a-first-class-flow). It is written against the same hard constraint as everything else in Loom — supply is *other people's consumer machines* behind residential NAT that vanish without notice (see the [architecture overview](../architecture/overview.md)) — and it resists the temptation to reach for a distributed cluster engine before the workload actually demands one.

The short version: **the vast majority of data work on Loom fits on one rented box, and we design for that first.** Distributed data processing carries the same WAN-bandwidth problem as distributed training and is the exception, not the default. Apache Spark is *supported* for practitioners who bring existing pipelines, but it is deliberately not the platform default.

## 1. What data work actually happens here

Before choosing engines, be honest about the workloads. Loom's renters are ML practitioners fine-tuning and evaluating models, not petabyte-scale data-lake operators. The recurring tasks:

- **Pulling datasets from the Hugging Face Hub** — the single most common first step. `load_dataset("some/corpus")`, a revision pin, a split. Handled through Loom's HF cache proxy (the sandbox is default-deny egress with an allowlisted HF mirror — see [networking](../platform/networking.md)).
- **Cleaning, dedup, and filtering text corpora** — language-ID filtering, boilerplate/HTML stripping, near-duplicate removal (MinHash/LSH), quality heuristics, PII scrubbing, length and perplexity filters.
- **Tokenization** — running a tokenizer over a corpus to produce packed token sequences; embarrassingly parallel, CPU-bound, trivially chunked.
- **Image preprocessing / augmentation** — decode, resize, crop, format-convert, sometimes GPU-accelerated (DALI/`torchvision`); the multimodal case where a GPU in the data stage genuinely helps.
- **Building evaluation sets** — curating held-out splits, formatting into eval-harness schemas, sanity-deduping against training data to avoid contamination.
- **Synthetic data generation** — generating instruction/preference data by *calling Loom's own serverless inference endpoints* (see §3). A differentiating loop.
- **Embeddings generation** — running an embedding model over a corpus for retrieval, clustering, or semantic dedup. GPU-accelerated batch inference over a dataset — structurally identical to eval batch inference.

**Scale reality — the load-bearing fact.** Most fine-tuning datasets are **megabytes to a few gigabytes**. A 50k-example SFT set is tens of MB of JSONL. A LoRA run's data fits in RAM on a laptop. Pretraining-scale corpora (hundreds of GB to TB) exist but are the **exception** on this platform — they belong to a small minority of users, and even they are usually doing *single-node scale-up* on one beefy box rather than genuine scale-out. Designing the data stack around the TB tail while the MB-GB body is 90% of jobs would be résumé-driven architecture. We size for the body and provide an honest escape hatch for the tail.

## 2. Right-sized engine recommendations

This is the section to get right. The rule: **start single-node, and only reach for a distributed engine when the data genuinely does not fit and cannot be streamed on the biggest box you can rent.**

### Single-node first — covers ~90% of jobs

Three tools cover almost everything, and they run beautifully inside a single Loom sandbox on one rented machine:

- **Hugging Face `datasets`** — the default for anything that starts life on the Hub. It is Arrow-backed and **memory-mapped from disk, not loaded into RAM**, so a dataset far larger than device memory iterates fine at 1–3 Gbit/s on ordinary hardware, and `.map()` parallelizes across processes over the shared mmap ([HF Datasets + Arrow](https://huggingface.co/docs/datasets/en/about_arrow)). It also supports **streaming** mode for lazy, out-of-core iteration without ever fully downloading ([loading docs](https://huggingface.co/docs/datasets/en/loading)). This is the workhorse for text corpora and tokenization.
- **Polars** — the default for tabular ETL, filtering, joins, group-bys, and dedup. Its 2026 **streaming engine** (`polars-stream`) is a morsel-driven, pull-based pipeline with **spillable sinks** for joins, group-bys, and sorts, so larger-than-RAM operations spill to disk efficiently — reportedly ~3× faster than the in-memory engine on big inputs with far better memory behavior ([Polars streaming](https://docs.pola.rs/user-guide/concepts/streaming/), [Dec-2025 roundup](https://pola.rs/posts/polars-in-aggregate-dec25/)). Multithreaded, no JVM, no cluster.
- **DuckDB** — the default when you want to think in SQL over Parquet/CSV/Arrow. Its out-of-core engine spills to disk for larger-than-memory queries; v1.5.2 (April 2026) is production, with the DuckLake extension now stable ([DuckDB OOM guide](https://duckdb.org/docs/current/guides/troubleshooting/oom_errors)). Ideal for building/joining eval sets and ad-hoc corpus analytics. *Caveat worth knowing:* a few blocking aggregates (`list()`, `string_agg()`) can still OOM because they don't offload to disk.

A single rented Loom box can carry tens of CPU cores and 128–256 GB RAM plus fast local NVMe scratch. With mmap (`datasets`) or spill-to-disk (Polars/DuckDB), **datasets well into the hundreds of GB are tractable single-node.** Scale *up* before you scale *out*.

### Distributed — only when genuinely needed

When a job truly outgrows one box — TB-scale dedup, multimodal preprocessing over tens of millions of images — the recommended distributed layer is **Ray Data**.

Ray Data is the right distributed choice for Loom for three reasons: it is **GPU-aware** (it schedules GPU actors for model-in-the-loop stages like embeddings/dedup and pipelines CPU decode with GPU compute in one job), it is **Pythonic** (no JVM, same runtime as [training](training.md) and [serving](serving.md), so it drops into curated images cleanly), and it fits **per-node deployment** far better than a JVM cluster. In 2026 Ray Data reports 3–8× the throughput of Spark/Flink on these workloads and, with cuDF/Blackwell GPU acceleration, an ~80% cost cut versus CPU-only pipelines on large-scale dedup ([Ray Data docs](https://docs.ray.io/en/latest/data/data.html), [Anyscale + NVIDIA dedup](https://www.hpcwire.com/bigdatawire/this-just-in/anyscale-cuts-multimodal-ai-data-processing-costs-by-80-with-nvidia-rtx-pro-4500-blackwell/)). For **multimodal** pipelines specifically, **Daft** (which ships a distributed runner as a Ray executor and streams via its Swordfish push-based engine) is a strong sibling and is offered in the same curated image ([Daft](https://www.daft.ai/), [Ray Data vs Daft benchmark](https://www.anyscale.com/blog/ray-data-daft-benchmarking-multimodal-ai-workloads)).

**The WAN caveat — read this before scaling out.** Distributed data processing across residential Loom nodes hits *exactly* the same wall as multi-node WAN training: shuffles, joins, and repartitions move bulk data between nodes, and residential uplinks are slow and jittery. A cross-node group-by or global dedup is a WAN shuffle, and Loom's own networking design says bulk data moves via object store + content-addressed cache + P2P precisely because node-to-node residential transfer is the bottleneck ([networking](../platform/networking.md)). So the realistic distributed pattern on Loom is **not** scatter-across-strangers. It is:

1. **Scale up** — rent one beefy multi-core CPU + GPU node and run Ray Data locally on it (Ray runs single-node too; you get its GPU-aware pipelining without any WAN shuffle). This is the recommended path for the large tail.
2. **Scale out only within a single multi-GPU host** — a LAN-local Ray cluster confined to one rig with multiple GPUs, where interconnect is PCIe/NVLink, not the public internet. Same rule as training: the sweet spot is one node or one multi-GPU box, never WAN fan-out.

We do **not** offer multi-node WAN data clusters across the marketplace. If your dedup truly needs a hundred nodes shuffling terabytes, Loom is the wrong tool for that stage — do it on a hyperscaler and push the cleaned Parquet to Loom for training.

### Apache Spark — supported, not default

Spark is a **curated image**, offered so practitioners can **bring existing Spark pipelines** unchanged. With the **RAPIDS Accelerator for Apache Spark** (2026 line, e.g. `26.02.x`, GPU-accelerated with no code change via a plugin jar + one config flag, supporting through Blackwell) a Spark job can run GPU-accelerated on a single rented multi-GPU node ([spark-rapids](https://nvidia.github.io/spark-rapids/), [download/versions](https://nvidia.github.io/spark-rapids/docs/download.html)). That combination — **single-node Spark + RAPIDS** — is the legitimate use: a team with battle-tested Spark ETL who don't want to rewrite it, running it on one beefy box.

But Spark is deliberately **not the platform default**, for reasons that are structural, not fashion:

- **JVM weight.** A JVM cluster (driver + executors, heap tuning, serialization overhead) is heavy to stand up inside an ephemeral consumer sandbox versus a Python process that starts instantly.
- **Cluster assumptions.** Spark's model is a managed cluster of reachable executors on a fast network — the same assumption Kubernetes makes and the same one Loom's architecture explicitly rejects for a fleet of NAT'd strangers ([overview: why not Kubernetes](../architecture/overview.md)). Multi-node Spark over residential WAN would drown in shuffle traffic.
- **Poor fit for scattered nodes.** The whole value proposition of Spark is horizontal scale-out across many machines — precisely the thing Loom's residential-bandwidth reality makes impractical. On a single node, lighter engines (Polars/DuckDB/Ray Data) usually match or beat it with a fraction of the operational weight; 2026 head-to-heads put Polars ahead of Spark for the single-node bracket Loom lives in ([Spark vs Polars 2026](https://sparkingscala.com/latest/2026/05/22/spark-vs-polars-2026/)).

So: Spark is a compatibility on-ramp for existing pipelines on a single (ideally multi-GPU, RAPIDS-accelerated) node — welcome, documented, but never what a new Loom user should reach for first.

### Comparison table

| Engine | Runs where | Best for | GPU | Scale ceiling on Loom | When to use |
|---|---|---|---|---|---|
| **HF `datasets`** | Single node | Hub datasets, text corpora, tokenization | No | Hundreds of GB (mmap + streaming) | **Default** for anything from the Hub |
| **Polars** | Single node (Cloud = distributed, external) | Tabular ETL, filter/join/group-by, dedup | No | Larger-than-RAM via spill | **Default** for tabular processing |
| **DuckDB** | Single node (in-process) | SQL over Parquet/Arrow, eval-set joins, analytics | No | Larger-than-RAM via spill | **Default** when you want SQL |
| **Ray Data** | Single node → LAN multi-GPU | Model-in-the-loop (embeddings, GPU dedup, image preproc), multimodal | **Yes** | One node / one multi-GPU host — **no WAN fan-out** | Data too big for one core-set *and* needs GPU/parallel model calls |
| **Daft** | Single node → Ray executor | Multimodal (image/audio/video) at scale | **Yes** | Same as Ray Data | Multimodal-heavy pipelines |
| **Apache Spark (+RAPIDS)** | Single node (RAPIDS-accelerated) | **Existing** Spark pipelines brought as-is | Yes (RAPIDS) | Single node only on Loom | You already have Spark code and won't rewrite it |

Decision heuristic: **From the Hub? → `datasets`. Tabular? → Polars or DuckDB. Needs a GPU model in the loop or multimodal at scale? → Ray Data/Daft on one big node. Already have Spark? → Spark+RAPIDS on one big node. Anything else? → you're overthinking it; use the single-node core.**

## 3. Synthetic data generation as a first-class flow

This is Loom's differentiating loop: **rent inference to build data, then fine-tune on rented GPUs.** Because Loom already runs an OpenAI-compatible [serverless inference gateway](serving.md), a renter can generate training data against strong open models on the network and immediately train on it — one platform, one bill, closing the loop.

**The pattern** (Self-Instruct / Evol-Instruct / distillation, the 2026 standard recipe — small real seed → teacher model → structured generation → judge filter → JSONL into [TRL/Unsloth](training.md) — [PremAI 2026 guide](https://blog.premai.io/how-to-generate-synthetic-training-data-for-llm-fine-tuning-2026-guide/), [FutureAGI stack](https://futureagi.com/blog/synthetic-data-fine-tuning-llms/)):

1. **Seed** — the renter supplies a small set of real examples/instructions.
2. **Generate** — a Loom job calls the [serverless endpoint](serving.md) with a teacher model to expand seeds (instruction synthesis, response generation, preference-pair generation). Runs as a normal batch job in a sandbox, hitting the gateway.
3. **Filter** — the single most important step. **An unfiltered synthetic set is worse than a smaller filtered one** ([PremAI](https://blog.premai.io/how-to-generate-synthetic-training-data-for-llm-fine-tuning-2026-guide/)). Loom's default recipe: **dedup** (near-duplicate instruction removal via MinHash), **quality scoring** (a judge model rating relevance/correctness — again a gateway inference call), length/format validation, and toxicity/PII filtering. `distilabel` is the recommended curated tool for reproducible, auditable generate-and-judge pipelines.
4. **Version** — the filtered output is pushed as a normal Loom dataset (§4–5), so its provenance (teacher model, prompts, filter config) is captured in lineage automatically.

**Rate and cost controls — mandatory, not optional.** A generation loop that calls inference in a tight loop can burn budget fast. Loom applies the same `max price` budget reservation used for compute jobs (see the [overview](../architecture/overview.md)) to synthetic-data jobs, plus per-job token-rate caps and a hard generation ceiling. The renter sets a spend cap; the job aborts cleanly and checkpoints partial output when it's hit. Because both the generation *and* the eventual training run bill through Loom, the renter sees a single end-to-end cost for "build the dataset and fine-tune on it" — the whole point of the loop.

## 4. Data staging architecture

Getting data *onto* the right node cheaply and verifiably is where Loom's networking reality drives the design.

**Sources.** A renter either:
- **Uploads to Loom object store** — S3-compatible, via **presigned URLs** (the CLI chunks and uploads directly; the control plane never proxies bytes). Same object store used for [checkpoints](training.md).
- **References external data** — an external S3/GCS bucket (scoped, short-lived creds) or an HF dataset revision. Loom pulls it through the allowlisted [cache proxy](../platform/networking.md) rather than copying it into Loom storage up front.

**Content-addressed chunking.** All data is split into content-addressed chunks (hash = identity). Identical chunks are stored once and, crucially, **fetched peer-to-peer between nodes** the same way [model weights](../architecture/overview.md) are — a chunk already warm on a nearby node comes from that node, not a re-download from origin. This is the whole reason bulk data doesn't crawl over residential uplinks.

**Prefetch before the job starts — and the billing decision that follows.** The node-local **cache manager** prefetches a job's dataset chunks *before* the GPU workload launches. This motivates an explicit design decision:

> **The prepare phase (download + cache-fill + manifest validation) is billed at a cheaper CPU rate, not GPU-seconds.** A renter must never pay premium GPU time while a box is merely downloading data. The GPU meter starts only when the training/processing workload actually begins on the GPU.

This makes Loom's flaky-node economics honest: a requeue onto a fresh node re-runs the prepare phase, but if chunks are already warm in the P2P cache the prefetch is near-instant, and even a cold prefetch is charged at CPU rates.

**Dataset manifests (hash tree).** Every staged dataset produces a **manifest** — a Merkle-style hash tree over its chunks plus metadata (schema, row count, split, source revision). The manifest is what a job actually references. On a **re-run or a resumed/requeued job**, the node revalidates cheaply: compare manifest hashes against what's in cache, fetch only the missing chunks, skip everything already present. Revalidation is a hash comparison, not a re-download — resumption after a node vanishes costs a manifest check, not a full restage.

## 5. Versioning and lineage

**Built-in manifests are the default, and they are zero-config.** Every `loom data push` yields an **immutable, hash-addressed manifest** — `corpus@v2` resolves to a specific hash tree that can never change under you. Immutability plus content-addressing means a dataset version is reproducible by construction: same manifest hash ⇒ byte-identical data.

**Lightweight lineage, recorded automatically.** Loom captures the minimal provenance triple in job metadata with no user effort:

```
dataset manifest hash  +  code/image digest  +  base-model hash  →  output checkpoint hash
```

Every training or processing job stamps this into its record, so any checkpoint can be traced back to the exact data version, image, and base model that produced it — the foundation for reproducibility and for the [evaluation](evaluation.md) story. This is *lightweight* by design: a provenance edge in the job ledger (Postgres, per the [control plane](../platform/control-plane.md)), not a heavyweight metadata store.

**External integrations, for teams that bring them.**
- **HF dataset revisions** map naturally onto Loom manifests — pin `some/corpus@<revision>` and Loom records that revision in lineage; the immutable-manifest model mirrors HF's revision model.
- **DVC / lakeFS** for teams that already version data this way. As of late 2025 **lakeFS acquired DVC**; they now operate as complementary tiers under one project — **DVC** for individual/single-project data-scientist workflows, **lakeFS** for petabyte-scale multimodal data lakes ([lakeFS acquires DVC](https://lakefs.io/media-mentions/lakefs-acquires-dvc-uniting-data-version-control-pioneers/)). Loom treats these as **bring-your-own**: a DVC- or lakeFS-tracked remote can be referenced as an external source (§4), and its version identifier is recorded in lineage. We do **not** require them — built-in manifests cover the default case with zero setup, and BYO versioning is for teams with an established practice.

## 6. Privacy and policy

Datasets on Loom live on **other people's machines**, and we are honest about what that means.

**The core fact:** on consumer-tier nodes (Tier A/B), consumer GPUs have no TEE silicon, so **data is visible to the host in principle** — in-use encryption against a malicious host is impossible on this hardware. Loom does not pretend otherwise. See [security](../platform/security.md) for the full threat model.

**Guidance, stated plainly:**
- **Public, synthetic, and licensed-for-redistribution data → consumer tier (A/B).** This is the overwhelming majority of Loom data work — HF corpora, self-generated synthetic sets, open datasets — and it belongs on the cheap consumer tier.
- **Sensitive/proprietary/regulated data → Tier C (confidential/attested) or don't put it on Loom at all.** The future [confidential tier](../platform/security.md) (SEV-SNP/TDX CPUs + confidential-computing GPUs, attestation-gated key release) is the real home for sensitive data. Until and unless a workload runs there, sensitive data should not touch a consumer node. This is a hard guidance line, not a soft suggestion.

**What Loom guarantees regardless of tier:**
- **Encrypted at rest** in the object store, and **encrypted in transit always** (all node transfers ride the end-to-end-encrypted relay/WireGuard data plane — [networking](../platform/networking.md); relays see only ciphertext).
- **Ephemeral on-node execution** — scratch is `tmpfs`, nothing renter-owned is written in plaintext to host disk, and scratch is scrubbed between tenants (per the [host agent](../platform/host-agent.md) design).
- **Retention / deletion API** — `loom data rm <manifest>` deletes the renter's chunks from Loom object store and issues cache-invalidation to any node holding them; content-addressing means a chunk shared with another live dataset is refcounted, not orphaned. Deletion is auditable.

## 7. Worked example

A renter has a cleaned instruction corpus locally and wants to fine-tune on it.

```bash
# Push the corpus. The CLI chunks, content-addresses, uploads via presigned
# URLs, and builds an immutable manifest. Nothing is re-uploaded that the
# store already has (dedup by chunk hash).
$ loom data push ./corpus
  scanning ./corpus ............. 41,207 examples, 1.9 GB
  chunking + hashing ............ 486 chunks (312 already in store, skipped)
  uploading 174 new chunks ...... done
  manifest: corpus@v2  sha256:9f3c… (immutable)

# Launch a fine-tune referencing the dataset by version.
$ loom run --image train:latest --data corpus@v2 --gpu 1x4090 -- \
      python finetune.py --epochs 3

  [prepare]  node nyc-7f3a selected
  [prepare]  prefetching corpus@v2 → 174 chunks cold, 312 warm from peer  (CPU-rate billing)
  [prepare]  manifest validated (Merkle root matches)          ✔
  [run]      GPU meter START — training…
  ...
  [done]     checkpoint ckpt@a17e  |  lineage: corpus@v2 + train:latest@d91f + base:llama-3-8b → ckpt@a17e
```

**Re-run on the cache hit.** The renter tweaks a hyperparameter and reruns the same job:

```bash
$ loom run --image train:latest --data corpus@v2 --gpu 1x4090 -- \
      python finetune.py --epochs 4

  [prepare]  node nyc-7f3a selected (or any peer with chunks warm)
  [prepare]  manifest corpus@v2 already resident — 486/486 chunks warm  ✔
  [prepare]  prepare phase: 0.4s, $0.00                         ← cache hit, no GPU billed
  [run]      GPU meter START — training…
```

The manifest revalidation is a hash check, the chunks are already warm from the P2P cache, and no GPU-seconds were burned to get the data in place. If the first run's node had vanished mid-training, requeue onto a fresh node would re-run only the (cheap, CPU-rate, mostly-cache-warm) prepare phase before resuming from the last [checkpoint](training.md).

## 8. Open questions

- **Prepare-phase billing gaming.** CPU-rate prepare is renter-friendly, but could a workload smuggle real computation into the "prepare" window to dodge GPU rates? Where exactly is the meter boundary, and how do we detect GPU utilization during a nominally-CPU prepare phase?
- **P2P cache privacy vs. efficiency.** Content-addressed P2P chunk sharing is what makes staging fast, but it means a chunk of renter A's (public) dataset can be served from renter B's node. That's fine for public/synthetic data — is it *ever* acceptable for anything else, and how do we hard-partition Tier-C data out of the shared P2P cache entirely?
- **Single-node ceiling for the tail.** We claim hundreds of GB is tractable single-node. Where does that actually break — at what dataset size / operation does spill-to-disk thrash badly enough that the LAN-multi-GPU Ray path becomes mandatory, and can we detect and recommend it automatically?
- **Ray Data vs. Daft default for multimodal.** Both are offered; both are strong. Should Loom pick one as *the* multimodal default in curated images, or keep both and let the workload decide? Daft is still 0.x — how much does that maturity gap matter for a platform default? *(Daft maturity/version status is a moving target — verify at image-curation time.)*
- **Synthetic-data cost transparency.** The generate→filter→train loop can silently rack up inference spend in the filter (judge) step. Is a single `max price` cap enough, or do renters need a per-stage cost breakdown before they commit the run?
- **BYO versioning depth.** We reference DVC/lakeFS as external sources and record their IDs in lineage — but do teams want *deeper* integration (e.g. Loom manifests exportable as lakeFS commits), or is reference-and-record sufficient?

---

*Cross-references: [architecture overview](../architecture/overview.md) · [training](training.md) · [evaluation](evaluation.md) · [serving](serving.md) · [networking](../platform/networking.md) · [security](../platform/security.md) · [host agent](../platform/host-agent.md) · [control plane](../platform/control-plane.md)*
