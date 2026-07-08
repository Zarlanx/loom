# Milestones — Phase 1 (self-hostable core)

**Status:** Implementation plan · July 2026 · owner: platform
**Scope:** The five milestones (M0–M4) that Phase 1 lands as, the concrete pass/fail exit criteria for each, and the command transcript that a human runs to *see* it working. This document is subordinate to the [build plan](./README.md): it groups the authoritative PR DAG into demoable checkpoints and never redefines a PR's scope or dependencies — every PR is defined once, in [README.md §3](./README.md), and referenced here by ID.

A milestone is not a bucket of PRs; it is a **demoable, risk-retiring checkpoint**. PRs are the unit of *work*; milestones are the unit of *proof* — the moment where a set of merged PRs first makes a sentence like "you can fine-tune and resume across a killed process" literally true and testable. Each milestone below answers one question, closes with one transcript anyone can run, and retires one named risk; if the demo doesn't pass, the milestone hasn't landed no matter how many PRs are green.

| Milestone | PRs | The question it answers | GPU needed? |
|-----------|-----|-------------------------|-------------|
| **M0** scaffold | [PR-01](./README.md)–[PR-04](./README.md) | Can a new engineer build the workspace and read the frozen contracts? | No |
| **M1** walking skeleton | [PR-05](./README.md)–[PR-13](./README.md) | Does a job flow through the entire spine end-to-end? | No |
| **M2** real metal | [PR-14](./README.md), [PR-16](./README.md), [PR-20](./README.md), [PR-24](./README.md) | Can a real GPU job run on the first verified backend (MLX on the M3 Max) with logs streaming? | **Yes** |
| **M3** train + resume | [PR-15](./README.md), [PR-17](./README.md), [PR-18](./README.md) | Can a `qlora-sft` fine-tune survive a killed process and resume? | **Yes** |
| **M4** serve + self-host | [PR-19](./README.md), [PR-21](./README.md), [PR-22](./README.md), [PR-23](./README.md), [PR-25](./README.md) | Can a stranger self-host the whole stack and serve the adapter? | **Yes** |

