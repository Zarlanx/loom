# Build-plan review log

Disposition records for external reviews of the [Phase-1 build plan](./README.md). The design-doc set has its own review record ([../architecture/external-review.md](../architecture/external-review.md)); this file is specific to `docs/build/`.

---

## Review 2 — Codex (2026-07-08): PR split, order, and feature-start

An independent [OpenAI Codex](https://openai.com/index/introducing-codex/) read-only pass reviewed the build plan with a narrow brief: how we start building, how the PRs are split, and whether the ordering holds. It endorsed the strategy and found the DAG was cut too much along crate lines, hid bootstrap work, and carried several dependency errors.

### Endorsed

- **Walking-skeleton-first is the right way to start** — `loom run -- echo hi` exercises the whole spine (API → store → scheduler → gateway → agent → sandbox → logs → terminal state) before any feature is thickened.
- **Contracts-first** (PR-02 proto, PR-04 OpenAPI) as the unblock for parallel work.
- **Checkpoint-resume fencing proven on the fake fleet** (PR-12/PR-17) is the correct way to prove the lease/requeue invariants without GPU cost.

### Accepted and applied

| # | Finding | Resolution |
|---|---------|------------|
| 1 | PR-19 marked *parallel with* PR-18, but its gate deploys PR-18's adapter | PR-19 now **depends on** PR-18 ([README](./README.md#wave-3--thicken-verticals)) |
| 2 | PR-20's `top` needs real NVML telemetry from PR-16 | PR-20 now depends on PR-16; minimal single-job log return moved into PR-13 |
| 3 | PR-22's gate *is* checkpoint-resume on real GPUs, but it didn't depend on PR-16/PR-17 | PR-22 now depends on **PR-16, PR-17, PR-21** — and the critical path extends through PR-22 to M4 |
| 4 | No PR owned the security bootstrap (CA, cert signing, token/admin-token issuance) though PR-09 issues certs | Added **PR-26 `bootstrap-auth`**; PR-09 and PR-11 now depend on it |
| 5 | No PR covered local per-second metering (a roadmap exit criterion) | Added **PR-27 `usage-metering`** |
| 6 | No PR covered sealed secret/env injection into jobs (needed for gated HF pulls) | Added **PR-28 `secret-injection`** |
| 7 | PR-05 missing `hosts`/`gpus`/`nodes` + `accounts`/`api_keys` tables needed downstream | PR-05 scope expanded to include them |
| 8 | PR-02 froze the *full* message catalog speculatively | PR-02 lands the **M1 message set** first; checkpoint/serving messages added additively before PR-17/PR-19 |
| 9 | PR-04's diff-gate can't enforce before handlers exist | PR-04 scaffolds the gate + ships spec/lint/mock; the diff gate becomes **enforcing at PR-11** |
| 10 | Postgres implied in Phase 1 (workspace CI matrix) vs SQLite-only roadmap | Phase 1 is **SQLite-only**: PR-05 = `SqliteStore` only; the Postgres conformance leg is Phase-3; CI matrix and prose corrected ([workspace-setup](./workspace-setup.md#4-ci-pipeline)) |
| 11 | PR-01 CI listed contract/store gates as required from day one | Only fmt/clippy/test required at PR-01; contract/store gates **become required as their owning PR lands** |
| 12 | The fattest PRs (07/08/09/17) too big to review with an achievable gate | Split by **proof-slice** into trait+fake then real-impl sub-PRs ([README §6](./README.md#6-revisions-from-the-second-external-review-codex-2026-07-08)) |
| 13 | PR-13 tries to prove the whole spine at once | Staged into **M1a** (fake agent+sandbox) → **M1b** (real protocol) → **M1c** (real runc) |
| 14 | `loom-ckpt` had no home; checkpoint-on-fakes doesn't prove the HF-callback/RNG path | `loom-ckpt` is a Python package (`python/loom-ckpt/`); PR-17b adds a **CPU-only deterministic restore fixture** before real-GPU PR-17c |
| 15 | Non-Rust artifacts had no home in the PR-01 tree | `proto/`, committed `openapi.json`, image definitions, `python/loom-ckpt/`, and `scripts/` added to the [workspace layout](./workspace-setup.md#1-the-workspace-tree) |

### Corrected critical path

- To **M3 (PR-17)**: ten PRs — `PR-01→03→05→06/26→09→11→12→13→16→17`. PR-26 sits at PR-06's depth and does not lengthen it.
- To a **complete Phase 1 (M4)**: eleven — `…→PR-17→PR-22`. This is the honest delivery floor, because M4 is a roadmap exit criterion and PR-22 depends on PR-17.

### Not changed (with reason)

- **The five hard calls stand** — the review endorsed all of them; the corrections are about *granularity and completeness of the DAG*, not the strategy.
- **The `2–3 engineer` staffing split** is unchanged in shape; PR-26/27/28 slot into the existing owners (bootstrap-auth → invariant core; metering → core; secret-injection → agent).
- **No renumbering.** New PRs are appended as PR-26/27/28 and slotted by dependency rather than renumbering 25 existing references across five docs — the dependency edges, not the numbers, define the order.
