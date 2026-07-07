# Architecture Decision Records

This directory records the load-bearing architectural decisions behind Loom, a distributed GPU compute marketplace, as fixed during the Phase 0 design effort. Each ADR captures one decision in the classic form — Status, Context, Decision, Consequences (including what we give up), and a Revisit-when trigger — and cites the design document(s) where the decision is specified in full. These records are descriptive, not aspirational: they document what the merged design docs under [`../`](../) already decided and why, so a new engineer can understand the *shape* of the system and the tradeoffs baked into it without reconstructing the reasoning from scratch. When a decision changes, supersede its ADR rather than editing history.

## Index

| # | Title | Status |
|---|---|---|
| [0001](./0001-rust-single-binary-host-agent.md) | Rust single-binary host agent | Accepted |
| [0002](./0002-cloud-hypervisor-vfio-microvm-tier.md) | Cloud Hypervisor + VFIO for the microVM tier | Accepted |
| [0003](./0003-tiered-isolation.md) | Tiered isolation instead of one mechanism for all hosts | Accepted |
| [0004](./0004-no-kubernetes-control-plane.md) | No Kubernetes; control plane = API + scheduler + Postgres + NATS | Accepted |
| [0005](./0005-outbound-only-connectivity.md) | Outbound-only agent connectivity; QUIC primary, WSS fallback, relay + WireGuard upgrade | Accepted |
| [0006](./0006-gateway-identity-stripping.md) | Gateway identity-stripping as the primary renter-privacy mechanism | Accepted |
| [0007](./0007-ephemeral-everything-teardown.md) | Ephemeral-everything teardown in the agent | Accepted |
| [0008](./0008-confidential-tier-c.md) | Confidential Tier C via SEV-SNP/TDX + H100/Blackwell CC | Accepted |
| [0009](./0009-vllm-primary-inference-engine.md) | vLLM as primary inference engine; whole-model-per-node; keep-warm | Accepted |
| [0010](./0010-curated-runtime-images.md) | Curated runtime images only at v1; Nydus/EROFS lazy pull | Accepted |
| [0011](./0011-single-node-scope.md) | Single-node scope: no WAN multi-node training or distributed data | Accepted |
| [0012](./0012-fixed-pricing-fiat-only.md) | Fixed ask pricing + 20% take + fiat only | Accepted |
| [0013](./0013-single-binary-self-host-control-plane.md) | Single-binary self-host control plane; SQLite/in-proc default, Postgres/NATS optional | Accepted |
| [0014](./0014-deployment-profiles-marketplace-optional.md) | Three deployment profiles; self-hostable core, marketplace optional and deferred | Accepted |
