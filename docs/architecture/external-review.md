# External review — Codex (2026-07-07)

**Status:** Disposition record · 2026-07-07 · owner: architecture

## Scope & method

This is the disposition record for an **independent external review of the complete Loom design doc-set**, conducted by the **OpenAI Codex CLI** at the founder's request. Codex read the full corpus — the architecture overview and profiles, all ADRs, the seven platform docs, the six ML-lifecycle docs, and the four product docs — as an outside auditor with no stake in the design, and returned a set of endorsements, concrete findings, and overlooked-area flags. The maintainer triaged every item; this document records what was **endorsed**, what was **accepted and applied** as surgical doc edits, and what was **accepted as follow-up work** (design or decisions owed, not doc edits). It is the companion to the internal red-team [design-review.md](./design-review.md): where that doc is our own adversarial self-critique, this one records that an *external* reviewer, reading independently, reached consistent conclusions.

## Endorsed

Codex validated the load-bearing decisions of the design rather than merely finding fault. It explicitly endorsed:

- **The self-host-first pivot** — leading with a self-hostable compute stack the user runs on their own hardware, with the marketplace as an optional deferred layer ([profiles.md](./profiles.md), [ADR-0014](../adr/0014-deployment-profiles-marketplace-optional.md)).
- **No Kubernetes** — the argument that K8s assumes an owned, inbound-reachable fleet while Loom's supply is NAT'd strangers' machines is genuinely load-bearing and correct ([overview.md](./overview.md), [ADR-0004](../adr/0004-no-kubernetes-control-plane.md)).
- **The embedded-SQLite floor** — a single binary with embedded SQLite and an in-process bus, Postgres/NATS only at marketplace scale, is the right zero-infra default ([ADR-0013](../adr/0013-single-binary-self-host-control-plane.md), [backend.md](../platform/backend.md)).
- **The library-first workspace** — every service embeddable behind a trait so the whole backend boots in one integration test ([backend.md §1, §9](../platform/backend.md)).
- **Single-node scope** — refusing multi-node WAN training on physics grounds, not as a roadmap gap ([training.md §1e](../ml-lifecycle/training.md), [ADR-0011](../adr/0011-single-node-scope.md)).
- **Curated images** — a small digest-pinned catalog with driver-floor-at-enrollment as the disciplined answer to the CUDA/driver combinatorial swamp ([environments.md](../ml-lifecycle/environments.md), [ADR-0010](../adr/0010-curated-runtime-images.md)).
- **vLLM whole-model-per-node serving** — every warm node a complete failover target, no cross-node coordination on the critical path ([serving.md](../ml-lifecycle/serving.md), [ADR-0009](../adr/0009-vllm-primary-inference-engine.md)).
- **Security honesty** — refusing to claim in-use confidentiality on consumer cards, with an explicit residual-risk register ([security.md](../platform/security.md)).
- **Default-deny egress** — the allowlisted-mirror sandbox posture as defense-in-depth even for a trusted user's own dependencies ([networking.md](../platform/networking.md), [self-host.md §5](../product/self-host.md)).

## Accepted and applied

Thirteen findings were accepted and applied as surgical doc edits. Each is resolved in-place; the fixed file is cited.

