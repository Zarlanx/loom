# Loom design documentation

This is the design-phase documentation for Loom, a distributed GPU compute platform. Documents are organized by concern; each is self-contained but cross-references the others.

## Architecture

- [overview.md](architecture/overview.md) — system components, request/job flows, technology decisions at a glance
- [profiles.md](architecture/profiles.md) — deployment profiles: the same components collapsed onto one box (standalone), spread across your own machines (private fleet), or extended into the hosted marketplace (deferred)
- [design-review.md](architecture/design-review.md) — red-team self-critique: ranked findings, pre-build spikes, verdict
- [external-review.md](architecture/external-review.md) — disposition record for the independent external review (Codex, 2026-07-07): endorsements, the 13 accepted-and-applied fixes, and accepted follow-up work

## Build (implementation plan)

- [build/](build/README.md) — the Phase-1 build plan: the authoritative 25-PR DAG, milestones, the parallelization/staffing model, workspace setup, and the research tracks. How the design docs become software.

## Decisions

- [adr/](adr/README.md) — Architecture Decision Records with context, consequences, and revisit-when triggers

## Platform (the compute fabric)

- [backend.md](platform/backend.md) — backend engineering design: the `loomd`/`hostd` binaries, embedded SQLite core, single-binary self-host control plane, and where Postgres/NATS enter at marketplace scale
- [compute-backends.md](platform/compute-backends.md) — pluggable compute backends: capability model, lazy runtimes, per-backend engines (mlx | cuda | cpu | rocm)
- [host-agent.md](platform/host-agent.md) — the Rust daemon hosts install: lifecycle, metering, hardware attestation, idle-time policy
- [isolation.md](platform/isolation.md) — sandboxing tiers: containers + nvidia-container-toolkit, gVisor/nvproxy, Cloud Hypervisor + VFIO microVMs
- [control-plane.md](platform/control-plane.md) — scheduler, job lifecycle, state, billing metering pipeline
- [networking.md](platform/networking.md) — outbound-only agents, NAT traversal, relay + WireGuard, data-plane routing
- [security.md](platform/security.md) — threat model both directions, trust tiers, gateway identity-stripping, ephemeral execution, TEE tier
- [agent-protocol.md](platform/agent-protocol.md) — wire-protocol spec between host agent and control plane
- [renter-api.md](platform/renter-api.md) — renter-facing API spec: control plane + OpenAI-compatible surface

## ML lifecycle (the managed workflows)

- [data.md](ml-lifecycle/data.md) — data collection, processing (Spark/Ray/Daft), storage, versioning
- [training.md](ml-lifecycle/training.md) — training and fine-tuning stack: PyTorch, FSDP, DeepSpeed, Unsloth, TRL, LoRA/QLoRA
- [evaluation.md](ml-lifecycle/evaluation.md) — benchmarks, metrics, lm-evaluation-harness, experiment tracking
- [serving.md](ml-lifecycle/serving.md) — inference engines (vLLM and friends), serverless gateway, weight cache, Hugging Face integration
- [environments.md](ml-lifecycle/environments.md) — curated runtime images, CUDA/ROCm support matrix, compilers (Triton, torch.compile)
- [recipes.md](ml-lifecycle/recipes.md) — the managed recipe catalog: manifest contract, v1 recipes, lineage, cost estimation

## Product

- [self-host.md](product/self-host.md) — self-hosting guide: stand up the core on one GPU box or your own private fleet, run a resumable fine-tune, serve models locally
- [deployment.md](product/deployment.md) — host onboarding, renter quickstart, CLI/UX design
- [roadmap.md](product/roadmap.md) — phased milestones from self-hostable core to TEE tier
- [marketplace.md](product/marketplace.md) — pricing, billing, reputation, work verification (hosted marketplace layer — deferred)
- [unit-economics.md](product/unit-economics.md) — operator cost/margin model, break-even and sensitivity analysis (hosted marketplace layer — deferred)
