# Loom

**A self-hostable GPU compute stack for ML — run the full lifecycle on your own GPUs.**

Loom is the entire ML compute backend as a set of lightweight Rust binaries you deploy yourself: managed training recipes, data staging, evaluation, and OpenAI-compatible serving, running on one GPU box or across your own private fleet. SQLite is embedded, the control plane idles under 100MB, and a stranger can stand it up in minutes. A hosted GPU **marketplace** — renting idle GPUs from other people, with billing, payouts, and reputation — is a future, optional layer built on the same components; its development is deferred until the self-hostable core is proven.

> **Status: design phase, refocused on self-host.** This repo currently contains the full design documentation. Implementation follows the [roadmap](docs/product/roadmap.md); see the [deployment profiles](docs/architecture/profiles.md) for how the same stack collapses onto one box or spreads across your fleet.

## Why

Managed ML platforms lock the training/eval/serving lifecycle behind someone else's control plane and someone else's GPUs. Loom gives you that lifecycle as software you run on hardware you already own — a lab GPU box, a workstation, or a rack of your own rigs — with no external dependency required to do real work. Because the stack is deliberately lightweight (single static binaries, embedded SQLite, no Kubernetes, no Postgres/NATS to operate at the core), self-hosting is a `run one binary` affair, not a platform-engineering project. The optional hosted marketplace layer adds both directions of trust for renting from strangers: hosts protected from renter workloads by tiered sandboxing (containers → gVisor → microVMs), and renters protected from snooping hosts by structural privacy design (identity-stripping gateway, ephemeral execution, and an attestable TEE tier) — but none of that is needed to run Loom on your own GPUs.

## What it does

- **Managed ML lifecycle on your own GPUs** — data processing, training, evaluation, and serving as first-class, documented workflows with Hugging Face integration throughout, running on hardware you control.
- **Managed training recipes** — batch and interactive jobs with checkpoint-resume: PyTorch, Triton, CUDA and ROCm, fine-tuning with Unsloth/TRL, the full modern training stack in curated images.
- **OpenAI-compatible serving** — an OpenAI-compatible API in front of your GPUs; models served with vLLM from a content-addressed weight cache, with mid-stream failover. Ships for self-hosters as an embedded gateway.
- **Single-binary backend** — `loomd` control plane + `hostd` host agent as static Rust binaries, a few MB idle, embedded SQLite, outbound-only connections, strict sandboxing, per-second metering. See [backend engineering](docs/platform/backend.md) and [self-hosting](docs/product/self-host.md).
- **Optional hosted marketplace (deferred)** — the same components, extended with billing, payouts, reputation, and a relay fleet, let you rent idle GPUs from strangers at a fraction of hyperscaler prices. Design of record; development parked until the core is proven.

## Design documentation

The full design lives under [`docs/`](docs/README.md):

| Section | Contents |
|---|---|
| [Architecture](docs/architecture/) | System overview, [deployment profiles](docs/architecture/profiles.md) (standalone / private fleet / marketplace), components, job lifecycle, red-team design review |
| [ADRs](docs/adr/) | Architecture Decision Records — what was decided, why, and what it costs; includes [single-binary self-host control plane](docs/adr/0013-single-binary-self-host-control-plane.md) and [deployment profiles / marketplace-optional](docs/adr/0014-deployment-profiles-marketplace-optional.md) |
| [Platform](docs/platform/) | [Backend engineering design](docs/platform/backend.md), host agent, isolation tiers, control plane, networking, security & trust model, wire protocol, renter API |
| [ML lifecycle](docs/ml-lifecycle/) | Data, training, evaluation, serving, runtime environments, recipe catalog |
| [Product](docs/product/) | [Self-hosting guide](docs/product/self-host.md), deployment & DX, roadmap, marketplace mechanics (hosted layer — deferred), unit economics (hosted layer — deferred) |

## Principles

1. **Runs on one box.** The whole backend is a couple of static Rust binaries with embedded SQLite, idling under 100MB — self-hostable on a single GPU machine with one command, no Kubernetes, no Postgres, no NATS to operate. Heavy infra (Postgres, NATS, relay fleet) appears only at marketplace scale.
2. **ML-first.** The stack exists to serve ML practitioners — the runtime images, APIs, and workflows are shaped around how models are actually built, evaluated, and served on your own GPUs.
3. **Nodes are cattle.** Machines vanish mid-job — a gaming rig that reboots, a fleet node that drops off; checkpointing, retry, and failover are core paths, not edge cases.
4. **Minutes, not days.** A self-hoster stands up the core and completes a fine-tune in minutes; in the hosted layer, a host onboards with one command in under 10 minutes and a renter runs their first job in under 5.
5. **Both directions of trust are first-class.** Host-from-workload isolation and renter-from-host privacy are designed together, honestly labeled per tier — the trust machinery that makes the hosted marketplace safe when that layer resumes.
6. **Boring where possible, novel only where necessary.** Proven components (Cloud Hypervisor, vLLM, WireGuard, SQLite at the core / Postgres at marketplace scale) glued by a small amount of careful Rust.

## License

Apache-2.0
