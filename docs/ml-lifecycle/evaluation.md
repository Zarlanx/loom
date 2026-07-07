# Evaluation and experiment tracking

Evaluation is a first-class stage on Loom, not a bolt-on. It sits between [training](./training.md) and [serving](./serving.md) and closes the loop back to [data](./data.md): a training recipe that produces a checkpoint but no eval report is an unfinished job. This document specifies how eval runs on the platform, what stack we ship, how experiments are tracked, and how eval becomes CI for models.

## 1. Why eval is a stage

Three commitments make eval structural rather than optional:

**Every recipe ends with an eval pass by default.** A training job on Loom is a DAG whose terminal node is an eval job. When a run finishes, the platform launches the recipe's declared eval suite against the final checkpoint automatically. The lineage record a training job already carries — data manifest + recipe + base model, see [training.md](./training.md) — is extended with the eval report. There is no such thing as a "trained but never scored" artifact in the catalog; the report is part of what "done" means.

**Deploys can be gated on eval results.** A serverless deployment ([serving.md](./serving.md)) can declare a promotion gate: `min_score` thresholds on named suites, or a "no regression vs. current production" delta check. The control plane refuses to flip traffic to a checkpoint that fails its gate. This is the mechanism that turns eval from a report you read into a control you enforce.

**Cheap inference makes thorough eval affordable — and eval is embarrassingly parallel.** This is the genuine platform advantage, so spell it out. A benchmark suite is thousands of independent prompts. There is no cross-prompt state: MMLU item 4,001 does not depend on item 4,000. That means an N-shard split of any suite runs on N nodes concurrently with near-linear speedup, and Loom's whole reason for existing is a large fleet of cheap consumer GPUs sitting behind a scheduler. A full suite that would take one rented A100 an afternoon can be scattered across forty $0.20/hr consumer nodes and come back in minutes, for roughly the same total GPU-seconds but a fraction of the wall-clock and dollar cost. Where a hyperscaler charges you for a big instance to run evals serially, Loom's fabric was built for exactly this fan-out. Thorough eval — full suites, multiple seeds, judge ensembles — stops being a luxury you skip under deadline pressure and becomes the cheap default.

## 2. The eval stack

Eval jobs are ordinary Loom jobs: a curated eval image, a checkpoint or endpoint to target, a suite spec. The image bundles the engines below. Every engine can target either **(a) a checkpoint served by a local vLLM instance inside the job** (the job pulls weights from the [weight cache](./serving.md), spins vLLM, and hits `localhost`), or **(b) any OpenAI-compatible endpoint** — including a Loom serverless deployment or an external API. Mode (a) is the default for freshly trained checkpoints; mode (b) is how you evaluate something already deployed, or a competitor's hosted model, through the same code path.