| # | Finding | Resolution |
|---|---|---|
| 1 | Phase 1 isolation contradiction (plain-runc vs gVisor default) | Phase 1 default is hardened-runc for the trusted user; gVisor/nvproxy *qualification* (S1) moves into Phase 1; hardened Tier B is a hard precondition for untrusted workloads ([roadmap.md](../product/roadmap.md), clarifying sentence in [profiles.md](./profiles.md), [self-host.md §5](../product/self-host.md)) |
| 2 | Remote access allowed bearer-token over plain non-loopback bind | Loopback may use HTTP; any non-loopback bind requires TLS via `loom init` self-signed cert + CLI fingerprint pinning ([self-host.md §3](../product/self-host.md)) |
| 3 | Port story inconsistent | Canonical: TCP 8443 = renter API *and* WSS agent transport; QUIC optional on UDP 8444 ([self-host.md §3](../product/self-host.md), [backend.md §7](../platform/backend.md)) |
| 4 | Rootless-Podman overpromise | Sandbox features need the root helper / root-capable runtime; rootless Podman softened to a future/limited mode ([self-host.md §2.1](../product/self-host.md)) |
| 5 | In-proc bus durability overclaim | Reframed to ADR-0013: bus events are delivery hints, state is authoritative in SQLite, effects reconstructable by reconciliation; no JetStream-equivalent claim ([backend.md §4](../platform/backend.md)) |
| 6 | `NUMERIC(12,6)` money type | Changed to `BIGINT` integer micro-USD, matching backend.md §4 and renter-api.md §1.1 ([control-plane.md §2](../platform/control-plane.md)) |
| 7 | OpenAPI ownership contradiction | Canonical code-first: utoipa-generated with a CI diff-gate that fails on drift ([control-plane.md §7](../platform/control-plane.md), [backend.md §8](../platform/backend.md)) |
| 8 | Flow B failover oversell | Aligned with serving.md §3: node death re-dispatches and the stream *restarts visibly*; deterministic continuation only where proven ([overview.md](./overview.md)) |
| 9 | Sealed-secrets "never visible to node owner" overclaim | Corrected for Tier A/B (a live host can dump guest RAM); added HF-token brokering through the control/artifact service ([training.md §6](../ml-lifecycle/training.md)) |
| 10 | Deletion semantics | `loom data rm` is authoritative on controlled infra, best-effort on untrusted marketplace hosts ([data.md §6](../ml-lifecycle/data.md)) |
| 11 | 15-minute headline honesty | Qualified: install-to-first-GPU-command < 15 min; first train/serve run additionally pulls multi-GB images/weights, bounded and cached ([self-host.md §1](../product/self-host.md)) |
| 12 | Phase-1 golden-path scope creep | Narrowed to one golden path (loomd + loom-hostd + CLI, local artifact store, `loom data push`, `qlora-sft`, adapter checkpoint/resume, local vLLM deploy, 3 images); other recipes/images/ROCm/P2P/full-FT to Phase 2+ ([roadmap.md](../product/roadmap.md), notes in [recipes.md §3](../ml-lifecycle/recipes.md) and [environments.md §2](../ml-lifecycle/environments.md)) |
| 13 | Raw-rsync-of-live-DB backup advice | Replaced with SQLite online backup (`VACUUM INTO`/`.backup`) via a planned `loom backup`/`loom restore` with verification; rsync retained for artifact/cache dirs ([self-host.md §6](../product/self-host.md)) |

## Accepted as follow-up work

Codex raised overlooked areas that need future design or a decision rather than a doc edit. These are accepted onto the backlog, not resolved here:

- **Licensing decision.** A clear stance on redistributable-vs-restricted dependencies (e.g. the Unsloth multi-GPU terms already flagged in training.md) before anything is bundled into a shipped recipe.
- **Self-hoster support / diagnostics-bundle model.** How a self-hoster gets help without an operator — a `loom support-bundle` (logs, config, `loom doctor` output, redacted) and a documented triage path.
- **Signing-key lifecycle runbook.** Generation, rotation, revocation, and compromise recovery for the release-signing and manifest-signing keys the installer and agent trust.
- **Private-fleet multi-user governance.** Named, revocable per-person tokens with quotas and an audit trail for a team fleet, without pulling in the full hosted identity system (the open question already flagged in self-host.md §9 and backend.md §8).
- **Air-gapped install story.** A no-internet install and image/weight-seeding path for regulated or disconnected environments.
- **Release-qualification hardware budget.** The standing set of real GPUs (the S1 matrix cards and more) needed to qualify each catalog release, and who owns/pays for it.
- **Operational SLOs for the core.** Concrete availability/recovery targets for `loomd` (restart-to-ready, scheduler failover window, backup RPO) that a self-hoster can plan against.

## Conclusion

The external review corroborates the internal one: the architecture is sound where it counts, the thirteen accepted findings were honest sharp edges now filed down in-place, and the remaining work is scoped and owned rather than hidden.
