# Loom design documentation

This is the design-phase documentation for Loom, a distributed GPU compute platform. Documents are organized by concern; each is self-contained but cross-references the others.

## Architecture

- [overview.md](architecture/overview.md) — system components, request/job flows, technology decisions at a glance

## Platform (the compute fabric)

- [host-agent.md](platform/host-agent.md) — the Rust daemon hosts install: lifecycle, metering, hardware attestation, idle-time policy
- [isolation.md](platform/isolation.md) — sandboxing tiers: containers + nvidia-container-toolkit, gVisor/nvproxy, Cloud Hypervisor + VFIO microVMs
- [control-plane.md](platform/control-plane.md) — scheduler, job lifecycle, state, billing metering pipeline
- [networking.md](platform/networking.md) — outbound-only agents, NAT traversal, relay + WireGuard, data-plane routing
- [security.md](platform/security.md) — threat model both directions, trust tiers, gateway identity-stripping, ephemeral execution, TEE tier

## ML lifecycle (the managed workflows)

- [data.md](ml-lifecycle/data.md) — data collection, processing (Spark/Ray/Daft), storage, versioning
- [training.md](ml-lifecycle/training.md) — training and fine-tuning stack: PyTorch, FSDP, DeepSpeed, Unsloth, TRL, LoRA/QLoRA
- [evaluation.md](ml-lifecycle/evaluation.md) — benchmarks, metrics, lm-evaluation-harness, experiment tracking
- [serving.md](ml-lifecycle/serving.md) — inference engines (vLLM and friends), serverless gateway, weight cache, Hugging Face integration
- [environments.md](ml-lifecycle/environments.md) — curated runtime images, CUDA/ROCm support matrix, compilers (Triton, torch.compile)

## Product

- [deployment.md](product/deployment.md) — host onboarding, renter quickstart, CLI/UX design
- [marketplace.md](product/marketplace.md) — pricing, billing, reputation, work verification
- [roadmap.md](product/roadmap.md) — phased milestones from MVP to TEE tier