> **Backend-first re-anchoring (2026-07-08, [ADR-0015](../adr/0015-pluggable-compute-backends.md) / [README §7](./README.md#7-backend-first-revision-2026-07-08-mlx-on-the-founders-metal)).** The hardware milestones M2–M4 are verified on the **MLX backend on the founder's M3 Max via the ProcessDriver** — M2's "real GPU job" is a Metal job, M3's fine-tune is `mlx-lm` LoRA, M4's served engine is the MLX server. The transcripts below were written for the Linux/CUDA flow; they remain valid as that backend's versions and will be joined by macOS transcripts as the MLX legs land. Two caveats: the CUDA legs (07c/07d, 17d, 18d, 19c) re-run these milestones on NVIDIA hardware when it exists, and **M4's fleet-resume drill (22c) needs a second machine** — it stays open until one exists; everything else in M4 closes on the M3 alone.

The two [roadmap Phase-1 exit criteria](../product/roadmap.md#phase-1--self-hostable-core) — "<15 min self-host + resumable fine-tune" and "25 jobs / 10 users on a private fleet with checkpointed-resume" — are closed by **M3** and **M4** respectively. The test tiers referenced throughout (unit / store-conformance / protocol golden vectors / simulated-fleet chaos / gateway-failover / hardware-gated GPU smoke) are defined in [backend.md §9](../platform/backend.md). Staffing, the critical path, and how the waves run in parallel are in [parallelization.md](./parallelization.md).

---

## M0 — Scaffold

**Goal.** After M0, the two contracts the whole team waits on are frozen and the workspace compiles green in CI. A new engineer can `git clone`, `cargo build`, and read the OpenAPI spec and the wire protocol without a single feature existing yet. This is the milestone that makes the rest of Phase 1 *parallelizable*: with `loom-proto` and the OpenAPI spec landed, the CLI, the server, and the agent can be built by different people at the same time without blocking each other.

**Composed of.**

- [PR-01](./README.md) `workspace-scaffold` — the Cargo workspace, all 10 crate stubs compiling empty, shared lints, `rust-toolchain`, CI (fmt/clippy/test), `xtask` stub.
- [PR-02](./README.md) `proto-contract` — `loom-proto`: the `.proto` `Envelope` + full message catalog, `prost-build` codegen, length-prefix codec, golden vectors.
- [PR-03](./README.md) `core-domain` — `loom-core`: domain types + pure state machines (job lifecycle, lease/fencing, scheduler filter→score→commit), zero I/O.
- [PR-04](./README.md) `openapi-contract` — the committed target OpenAPI spec, the CI spec-diff gate, and a mock server for CLI development.

**Exit criteria** (all pass/fail):

- `cargo build && cargo clippy -D warnings` is green in CI on the empty workspace.
- `loom-proto` golden-vector round-trip test passes: a fake agent and the server decode the same bytes.
- `loom-core` FSM unit + property tests pass, including requeue-lineage and fencing cases.
- The OpenAPI spec validates, the diff-gate CI job runs, and the mock server answers every golden-path route.

**Demo script.** M0's "demo" is that a newcomer is productive in minutes:

```
$ git clone https://github.com/loom/loom && cd loom
$ cargo build
   Compiling loom-proto v0.1.0
   Compiling loom-core v0.1.0
   ...
   Compiling loom v0.1.0
    Finished dev [unoptimized] in 41.2s
$ cargo xtask verify-golden
  loom-proto golden vectors: 37/37 round-trip ✓
$ cargo xtask openapi-diff
  openapi.yaml matches committed spec ✓  (mock server: 12/12 golden routes)
```

**Verified in CI vs. on hardware.** Entirely in CI, no GPU. M0 is closed by the **unit** tier (`loom-core` FSMs) and the **protocol golden-vector** tier ([backend.md §9](../platform/backend.md)); the OpenAPI diff-gate is a CI job. Nothing here touches a card.

**The risk this milestone retires.** "The contracts drift and every downstream crate integrates against a moving target." Freezing `loom-proto` and the OpenAPI spec — additive-only thereafter — is what makes the parallel build safe.

---

## M1 — Walking skeleton

**Goal.** After M1, a no-GPU `echo` job flows through the *entire spine* — CLI → `loomd` → store → scheduler → agent-gateway → wire protocol → `loom-hostd` → runc → log stream back → terminal state. Every crate exists in skeletal form and is *wired* before any crate is *finished*. This is the single most important milestone in the phase: if `loom run -- echo hi` doesn't work end-to-end, nothing else we build is real. It also lands the invariant core — the single-writer scheduler with lease-and-fencing — proven against a chaos harness with zero GPUs.

**Composed of.**

- [PR-05](./README.md) `store-sqlite` — `Store` trait + `SqliteStore` + migrations + file-backed WAL conformance suite.
- [PR-06](./README.md) `bus-inproc` — `Bus` trait + `InProcBus` + outbox-relay task.
- [PR-07](./README.md) `sandbox-runc` — `SandboxDriver` trait + hardened `RuncDriver` (seccomp, dropped caps, cgroup v2, default-deny egress). No GPU yet.
- [PR-08](./README.md) `hostd-skeleton` — `loom-hostd`: config, WSS control channel, enrollment CSR handshake, agent FSM, heartbeat, spool.
- [PR-09](./README.md) `agentgateway` — `loom-agentproto`: server-side WSS/quinn terminator, mTLS identity, 4-stream demux, bridge to `Bus`, cert issuance on enroll.
- [PR-10](./README.md) `cli-skeleton` — the `loom` CLI: `clap` tree, local-token auth, HTTP client against the OpenAPI contract, streaming output.
- [PR-11](./README.md) `loomd-skeleton` — `loomd`: process wiring (store + bus + axum app with real job submit/get/list + agent-gateway), `loom.toml`, `loomd init`/`doctor`.
- [PR-12](./README.md) `scheduler-v0` — the single-writer reconciliation loop, lease-with-fencing, requeue-on-lost.
- [PR-13](./README.md) `tracer-bullet` **· M1** — the end-to-end no-GPU integration that joins all of the above.

**Exit criteria** (all pass/fail):

- The M1 demo passes in CI: `loomd` + `loom-hostd` in one test, a container runs `echo`, logs return, the job reaches `Succeeded`.
- Store conformance is green against a **file-backed WAL** SQLite database (not `:memory:`).
- The simulated-fleet chaos test passes: every lost attempt requeues with a **strictly greater fence**; no double-lease; no double-bill.
- `loomd` boots standalone, serves the API on SQLite, and accepts a `loom-hostd` connection.

**Demo script.** The tracer bullet — the spine proven:

```
$ loom init --standalone
  ✓ standalone Loom is up.  API: http://127.0.0.1:8443  admin token: loom_admin_7f3a…
$ loom run -- echo hi
  [prepare]  node local selected · container starting (runc)  ✔
  [run]      hi
  [done]     exit 0 · 00:00 wall  · job job_2f1c Succeeded
```

No GPU, no image pull, no card — a locked-down runc container runs `echo hi`, the log line streams back over the agent-gateway, and the job settles to `Succeeded`.

**Verified in CI vs. on hardware.** Entirely in CI, no GPU. M1 is closed by the **store-conformance** tier (file-backed WAL), the **simulated-fleet chaos** tier (N fake agents, random disconnects, stale-fence late writes), and the tracer-bullet integration — all of which run in-process because every service is a library ([backend.md §9](../platform/backend.md)). The `RuncDriver` egress-deny path uses a fake driver for CI-without-root.

**The risk this milestone retires.** "The ten crates never actually integrate" — the three-weeks-of-integration-hell failure mode. Wiring the whole spine before finishing any crate kills it. M1 also retires "split-brain: two nodes run and bill the same attempt," because the fencing invariant is proven under chaos here, on fakes, before any real GPU exists.

---

## M2 — Real GPU

**Goal.** After M2, a real CUDA job runs sandboxed on your own GPU. `loom-hostd` inventories the card via NVML, gates enrollment on the driver floor, injects the GPU into the sandbox via the nvidia-container-toolkit, and the curated images pull and cache. Live logs and GPU telemetry stream to `loom logs`/`ps`/`top`. This is the seam-swap the whole plan was designed around: the scheduler, store, protocol, and gateway were all proven on fakes in M1, so bringing in a real card is a driver swap behind `SandboxDriver`, not a from-scratch build.

**Composed of.**

- [PR-14](./README.md) `artifact-store` — local content-addressed on-disk store + presign-style local URLs + GC/keep-last-N.
- [PR-16](./README.md) `gpu-execution` — real-GPU job: NVML inventory, driver-floor enrollment gate, GPU injection into the sandbox, the hardware-gated GPU smoke suite.
- [PR-20](./README.md) `logs-ps-top` — end-to-end SSE log streaming + `loom logs`/`ps`/`top` (live GPU telemetry from heartbeats).
- [PR-24](./README.md) `image-pipeline` — the three curated images (`base-cuda`/`train`/`serve-vllm`): reproducible builds, digest pinning, SBOM + scan, direct pull + local cache.

**Exit criteria** (all pass/fail):

- A real tiny CUDA job runs on an actual GPU box and teardown verifies clean; the same suite **skips cleanly** with no GPU present.
- The three curated images build reproducibly in CI, are pinned by digest, and scan clean.
- Live logs stream with a resume token; `loom top` shows real GPU utilization from heartbeats.
- The artifact store round-trips put/get by digest and GC keeps last N.

**Demo script.** A real card runs a real CUDA job:

```
$ loom run --gpu auto -- python -c "import torch; print(torch.cuda.get_device_name())"
  [prepare]  node local selected
  [prepare]  image loom/base-cuda:2026.07 not resident — pulling ███████ 100%  ✔
  [run]      NVIDIA GeForce RTX 4090
  [done]     exit 0 · 00:41 wall
$ loom top
  NODE   GPU        UTIL   VRAM         JOB
  local  rtx4090    97%    13.9/24 GB   job_9a2e (running)
```

**Verified in CI vs. on hardware.** The image pipeline and artifact store close **in CI** (reproducible builds, digest pins, GC tests). Real GPU execution requires the **hardware-gated GPU smoke** tier ([backend.md §9](../platform/backend.md)) — a real `loom-hostd` on an actual card enrolled against a real `loomd` — which skips cleanly when no GPU is present, the same skip-without-hardware discipline used for live-integration tests. M2 is the first milestone that cannot fully close in CI.

**The risk this milestone retires.** "Real GPUs don't behave like the fakes" — driver floors, NVML quirks, toolkit injection, teardown leaks. Deferring real hardware to one vertical behind the sandbox seam means GPU scarcity never bottlenecked the ~80% of the backend that didn't need a card, and the seam-swap either works or fails loudly against a real device.

---

## M3 — Train + resume

**Goal.** After M3, a `qlora-sft` fine-tune runs to an adapter *and survives a killed process* — checkpoint on eject with grace, incremental upload, resume-from-checkpoint requeue with fencing and exact-step/RNG restore. This is the roadmap's **headline Phase-1 exit criterion** and its **stated #1 risk**: "checkpoint-resume across vanishing nodes is genuinely hard and is our core promise — if it's flaky, we have nothing." It closes the first roadmap exit criterion: a stranger can self-host on one box and complete a **resumable** fine-tune.

**Composed of.**

- [PR-15](./README.md) `data-push` — `loom data push`: manifest + chunking + upload to the artifact store + node prefetch + `name@vN` refs.
- [PR-17](./README.md) `checkpoint-resume` **· M3** — `loom-ckpt` (HF Trainer callback), checkpoint-now-on-eject with grace, incremental upload, resume-from-checkpoint requeue with fencing + exact-step/RNG restore.
- [PR-18](./README.md) `qlora-recipe` — the `qlora-sft` recipe manifest + config schema + VRAM/cost estimator + the `train` image; `loom train --recipe qlora-sft`.

**Exit criteria** (all pass/fail):

- Kill a `qlora-sft` job mid-run → it **resumes to completion** from the exact step, first in the fake fleet, then on real hardware. This is the [roadmap exit criterion](../product/roadmap.md#phase-1--self-hostable-core).
- `loom data push` produces an immutable `name@vN` manifest; a job references the dataset; a warm re-run is a cache hit.
- A real small `qlora-sft` run yields an adapter + a lineage record + an eval stub.
- The resume requeue carries a **strictly greater fence** — no double-run, no double-bill of the resumed attempt.

**Demo script.** The kill-and-resume drill — the headline:

```
$ loom data push ./sft_data.jsonl --name my-sft
  chunking + hashing ... 52 chunks · manifest: my-sft@v1  sha256:9f3c… (immutable)

$ loom train --recipe qlora-sft \
    --base meta-llama/Llama-3.1-8B --data my-sft@v1 --gpu auto --epochs 3 --yes
  [run]      QLoRA · micro_batch=8 (auto) · grad_ckpt on
  [step 200] loss 1.412 · ckpt@a17e9f saved locally · 00:18 elapsed
  [step 900] loss 1.031 · 00:47 elapsed

# ... in another terminal, kill it mid-run ...
$ loom-hostd eject            # or: kill -9 the job process
  ejecting job_3d7a — checkpointing via loom-ckpt (grace 30s) … ckpt@c4e1 uploaded ✔

# ... loomd requeues with a greater fence; the job resumes from the exact step ...
$ loom logs job_3d7a --follow
  [resume]   from ckpt@c4e1 · step 900 · RNG restored · fence 3→4
  [step 3600] loss 0.887 · 01:52 elapsed
  [done]     checkpoint ckpt@a17e9f · adapter (74 MB) + model card
  [eval]     instruction-following → report ev@7b1a (score 0.68) ✔
```

**Verified in CI vs. on hardware.** Two-stage by design ([README.md §1, hard call #5](./README.md)). The checkpoint → requeue → resume machinery — fencing, lineage, exact-step restore — is proven **in CI** first via the **simulated-fleet chaos** tier (fake agents, zero GPUs, owner-ejects and stale-fence late writes). Then the same path is verified **on hardware** via the **hardware-gated GPU smoke** tier ([backend.md §9](../platform/backend.md)): a real `qlora-sft` run killed and resumed on a real card. Proving correctness on fakes first is what makes the real-GPU version a seam-swap rather than a debugging exercise under GPU cost.

**The risk this milestone retires.** The roadmap's **#1 risk**: flaky checkpoint-resume. It was built early on fakes and chaos-tested before real GPUs precisely so that "resume across a killed process" is a proven invariant, not a hope. If M3's demo passes, the core promise of Loom is real.

---

## M4 — Serve + self-host

**Goal.** After M4, the full self-host story is production-honest: deploy the M3 adapter to a **local OpenAI-compatible endpoint** and curl it, run the CLI **remotely over TLS** with a pinned fingerprint, fan work across a **private fleet** of your own machines with a mid-job node death resuming elsewhere, and back up / upgrade with automatic rollback. This closes the second [roadmap exit criterion](../product/roadmap.md#phase-1--self-hostable-core): 25 jobs from 10 users on a private fleet with checkpointed-resume demonstrated.

**Composed of.**

- [PR-19](./README.md) `serve-vllm` — embedded `loom-gateway` SSE proxy + replica table from heartbeats + `loom deploy adapter:` + `serve-vllm` image + restart-visible failover.
- [PR-21](./README.md) `tls-remote` — self-signed cert at `loom init`, CLI fingerprint pinning, non-loopback bind requires TLS.
- [PR-22](./README.md) `private-fleet` **· M4** — `loom-hostd enroll --server`, multi-node scheduling fan-out, LAN/WireGuard data plane.
- [PR-23](./README.md) `backup-upgrade` — `loom backup`/`restore` (`VACUUM INTO` + verify), N−1 migration contract, staged `loomd upgrade` + auto-rollback.
- [PR-25](./README.md) `observability` — `tracing` spans across crates, optional `/metrics` (off by default in standalone), `loomd doctor` completeness.

**Exit criteria** (all pass/fail):

- Deploy the PR-18 adapter, curl the local OpenAI endpoint, and get a completion; a killed replica surfaces a visible restart event.
- Three machines: `loom run --gpu all` fans out; a mid-job node death **resumes elsewhere** without renter intervention — the [fleet exit criterion](../product/roadmap.md#phase-1--self-hostable-core).
- 25 jobs from 10 users complete on the private fleet with checkpointed-resume demonstrated.
- Remote CLI over TLS with a pinned fingerprint works; plain HTTP is refused off-loopback.
- `loom backup`→`restore` round-trips and verifies; a crash-looping `loomd upgrade` auto-rolls-back.

**Demo script.** Deploy + curl the local endpoint, then fan out across the fleet:

```
# 1. Deploy the M3 adapter behind the embedded OpenAI-compatible gateway.
$ loom deploy adapter:a17e9f --name my-model
  adapter placed on local base replica (llama-3.1-8b)
  → http://127.0.0.1:8443/v1   (model = "my-model")

$ curl http://127.0.0.1:8443/v1/chat/completions \
    -H "Authorization: Bearer $LOOM_ADMIN_TOKEN" \
    -d '{"model":"my-model","messages":[{"role":"user","content":"hi"}]}'
  {"choices":[{"message":{"role":"assistant","content":"Hello! …"}}], …}

# 2. Enroll two more rigs, then fan a job across the fleet.
$ loom-hostd enroll --server https://loom.mytailnet:8443 --token loom_admin_7f3a…
  enrolled as node "rig-2" (rtx3090) ✓

$ loom run --gpu all -- python bench.py
  [prepare]  fanning to 3 nodes: local, rig-2, rig-3
  ✓ local   (rtx4090)  done  00:58
  ✗ rig-2   (rtx3090)  node lost mid-job — requeued, resumed on rig-3 (fence 5→6)
  ✓ rig-3   (rtx3090)  done  01:31
```

**Verified in CI vs. on hardware.** Split. Gateway failover closes **in CI** via the **gateway-failover** tier (a fake vLLM killed mid-generation, asserting re-dispatch and a visible restart event). TLS pinning, backup/restore round-trip, and the auto-rollback upgrade close **in CI**. The private-fleet fan-out and the multi-node death-and-resume are proven in CI first under the **simulated-fleet chaos** tier and then confirmed on the **hardware-gated GPU smoke** tier with the seed private fleet ([backend.md §9](../platform/backend.md)). The 25-jobs/10-users criterion is a hardware exercise on the seed fleet.

**The risk this milestone retires.** "The self-host story falls apart the moment it leaves one box" — remote access without TLS, a fleet that can't survive a node death, an upgrade that bricks the coordinator with no way back. M4 makes each of those a tested pass/fail, so "a stranger self-hosts the whole stack" is a claim backed by a runnable demo.

---

## How the milestones ladder up

M0 freezes the contracts; M1 proves the spine on fakes; M2 swaps in a real card behind the same seams; M3 lands the headline resumable-fine-tune promise; M4 makes it a complete, remote, fleet-capable, upgradeable self-host product. The two roadmap Phase-1 exit criteria are closed at M3 (<15 min self-host + resumable fine-tune) and M4 (25 jobs / 10 users with checkpointed-resume). Everything before the GPU line (M0, M1) closes purely in CI on fakes; everything from M2 onward needs the hardware-gated suite — the deliberate consequence of building the risky machinery on fakes first.

*Cross-references: [README.md](./README.md) (the authoritative PR DAG) · [parallelization.md](./parallelization.md) (staffing, critical path, sync points) · [../product/roadmap.md](../product/roadmap.md) (Phase 1 scope + exit criteria) · [../platform/backend.md](../platform/backend.md) (§9 testing tiers).*
