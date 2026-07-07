# 0004 — No Kubernetes; control plane = API + scheduler + Postgres + NATS

**Status:** Accepted — 2026-07-07 · Amended by [ADR-0013](0013-single-binary-self-host-control-plane.md): Postgres+NATS is the marketplace-scale configuration; the core control plane embeds SQLite and an in-process queue.

## Context

Loom's supply is thousands of independently-owned single machines, each behind residential NAT, each of which can vanish without notice, each reachable only via outbound connections it initiates. This is the exact opposite of what a cluster orchestrator models. Kubernetes (and Nomad, and Ray) assume a fleet of machines an operator owns, can reach on a private network, and administers as one pool; a kubelet expects a controllable node on a flat network. Our "node" is a stranger's gaming PC that we can never dial into. Forcing that supply into a cluster abstraction is a category error.

## Decision

Build the control plane as a small set of near-stateless services around **one Postgres and one NATS**, and **do not use Kubernetes** for the host fleet ([`../architecture/overview.md`](../architecture/overview.md) "not building on Kubernetes"; [`../platform/control-plane.md`](../platform/control-plane.md)):

- **API service** — the only writer of intent (jobs, deployments, admin actions); horizontally scalable, stateless.
- **Scheduler** — a single-process filter → score → commit loop over Postgres, with leases guarding against double-scheduling.
- **Agent-gateway** — terminates outbound agent QUIC/WSS, bridges to NATS.
- **Postgres** — source of truth (hosts, nodes, jobs, attempts, leases, usage, ledger) and the transactional-outbox substrate.
- **NATS (JetStream)** — request/reply, fan-out control, and durable streams for usage/lifecycle events. Chosen over Kafka (heavy ops, no first-class req/reply) and Redis Streams (conflates cache and bus) ([`../platform/control-plane.md`](../platform/control-plane.md) §1.1).

Checkpoint, requeue, and failover are core paths, not exception handlers: a vanished node is the expected case.

## Consequences

- Operator infrastructure stays small and legible — MVP fits in a single Docker Compose that a two-person on-call can reason about at 3 a.m. ([`../platform/control-plane.md`](../platform/control-plane.md) §9).
- The model matches reality: nodes are cattle reachable only outbound; the control plane coordinates rather than orchestrates.

**What we give up:**

- **We build scheduling, leasing, reconciliation, and self-healing ourselves** — the filter/score/commit loop, lease TTLs and lapse-on-death, the 90 s silence timeout, serving-replica maintenance, and preemption ordering are all our code, not an off-the-shelf orchestrator's ([`../platform/control-plane.md`](../platform/control-plane.md) §3–§4). We forgo the K8s ecosystem (operators, autoscalers, service mesh) entirely.
- A single-process scheduler is the honest MVP; HA is lease-based fail-safe (crash-and-restart), and horizontal scale means building region sharding later ([`../platform/control-plane.md`](../platform/control-plane.md) §4, §8).
- Exactly-once effects require the transactional-outbox + JetStream dedup discipline we must maintain carefully around every money-touching path.

## Revisit when

The fleet outgrows a single-process scheduler — the pre-designed path is **shard by region**, one scheduler per region, not adopting Kubernetes ([`../platform/control-plane.md`](../platform/control-plane.md) §4). Kubernetes does not become correct for this supply shape; only its scaling lessons transfer.
