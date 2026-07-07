# Loom

**A distributed GPU compute platform — rent idle GPUs, run ML workloads, serve models.**

Loom weaves consumer and prosumer GPUs scattered across the internet into a single compute fabric. Hosts install a lightweight Rust agent and rent out idle GPU time; renters get managed ML environments for testing, training, and fine-tuning, plus serverless inference endpoints served from the network — at a fraction of hyperscaler prices.

> **Status: design phase.** This repo currently contains the full design documentation. Implementation follows the [roadmap](docs/product/roadmap.md).

## Why

GPU and RAM prices make ML experimentation prohibitively expensive, while millions of capable GPUs sit idle in gaming rigs, workstations, and ex-mining setups. Loom connects the two sides with a marketplace that takes both directions of trust seriously: hosts are protected from renter workloads by tiered sandboxing (containers → gVisor → microVMs), and renters are protected from snooping hosts by structural privacy design (identity-stripping gateway, ephemeral execution, and an attestable TEE tier for sensitive workloads).

## What it does

- **Managed ML environments** — batch and interactive jobs on rented GPUs: PyTorch, Triton, CUDA and ROCm, fine-tuning with Unsloth/TRL, the full modern training stack in curated images.
- **Serverless inference** — an OpenAI-compatible API in front of the network; models served with vLLM from a content-addressed weight cache, with mid-stream failover.
- **Managed ML lifecycle** — data processing, training, evaluation, and serving as first-class, documented workflows with Hugging Face integration throughout.
- **Host agent** — a single static Rust binary, a few MB idle, outbound-only connections, strict sandboxing, per-second metering.

## Design documentation

The full design lives under [`docs/`](docs/README.md):

| Section | Contents |
|---|---|
| [Architecture](docs/architecture/) | System overview, components, job lifecycle |
| [ADRs](docs/adr/) | Twelve Architecture Decision Records — what was decided, why, and what it costs |
| [Platform](docs/platform/) | Host agent, isolation tiers, control plane, networking, security & trust model, wire protocol, renter API |
| [ML lifecycle](docs/ml-lifecycle/) | Data, training, evaluation, serving, runtime environments, recipe catalog |
| [Product](docs/product/) | Deployment & DX, marketplace mechanics, roadmap, unit economics |

## Principles

1. **Both directions of trust are first-class.** Host-from-workload isolation and renter-from-host privacy are designed together, honestly labeled per tier.
2. **Boring where possible, novel only where necessary.** Proven components (Cloud Hypervisor, vLLM, Postgres, WireGuard) glued by a small amount of careful Rust.
3. **Minutes, not days.** A host onboards with one command in under 10 minutes; a renter runs their first job in under 5.
4. **Nodes are cattle.** Consumer machines vanish mid-job; checkpointing, retry, and failover are core paths, not edge cases.
5. **ML-first.** The platform exists to serve ML practitioners — the runtime images, APIs, and workflows are shaped around how models are actually built, evaluated, and served.

## License

Apache-2.0
