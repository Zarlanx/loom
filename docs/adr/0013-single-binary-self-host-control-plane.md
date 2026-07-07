# 0013 — Single-binary self-host control plane; SQLite/in-proc default, Postgres/NATS optional

**Status:** Accepted — 2026-07-07

**Amends [ADR-0004](0004-no-kubernetes-control-plane.md).**

## Context

Loom's core product is now a **self-hostable GPU compute stack**: anyone should be able to deploy the entire backend on their own machine or fleet and get the full ML lifecycle on their own GPUs, with the operator-run marketplace as an optional layer on top ([ADR-0014](0014-deployment-profiles-marketplace-optional.md), [`../architecture/profiles.md`](../architecture/profiles.md)). [ADR-0004](0004-no-kubernetes-control-plane.md) fixed the control plane as API + scheduler + **Postgres + NATS**. That is the right shape at marketplace scale, but it is the wrong *floor* for self-host: telling someone who wants to fine-tune on their gaming PC to first stand up Postgres and a NATS cluster violates the self-host promise and the hard lightweight constraint (small binaries, `loomd` < 100 MB RSS idle, no JVM/Python/Kubernetes/Docker in the core).

## Decision

The control plane is **`loomd`, one lightweight Rust binary** carrying the API, scheduler, and an embedded inference gateway. Two seams make it span standalone to marketplace scale:

- **Storage behind a repository trait.** **SQLite embedded is the default**; **Postgres is a compile-time feature** for concurrent-writer throughput, PITR, and streaming replicas at marketplace scale.
- **Message bus behind a bus trait.** An **in-process queue is the default**; **NATS (JetStream) is optional** for durable cross-process streams and req/reply at fleet scale.

The same binary serves all three deployment profiles ([`../architecture/profiles.md`](../architecture/profiles.md)); the profile chooses which backend each trait binds to. Internals are specified in [`../platform/backend.md`](../platform/backend.md). This **amends ADR-0004**: Postgres+NATS is now the *marketplace-scale configuration*, not the core requirement. The no-Kubernetes decision, the cattle-node model, and checkpoint/requeue/failover as core paths all stand unchanged.

## Consequences

- Self-host is genuinely one binary and a data file — zero external services for standalone and private-fleet ([`../architecture/profiles.md`](../architecture/profiles.md)).
- The core stays lightweight and legible; the heavy infra is opt-in, reached only when scale demands it.
- The scaling path from ADR-0004 is preserved: at marketplace scale you bind the Postgres and NATS backends and get exactly the design already documented in [`control-plane.md`](../platform/control-plane.md).

**What we give up:**

- **Two storage backends to build, test, and keep behaviourally equivalent.** SQLite and Postgres differ in concurrency, locking, and type affinity; the repository trait must be exercised against both in CI or subtle divergences ship.
- **No distributed-bus semantics in the core.** The in-process queue has no durability across a `loomd` restart and no cross-process fan-out. Features that need durable streams or multi-consumer req/reply (the metering→billing pipeline, JetStream-backed exactly-once effects) require the heavier NATS configuration and are therefore effectively marketplace-tier.
- The transactional-outbox / exactly-once discipline from [control-plane.md](../platform/control-plane.md) §3 applies fully only in the Postgres+NATS configuration; the in-proc default offers weaker delivery guarantees, acceptable because the trusted single-user/single-fleet profiles don't run the money path.

## Revisit when

A self-host user hits a wall the in-proc/SQLite defaults can't clear *below* marketplace scale — e.g. a private fleet large enough to want durable streams without adopting the full marketplace stack. At that point the trait boundaries already allow mixing (Postgres with the in-proc bus, or vice versa); revisit whether an intermediate "durable-embedded" bus (e.g. a SQLite-backed queue) is worth a third backend.
