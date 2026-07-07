# 0009 — vLLM as primary inference engine; whole-model-per-node; keep-warm

**Status:** Accepted — 2026-07-07

## Context

Serverless inference runs on a maximally heterogeneous fleet of consumer GPUs (24–32 GB, whole-card only) behind residential NAT that die a lot. Three coupled choices follow from that: which inference engine, how a model maps onto nodes, and how to handle cold starts. The fleet's heterogeneity and churn are the deciding constraints — any design that assumes homogeneous hardware or stable nodes is wrong here.

## Decision

- **vLLM is the primary engine** ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §2): mature OpenAI-compatible server, PagedAttention + continuous batching, and now first-class ROCm — one engine covers the NVIDIA-first / ROCm-fast-follow fleet. SGLang is a per-endpoint alternate for measurably prefix-heavy traffic; llama.cpp and ONNX/diffusers/whisper are narrow fast-follows behind the same gateway.
- **TensorRT-LLM is rejected as the default.** Its ahead-of-time build produces an engine binary specific to one GPU + dtype, and **engines are not portable across GPUs** — on our heterogeneous fleet we'd cache a distinct engine per (model, GPU SKU, dtype), fighting whole-model-per-node fungibility ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §2; [`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md) §4). It is a serving-side opt-in for high-QPS pinned deployments only.
- **Whole-model-per-node** ([`../architecture/overview.md`](../architecture/overview.md); [`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md)): every serving node holds the entire model, so any warm node is a complete failover target with no cross-node coordination on the request path. Dynamo-style disaggregation is rejected — it would make every request depend on multiple flaky residential nodes staying up simultaneously.
- **Keep-warm over scale-to-zero cold starts** ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §4): the cold-start floor is download-dominated and therefore minutes-scale, so scale-to-zero is inherently a minutes-scale cold start. Keep-warm (replicas held resident, a paid feature) is the product answer; scale-to-zero is offered only where the renter accepts the latency.

## Consequences

- One engine, multiple engine images, vLLM as default — the gateway only speaks the OpenAI wire protocol and doesn't care which engine answers.
- Whole-model-per-node makes mid-stream failover clean: the gateway re-dispatches to any warm replica holding the same model ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §3).

**What we give up:**

- Whole-model-per-node **caps servable models at what fits one card** — 7–32 B dense plus small MoEs, quantized; 70 B+ only on a rare LAN multi-GPU host; frontier MoEs out of scope ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §1). No WAN sharding to fit a bigger model.
- Rejecting TensorRT-LLM as default forgoes its per-node latency wins on homogeneous NVIDIA sub-fleets.
- Keep-warm **costs money** (a per-second floor); scale-to-zero saves money but exposes minutes-scale cold starts — we surface this pricing tradeoff honestly rather than hide it (ADR-0012).
- Deterministic seamless failover continuation is only available where the engine guarantees bit-reproducible seeded decoding; the default is restart-from-scratch with an idempotent request ID, and the client may see a regenerated tail ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §3).

## Revisit when

A large homogeneous NVIDIA sub-fleet emerges and per-node latency becomes the bottleneck (reconsider TensorRT-LLM), serverless volume justifies disaggregation's coordination cost, or a "warm-ish" tier (weights resident, engine stopped) proves worth pricing between keep-warm and scale-to-zero ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §9).
