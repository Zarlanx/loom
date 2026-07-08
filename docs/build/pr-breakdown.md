# PR breakdown — small, stacked PRs

The [25+3 entries in the DAG](./README.md#3-the-authoritative-pr-dag) are **workstreams (epics)**, not single pull requests. Each ships as a *stack* of small PRs listed here. This doc is the implementable unit of work; the README DAG is the dependency map between epics.

## The sizing rule

1. **One reviewable seam per PR.** Target **≤ ~400 changed lines** and **< 1 day** of work, independently revertable. If a PR can't be reviewed carefully in one sitting, it's too big.
2. **Cut by seam, not by line count.** The natural cut points, in order: **trait + fake** → **real implementation** → **wiring/integration** → **hardening**. Each is a landing.
3. **Stacked and ordered.** Sub-PRs of an epic (`05a → 05b → 05c`) are a dependent stack, merged in sequence. A later sub-PR's gate assumes the earlier ones landed.
4. **Every sub-PR keeps its own runnable gate.** "Proves itself by" must hold *at that sub-PR* — a trait+fake PR proves the fake passes the conformance suite; the real-impl PR proves the real backend passes the *same* suite.
5. **The one exception — never split a correctness invariant.** The lease/fencing rules (**PR-03b**) and the scheduler reconciliation loop (**PR-12**) land as **single coherent PRs even though they are the largest**, because splitting split-brain correctness across two PRs is exactly where the bug hides ([hard call #3](./README.md#1-the-five-hard-calls-read-this-before-the-table)). Smaller is the default; coherent-invariant beats smaller when they conflict.
6. **Contracts split schema-from-harness.** The `.proto` and OpenAPI *schemas* are reviewed as a whole (additive-only afterward); the codegen/mock/diff *harness* is a separate PR.

**Result: 66 small PRs across the 28 workstreams** (verified acyclic, no dangling deps). Measured in small PRs the critical path is longer than the 11-epic chain but the same *shape* — single-owned and sequential, width hiding behind it ([parallelization.md](./parallelization.md)): **12 sub-PRs to M1** (`01a→03a→03b→05a→05b→05c→26b→11a→11c→13a→13b→13c`), **16 to M3** (…`→16a→16b→16c→17c`), **17 to M4** (…`→22c`). One structural note the decomposition surfaces: at the sub-PR level `bootstrap-auth` (26b, via its dep on the `accounts`/`api_keys` schema `05c`, and `loomd`'s dep on it) sits *on* the critical path — a coupling the epic-level graph hid, and a reason to keep PR-26 lean. Notation below: **`id`** — scope · *dep: …* · gate.

---

## Wave 0 — Foundations & Contracts (8)

**PR-01 `workspace-scaffold` → 2**
- **01a** — Cargo workspace + 10 empty crate stubs + `rust-toolchain` · *dep: —* · gate: `cargo build` green on empty workspace
- **01b** — CI (fmt/clippy/test) + `xtask` stub + `deny.toml` + non-Rust dirs (`proto/`, `openapi.json`, `images/`, `python/loom-ckpt/`, `scripts/`) · *dep: 01a* · gate: fmt+clippy+test required and green

**PR-02 `proto-contract` → 2**
- **02a** — `Envelope` + length-prefix codec + golden-vector harness (`xtask golden`) · *dep: 01b* · gate: codec round-trips; regen works
- **02b** — M1 message set (enrollment/job/log/heartbeat) `.proto` + prost codegen · *dep: 02a* · gate: fake agent + server decode identical bytes

**PR-03 `core-domain` → 2**
- **03a** — Domain types (`Job`/`Attempt`/`Node`/`Lease`/…) + job-lifecycle FSM · *dep: 01a* · gate: exhaustive FSM transition tests
- **03b** — **Lease + fencing rules + scheduler pure logic** *(invariant — one PR)* · *dep: 03a* · gate: property tests for requeue-lineage/fencing; no split-brain reachable in the pure model

**PR-04 `openapi-contract` → 2**
- **04a** — Committed `openapi.json` + RFC 9457 error taxonomy · *dep: 01a* · gate: spec validates + lints
- **04b** — Mock server + diff-gate scaffold (enforcing deferred to 11b) · *dep: 04a* · gate: mock answers golden-path routes

## Wave 1 — Seams & Skeletons (16)

**PR-05 `store-sqlite` → 3**
- **05a** — `Store` trait + `FakeStore` + conformance-suite skeleton · *dep: 03b* · gate: fake passes the conformance skeleton
- **05b** — `SqliteStore` + core migrations (jobs/attempts/leases/usage/outbox/idempotency) · *dep: 05a* · gate: conformance green on **file-backed WAL**
- **05c** — Schema: `hosts`/`gpus`/`nodes` + `accounts`/`api_keys` + rows · *dep: 05b* · gate: enrollment/auth/scheduling reads+writes covered

**PR-06 `bus-inproc` → 2**
- **06a** — `Bus` trait + `InProcBus` + delivery tests · *dep: 03a* · gate: at-least-once + reconcile-from-store
- **06b** — Outbox relay task · *dep: 06a, 05b* · gate: relay drains outbox rows

**PR-07 `sandbox-runc` → 3**
- **07a** — `SandboxDriver` trait + `FakeDriver` (CI-without-root) · *dep: 01a* · gate: fake runs a scripted job
- **07b** — `RuncDriver` run + teardown · *dep: 07a* · gate: `echo` runs in a container; clean teardown
- **07c** — Hardening: seccomp, dropped caps, cgroup v2, default-deny egress netns · *dep: 07b* · gate: egress-deny + no-RFC1918 tests

**PR-08 `hostd-skeleton` → 3**
- **08a** — Config + control-channel client (WSS) + connect/reconnect/backoff · *dep: 02b* · gate: connects+reconnects to a fake gateway
- **08b** — Enrollment CSR client · *dep: 08a* · gate: obtains a (fake-signed) node cert
- **08c** — Agent FSM + heartbeat + durable spool · *dep: 08b, 03a* · gate: FSM drives; spool replays after a drop

**PR-09 `agentgateway` → 3**
- **09a** — quinn/WSS terminator + mTLS + 4-stream demux · *dep: 02b, 05a* · gate: fake agent connects over the real terminator
- **09b** — Bus bridge (agent messages ↔ `Bus`) · *dep: 09a, 06a* · gate: messages land on the bus
- **09c** — Enrollment + cert issuance · *dep: 09a, 26a* · gate: token-only agent gets a signed cert; bad token refused

**PR-10 `cli-skeleton` → 2**
- **10a** — `clap` tree + auth/local-token + JSON mode · *dep: 04a* · gate: `--help` + auth shell against mock
- **10b** — `loom run`, job status, `ps` (vs mock) · *dep: 10a, 04b* · gate: golden-path commands render

## Wave 2 — Walking Skeleton (7)

**PR-11 `loomd-skeleton` → 3**
- **11a** — Process wiring (Store+Bus+config+lazy start) + `loomd init`/`doctor` · *dep: 04a, 05b, 06a, 26b* · gate: `loomd` boots standalone on SQLite
- **11b** — axum API handlers (job submit/get/list, store-backed) + **diff-gate now enforcing** · *dep: 11a, 09b* · gate: API works; generated spec == committed
- **11c** — Agent-gateway mount into loomd · *dep: 11a, 09c* · gate: a real agent connects to a running loomd

**PR-12 `scheduler-v0` → 1 (deliberately not split)**
- **12** — **Single-writer reconciliation loop + lease-with-fencing + requeue-on-lost** *(invariant — one PR)* · *dep: 05b, 11a, 03b* · gate: simulated-fleet chaos — strictly-greater fence on requeue; no double-lease; no double-bill

**PR-13 `tracer-bullet` · M1 → 3**
- **13a** — `loom run -- echo hi`: CLI → real API → SQLite → scheduler → **fake** agent + **fake** sandbox · *dep: 10b, 11c, 12, 07a* · gate: echo job reaches `Succeeded` (all fakes)
- **13b** — Swap in real agent protocol + `loom-hostd` over loopback · *dep: 13a, 08c* · gate: same demo, real protocol
- **13c** — Swap in real `runc` + minimal single-job log return · *dep: 13b, 07c* · gate: same demo, real container — **M1 closed**

## Wave 3 — Verticals (17)

**PR-14 `artifact-store` → 2**
- **14a** — Content-addressed on-disk store (put/get by digest) · *dep: 11a* · gate: digest round-trip
- **14b** — Presign-style local URLs + GC/keep-last-N · *dep: 14a* · gate: presigned round-trip; GC keeps last N

**PR-15 `data-push` → 2**
- **15a** — Manifest + chunking + upload to artifact store · *dep: 14a* · gate: push produces a manifest
- **15b** — Node prefetch + `name@vN` refs + `loom data push` · *dep: 15a, 10a* · gate: warm re-run is a cache hit

**PR-16 `gpu-execution` → 3**
- **16a** — NVML inventory + driver-floor enrollment gate · *dep: 13c* · gate: old-driver card refused; specs reported
- **16b** — GPU injection into the sandbox (nvidia-container-toolkit) · *dep: 16a, 07c* · gate: a real CUDA job sees the GPU
- **16c** — Hardware-gated GPU smoke suite · *dep: 16b* · gate: passes on a GPU runner; skips cleanly without

**PR-17 `checkpoint-resume` · M3 → 3**
- **17a** — Checkpoint state/protocol + fencing on the fake fleet · *dep: 12, 14a* · gate: kill+resume on fakes; fence increments
- **17b** — Artifact checkpoint I/O + **CPU-only deterministic `loom-ckpt` fixture** · *dep: 17a, 14b* · gate: RNG/exact-step restore proven with no GPU
- **17c** — Real-GPU HF-Trainer resume · *dep: 17b, 16c* · gate: a real fine-tune survives a killed process — **M3 exit**

**PR-18 `qlora-recipe` → 3**
- **18a** — `train` image + `qlora-sft` recipe manifest/config schema · *dep: 16b* · gate: image builds; schema validates
- **18b** — VRAM/cost estimator + `loom train` · *dep: 18a* · gate: `--dry-run` prints a bounded estimate
- **18c** — Real `qlora-sft` run → adapter + lineage record · *dep: 18b* · gate: adapter + lineage produced

**PR-19 `serve-vllm` → 2**
- **19a** — `serve-vllm` image + gateway SSE proxy + replica table · *dep: 13c, 16b* · gate: streams tokens from a real replica
- **19b** — `loom deploy adapter:` + restart-visible failover · *dep: 19a, 18c* · gate: deploy PR-18 adapter; killed replica → visible restart

**PR-20 `logs-ps-top` → 2**
- **20a** — SSE log streaming + resume token + `loom logs` · *dep: 13c* · gate: logs stream with resume
- **20b** — `ps` + `top` (real telemetry) · *dep: 20a, 16a* · gate: `top` shows real utilization

## Wave 4 — Self-host Hardening (12)

**PR-21 `tls-remote` → 2**
- **21a** — Self-signed cert at `loom init` + TLS listener · *dep: 11a* · gate: TLS listener up
- **21b** — CLI fingerprint pinning + non-loopback-requires-TLS guard · *dep: 21a, 10a* · gate: plain HTTP refused off-loopback

**PR-22 `private-fleet` · M4 → 3**
- **22a** — `loom-hostd enroll --server` + multi-node registration · *dep: 13c, 21b* · gate: a second node enrolls over TLS
- **22b** — Scheduler fan-out across nodes · *dep: 22a, 16a* · gate: `loom run --gpu all` fans out
- **22c** — Fleet resume drill (node death → resume elsewhere) · *dep: 22b, 17c* · gate: mid-job node death resumes on another node — **M4 exit**

**PR-23 `backup-upgrade` → 2**
- **23a** — `loom backup`/`restore` (`VACUUM INTO` + verify) · *dep: 05b, 11a* · gate: backup→restore round-trip verified
- **23b** — Staged `loomd upgrade` + auto-rollback + N−1 migration contract · *dep: 23a* · gate: crash-looping upgrade auto-rolls-back

**PR-24 `image-pipeline` → 3**
- **24a** — `base-cuda` reproducible build + digest pinning · *dep: 01b* · gate: reproducible; pinned by digest
- **24b** — `train` image · *dep: 24a* · gate: builds; used by PR-18
- **24c** — `serve-vllm` image + SBOM + scan in CI · *dep: 24a* · gate: builds; scanned clean

**PR-25 `observability` → 2**
- **25a** — `tracing` spans across crates · *dep: 11a* · gate: one job → coherent trace
- **25b** — Optional `/metrics` (off by default) + `doctor` completeness · *dep: 25a* · gate: metrics gated by config

## New PRs from the second review (6)

**PR-26 `bootstrap-auth` → 2**
- **26a** — Local CA + node-cert signing · *dep: 05a* · gate: signs a CSR; verifies chain
- **26b** — Enrollment-token issuance + local admin token + secrets file · *dep: 26a, 05c* · gate: `loomd init` mints CA+admin token; bad token refused

**PR-27 `usage-metering` → 2**
- **27a** — Signed `UsageRecord` ingest → store · *dep: 12, 13c* · gate: records persist; monotonic; deduped by `(attempt, seq)`
- **27b** — Per-second aggregation + accuracy test · *dep: 27a* · gate: metered GPU-seconds match wall-clock within tolerance

**PR-28 `secret-injection` → 2**
- **28a** — Sealed secret store + injection into sandbox env at launch · *dep: 13c, 26b* · gate: secret reaches workload env
- **28b** — Scrub-on-teardown + absence tests · *dep: 28a* · gate: secret absent from host FS + logs; scrubbed on teardown

---

## What this does and does not change

- **The epic-level DAG in [README.md](./README.md) is unchanged** — the dependency map between workstreams still holds; each edge now connects the *last* sub-PR of the upstream epic to the *first* of the downstream one (e.g. epic PR-11 depends on PR-09 means `11a` waits on `09c`).
- **The critical path shape is unchanged** — still single-owned, still sequential; only the *count* of PRs on it grows because each spine epic is now a sub-stack.
- **The invariant core stays coherent** — PR-03b and PR-12 are the deliberate large PRs; do not split them for the sake of the line-count rule.
- **Owner assignments carry down** — the sub-PRs of an epic inherit that epic's owner from [parallelization.md §2b](./parallelization.md#2b-23-engineers-the-realistic-default).