**lm-evaluation-harness (EleutherAI)** is our standard-benchmark engine. It is the de-facto standard and the same harness behind the historical Open LLM Leaderboard, with 200+ tasks and active maintenance through 2026 (recent releases added HumanEval Instruct, MBPP Instruct, RULER, LongBench, and GSM8K Platinum). ([EleutherAI releases](https://github.com/EleutherAI/lm-evaluation-harness/releases), [morphllm 2026 guide](https://www.morphllm.com/llm-eval-harness)) It speaks to local models and to OpenAI-compatible servers, so both target modes above are native.

The 2026-relevant standard set, named honestly with contamination caveats:

- **MMLU / MMLU-Pro** — broad knowledge. MMLU is heavily contaminated: Johns Hopkins work found ~29% of MMLU items show contamination signs, and frontier models now cluster at 88–93%, a spread too narrow to survive a prompt-format change. Report MMLU-Pro (harder, less saturated) alongside or instead. ([Pebblous](https://blog.pebblous.ai/blog/llm-benchmark-contamination/en/), [SentryML 2026](https://sentryml.com/posts/llm-benchmarks-2/))
- **GSM8K / GSM8K-Platinum** — grade-school math. Known leakage; the Platinum variant is the cleaner cut.
- **IFEval** — instruction-following, verifiable-constraint style, less gameable than knowledge MCQs.
- **HumanEval / MBPP + EvalPlus** — code (see below).
- **Contamination-resistant / live benchmarks** — LiveCodeBench, LiveBench, MMLU-Pro, FrontierMath. Contamination is permanent for any benchmark older than the model under test, so these post-cutoff, continuously-refreshed suites are where a 2026 score actually discriminates. ([arXiv 2605.19999](https://arxiv.org/pdf/2605.19999)) Loom's suite templates default to including at least one live benchmark and label saturated ones as "reference only."

**lighteval (Hugging Face)** ships as the alternative engine. It started as an extension of lm-eval-harness, draws on HELM, and offers multi-backend support and tight HF Hub integration (results push straight to the Hub) with 1000+ tasks across languages. It can be modestly slower than lm-eval-harness on some tasks (a reported ~2x on arc-challenge in one benchmark). ([lighteval GitHub](https://github.com/huggingface/lighteval), [issue #179](https://github.com/huggingface/lighteval/issues/179)) We offer it because HF-native teams already have configs for it and because its multilingual coverage is broader.

**HELM (Stanford)** we position as a reference methodology and an occasional suite, not the default engine. Its multi-metric, scenario-based framing (accuracy plus calibration, robustness, fairness, efficiency) informs how our report artifact is structured, but its run harness is heavier than most Loom users need day-to-day.

**Code: EvalPlus and bigcode-evaluation-harness.** EvalPlus provides HumanEval+ / MBPP+ — the original problems with far more test cases, which catch overfit solutions the base sets miss. bigcode-evaluation-harness runs 15+ code benchmarks (HumanEval(+), MBPP(+), MultiPL-E across ~18 languages, APPS, DS-1000) and can consume EvalPlus datasets. ([EvalPlus](https://github.com/evalplus/evalplus), [bigcode-evaluation-harness](https://github.com/bigcode-project/bigcode-evaluation-harness)) Code eval executes generated programs, so these run in Loom's hardened sandbox tier (see [platform isolation](../platform/isolation.md)) — untrusted model output is never executed on a bare host.

**Domain packs** are versioned suite bundles (medical QA, legal, function-calling, retrieval-grounded QA) that a team registers once and reuses. A pack is just a named, version-pinned set of tasks plus a scoring config; it is a first-class catalog artifact like a dataset.

## 3. LLM-as-judge evals

For open-ended generation where there is no gold string, Loom supports judge-based eval in two modes: **pairwise comparison** (A vs. B against a reference set, producing win rates) and **rubric scoring** (absolute scores on named criteria). The judge is a strong model reached via Loom inference or an external API using the **renter's own key, sealed** — the key is injected as a sealed secret at job start, used inside the sandbox, and never logged or persisted (see [security](../platform/security.md)).

Judge bias is real and we treat mitigations as defaults, not options. The four biases that appear in every untreated judge pipeline are position, verbosity, self-preference, and authority. ([FutureAGI bias mitigation 2026](https://futureagi.com/blog/evaluating-llm-judge-bias-mitigation-2026/))

- **Position bias** — judges favor the first (or last) option. Mitigation: run every pairwise comparison in both orderings and average; treat order-dependent verdicts as ties. Cost doubles, bias goes near zero. On by default.
- **Self-preference bias** — judges prefer their own family's outputs. Mitigation: never let a model judge its own family; offer a three-family judge ensemble template for high-stakes runs. ([arXiv 2410.21819](https://arxiv.org/pdf/2410.21819))
- **Verbosity bias** — longer answers win regardless of correctness. Mitigation: explicit "do not prefer longer answers" rubric language plus length normalization on aggregate scores.

We also record judge–human agreement (Cohen's kappa) when a labeled reference subset is available, so a judge suite reports its own reliability rather than asserting it.

**Custom eval sets are first-class artifacts**, versioned exactly like datasets and linked to [data.md](./data.md) manifests. A reference set has a content hash, a version, and lineage; an eval report cites the exact set version it ran against. This is what makes judge results comparable over time — "the model improved" only means something if the set it was judged against is pinned.

## 4. Classic ML metrics for non-LLM workloads

Loom is a general compute platform, and eval records **arbitrary scalar metrics**, not just LLM benchmarks. The eval framework's contract is "job emits named metrics → platform records them against lineage," so any workload participates:

- **Classifiers** — accuracy, precision/recall, F1, ROC-AUC, PR-AUC, confusion matrices.
- **Image generation** — FID, CLIP-score, and human-preference proxies.
- **ASR** — WER / CER.
- **Retrieval / ranking** — recall@k, nDCG, MRR.

These come from the user's own eval script; the platform provides the logging client and the comparison UI, not the metric implementations. A tabular XGBoost job and a 70B fine-tune land in the same experiment-tracking surface with the same lineage guarantees.

## 5. Experiment tracking

**The 2026 landscape.** W&B remains the polished managed leader and now includes Weave for LLM tracing/eval in the same UI. MLflow 3.0 (June 2025) pivoted into a unified AI-engineering platform with OpenTelemetry tracing, 50+ eval metrics including LLM-as-judge, and prompt versioning. HF's **Trackio** (launched July 2025) is a sub-1000-LOC, local-first, wandb-drop-in tracker: SQLite locally, Parquet backup to HF Spaces on sync, negligible overhead, but deliberately omits advanced artifact versioning/governance. ([W&B/MLflow comparison](https://futureagi.com/blog/best-weights-and-biases-alternatives-2026/), [Trackio blog](https://huggingface.co/blog/trackio), [Trackio GitHub](https://github.com/gradio-app/trackio))

**Our position, stated honestly: we will not out-build W&B, and we won't try.** Instead:

- **Built-in lightweight tracking is the zero-config default.** Every job records params, metrics (time-series and final), and artifact references to the platform automatically, keyed to the job's lineage. The dashboard has a run-comparison view (overlay curves, diff configs, sort by metric). This is *enough for solo practitioners and small teams* — no account, no setup, works the moment you submit a job.
- **First-class W&B and MLflow integration for teams that live there.** A run declares `tracker: wandb` (or `mlflow`) and the platform injects the API key / tracking URI as a **sealed secret via env-var passthrough**. The user's existing `wandb.log` / `mlflow.log_metric` calls work unchanged; data flows to their workspace. We do not proxy or re-host it.
- **Trackio and TensorBoard** are supported as light options: Trackio for HF-ecosystem users who want its Spaces sync, and TensorBoard via the standard pattern — the job writes a log dir, we sync it to object store, and the dashboard embeds a TensorBoard viewer over it. (Trackio's maturity and its exact API surface should be re-verified before we pin a version — *flagged as fast-moving*.)

The rule: built-in for people who just want their numbers recorded; integrate for people who already have a tracking home.

## 6. Regression testing for models — eval as CI

This is the "ML professionals testing their tools" use case, and it is a first-class product surface. Eval suites act as CI for models:

```
loom eval --suite mysuite --model ckpt@N
```

- **On every training run.** A recipe can declare `eval.suite` and `eval.gate`; the suite runs on the final (and optionally intermediate) checkpoints, and the gate can fail the run or block promotion.
- **On a schedule against a deployed endpoint.** A cron-style trigger runs the suite against a live serverless deployment, so you catch regressions from base-model swaps, quantization changes, or serving-config drift — not just training changes.
- **Drift alarms on serving.** Because scheduled endpoint evals produce a time series of scores, the platform can alarm on score deltas exceeding a threshold (a suite that dropped 4 points week-over-week pages you). This ties serving quality to an objective signal instead of anecdote.

For a founder who cares about ML professionals testing their own tools, this is the wedge: bring your own suite, wire it to `loom eval`, and get quality gates and drift detection on cheap, parallel compute without standing up eval infrastructure yourself.

## 7. Leaderboard and report artifacts

Every eval run yields a **signed report** — the unit of comparability and sharing. It contains:

- **Model lineage** — checkpoint id, base model, data manifest, recipe (pulled from [training.md](./training.md) lineage).
- **Suite version** — the exact pinned task set / domain pack / reference-set versions.
- **Scores** — per-task, with seeds and any judge-agreement stats.
- **Config hash** — engine, prompts, few-shot counts, decoding params, hardware. Two reports are only comparable if their config hashes agree on the axes that matter; the report makes divergence visible rather than hiding it.

The report is **signed** by the platform so a shared score can't be silently edited, and it is **attached to the HF model card on push** (see [serving.md](./serving.md) HF integration). Push a checkpoint to the Hub and its eval report travels with it, so consumers see how it was measured, not just a bare number. Internal leaderboards are just a view over many reports filtered to a shared suite version.

## 8. Cost model of eval on Loom

No invented precise numbers — here is the estimation methodology, which is the point.

**Token math per suite.** Estimate `total_tokens ≈ Σ_items (prompt_tokens + expected_output_tokens)`. MMLU is ~14,000 items; at, say, a few hundred prompt tokens each (few-shot context dominates) plus short answers, a full pass is on the order of low-single-digit *millions* of tokens. Generative suites (code, judge evals) cost more per item because outputs are long; multiple-choice log-likelihood suites cost less because outputs are scored, not generated.

**Wall-clock and dollars.** `gpu_seconds ≈ total_tokens / throughput_tokens_per_sec` for the target model on the target GPU class; `cost ≈ gpu_seconds × node_$_per_sec`. Consumer-GPU throughput for a 7–8B model on modern vLLM is high enough that a single node clears a multi-million-token suite in the tens of minutes to low hours; the dollar figure is `that time × a sub-dollar hourly rate`. (These throughput and rate inputs must be filled from live fleet benchmarks — *flagged as unverified*; the structure is what's asserted, not the constants.)

**Why scatter across cheap nodes makes full suites viable.** Because eval is embarrassingly parallel (§1), sharding across N nodes divides wall-clock by ~N at roughly constant total GPU-seconds. The economic claim is not that Loom eval uses fewer total FLOPs — it's that Loom converts a serial afternoon on one expensive instance into a parallel few minutes on many cheap ones for comparable or lower total dollars. That inversion is what makes running the *whole* suite (plus seeds, plus a judge ensemble) the affordable default instead of the corner you cut.

## 9. Open questions

- **Suite-version drift on live benchmarks.** Contamination-resistant benchmarks refresh continuously, which is good for validity but bad for longitudinal comparability. How do we present a score trend when the underlying suite legitimately changed? Pin-and-snapshot vs. always-latest — probably both, surfaced explicitly in the report.
- **Judge-model cost and neutrality at scale.** Two-ordering + three-family ensembles multiply judge inference cost. When is a single calibrated judge acceptable, and can we auto-recommend based on the measured kappa?
- **Sealed external judge keys.** Renter-supplied API keys for judge models pass through the sandbox; we must guarantee they never appear in reports, logs, or cached prompts. Needs a hard review against [security.md](../platform/security.md).
- **Gate policy expressiveness.** Thresholds and no-regression deltas cover common cases; do we need statistical gates (confidence intervals, seed variance) before a gate is trustworthy enough to block a deploy?
- **Trackio maturity.** Its artifact/governance story is deliberately thin; re-verify before committing it as more than a light option.
- **Report signing and portability.** Signed reports attached to HF model cards are only trustworthy if a third party can verify the signature. What's the key-distribution / verification story off-platform?

---

*Cross-references: [data.md](./data.md) (eval sets as versioned artifacts), [training.md](./training.md) (recipe lineage, terminal eval node), [serving.md](./serving.md) (endpoint targets, promotion gates, HF push), [platform/isolation.md](../platform/isolation.md) (sandboxed code execution), [platform/security.md](../platform/security.md) (sealed secrets).*
