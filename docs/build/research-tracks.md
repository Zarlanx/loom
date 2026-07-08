# Research tracks — parallel, non-blocking validation

**Status:** Build plan companion · July 2026 · owner: platform
**Scope:** The four validation spikes (T1–T4) that run *alongside* the [coding waves](./README.md) from day one. Each answers one load-bearing question, produces a decision or a dataset, and feeds a specific PR. **None of them gates the self-hostable core**, which ships on plain hardened-runc for a trusted user on their own hardware ([isolation.md §3](../platform/isolation.md)). This document names each track, its method, and — most importantly — exactly what it does *not* block.

These tracks are the build-plan's instantiation of the [design review's pre-build spike backlog](../architecture/design-review.md#4-recommended-pre-build-spikes) (S1–S8), narrowed to the four that touch the Phase-1 self-host path. The other spikes (S2, S4, S5, S7, S8) are real, filed, and out of Phase-1 scope — see [What we are NOT researching in Phase 1](#what-we-are-not-researching-in-phase-1).

---

## The tracks at a glance

| Track | Spike / Issue | Question | Feeds | Blocks the core? |
|-------|---------------|----------|-------|------------------|
| **T1 — nvproxy compatibility matrix** | S1 · [#11](https://github.com/Zarlanx/loom/issues/11) | Does gVisor `runsc + nvproxy` cover the CUDA/driver paths our 3 curated images hit on real consumer GPUs, and how narrow is the qualified driver window? | [PR-07](./README.md#wave-1--seams--skeletons), [PR-16](./README.md#wave-3--thicken-verticals); gates the **future** untrusted tier (Phase 3) | **No** |
| **T2 — Cloud Hypervisor VFIO qualification** | (Phase-3 de-risk) | Does CH + VFIO GPU passthrough work on Tier-A-class hardware, and what are the boot/reset quirks? | A future `CloudHypervisorDriver` in `loom-sandbox` | **No** |
| **T3 — vLLM cold-start on residential** | S3 · [#13](https://github.com/Zarlanx/loom/issues/13) | Is scale-to-zero minutes-scale — i.e. is keep-warm mandatory? | [PR-19](./README.md#wave-3--thicken-verticals) UX; Phase-2 keep-warm design | **No** |
| **T4 — checkpoint-under-eject drill** | S6 · [#16](https://github.com/Zarlanx/loom/issues/16) | Does checkpoint-resume hold for the *hard* case — large checkpoints on a slow uplink, mid-drain eject — not just tiny adapters? | [PR-17](./README.md#wave-3--thicken-verticals) | **No** |

All four run continuously from Wave 0. They land data and decisions *into* the coding waves; they never form a dependency edge in the [PR DAG](./README.md#3-the-authoritative-pr-dag).

---

## T1 — nvproxy compatibility matrix

*Spike S1 · GitHub issue #11 · **the most important track***

This is the roadmap's [#2 technical risk](../architecture/design-review.md#f2--gvisornvproxy-compatibility-is-unproven-on-exactly-the-consumer-cards-where-all-early-supply-lives-critical) turned into a Phase-1 experiment. gVisor's `nvproxy` officially supports datacenter silicon (T4, A100, A10G, L4, H100); the same-architecture *consumer* cards — the RTX 30/40/50-series that all early supply and every self-hoster's gaming rig actually run — are documented only as "likely work, aren't officially supported" ([isolation.md §3.3](../platform/isolation.md), [Open Question #1](../platform/isolation.md#9-open-questions)). "Likely" is not a build input. T1 replaces it with a matrix.

**Question.** Does gVisor's GPU passthrough (`runsc + nvproxy`) cover the exact CUDA and driver code paths our three Phase-1 workloads exercise, on the consumer cards we target, across the driver-version band the enrollment floor admits — and if it fails, *where* does it fail (which card, which driver, which ioctl, which workload phase)?

**Method.** Take the **three curated Phase-1 images** as-shipped — `base-cuda`, `train` (the `qlora-sft` recipe from [PR-18](./README.md#wave-3--thicken-verticals)), and `serve-vllm` ([PR-19](./README.md#wave-3--thicken-verticals)) — and run each end-to-end under `runsc + nvproxy` on **five real consumer GPUs**: RTX 3060, 4070, 4090, 5090, and 3090 (low-end Ampere through current Blackwell). For each `(card × image)` cell, sweep the **driver-version band** the enrollment floor admits, using `runsc nvproxy list-supported-drivers` to bound the window, and record for each combination: does the workload run to completion; which ioctls were rejected (if any); whether `--enforce-eager` vs. full CUDA-graph capture changes the result; whether managed-memory patterns (`cudaMallocManaged`, flaky on the KVM platform per [isolation.md §3.3](../platform/isolation.md)) are hit; and the syscall-interposition overhead on the IO-heavy phases. The 5090 is the explicit unknown — Blackwell/RTX-50 support in `runsc` is [flagged unverified for 2026 silicon](../platform/isolation.md#9-open-questions).

**Feeds.** [PR-07](./README.md#wave-1--seams--skeletons) (`sandbox-runc`) grows a *second* driver — the gVisor/nvproxy path — behind the same `SandboxDriver` trait; T1 tells that PR which cards and drivers the second driver may advertise. [PR-16](./README.md#wave-3--thicken-verticals) (`gpu-execution`) consumes the qualified driver band as its enrollment gate. Crucially, both are **informed, not blocked**: PR-07 ships the hardened `RuncDriver` regardless, and PR-16 runs real CUDA on plain runc regardless.

**Output / Decision.** A **per-card go/no-go matrix**: for each `(card, driver-band, image)` a verdict of *qualified* / *qualified-with-caveats* / *unsupported*, plus the failing ioctl or phase for every non-green cell. This is the qualification matrix [isolation.md already knows it needs](../platform/isolation.md#9-open-questions). The decision it drives is scoping, not go/no-go on the build: which cards are eligible for the *future* untrusted tier, and how narrow the qualified driver window is (which prices the supply loss for that tier later).

**Priority. Critical.** This is the single most important track. It is the design review's [F2](../architecture/design-review.md#f2--gvisornvproxy-compatibility-is-unproven-on-exactly-the-consumer-cards-where-all-early-supply-lives-critical), and the [post-pivot addendum](../architecture/design-review.md) confirms it *remains critical even for self-hosters* — because a self-hoster running an untrusted or poisoned dependency will want exactly the same nvproxy hardening Tier B provides.

**Non-gating note.** T1 blocks **nothing in Phase 1**. The self-host core defaults to plain hardened-runc, which is correct for a trusted user on their own hardware ([isolation.md §3.1](../platform/isolation.md)) — a self-hoster is not their own adversary. nvproxy qualification gates the **future untrusted/marketplace tier (Phase 3)**, where strangers' code runs on strangers' hardware. Running T1 in Phase 1 — while the core is happily shipping on runc — is precisely the point: it converts a Phase-3 *assumption* ("nvproxy will cover consumer cards when we need it") into Phase-1 *data*, at the moment GPUs and driver bands are cheapest to sweep and the answer is most useful. If T1 comes back badly (say nvproxy is broken across the 50-series driver band), the Phase-1 response is a **scoping decision** — "Blackwell is untrusted-tier-ineligible until upstream catches up" — recorded against the future tier. The core does not stall, because the core never depended on nvproxy in the first place.

---

## T2 — Cloud Hypervisor VFIO qualification

*Phase-3 de-risk · lower priority*

Tier A — a whole real GPU passed through to a microVM with its own kernel — is a categorically stronger boundary than any container ([isolation.md §4](../platform/isolation.md)), and it is the isolation tier for dedicated/headless rigs. It is also entirely a *future* concern: Phase 1 has no untrusted tier at all. T2 is a de-risking probe, not a Phase-1 deliverable, run so that when a `CloudHypervisorDriver` is eventually written we already know the hardware behaves.

**Question.** Does Cloud Hypervisor + VFIO GPU passthrough work end-to-end on the hardware we'd target for Tier A, and what are the boot-time and GPU-reset quirks in practice? ([isolation.md Open Questions #4 and #5](../platform/isolation.md#9-open-questions) flag CH IOMMU-group edge cases and per-job boot cost as unresolved.)

**Method.** On a **dedicated rig** with a headless/secondary GPU in a clean IOMMU group: rebind the card from the NVIDIA driver to `vfio-pci`, boot a minimal Cloud Hypervisor microVM with the card cold-plugged, and run **one GPU workload end-to-end** inside the guest (a small CUDA job is sufficient — this is a "does the boundary work at all" probe, not a matrix). Measure VM boot-to-GPU-ready time, exercise the [teardown + GPU-reset/FLR path](../platform/host-agent.md) across a few job cycles, and note any IOMMU-group creation issues (the known upstream [kata-containers#11687](https://github.com/kata-containers/kata-containers/issues/11687) class of bug) or reset quirks that would block safe card reuse.

**Feeds.** A future `CloudHypervisorDriver` implementation in `loom-sandbox` (a third `SandboxDriver` behind the same trait as runc and the eventual nvproxy path). Nothing in the Phase-1 PR DAG.

**Output / Decision.** A short **go/no-go** on CH+VFIO for our target Tier-A hardware, plus a boot-time number and a list of reset/IOMMU quirks to design around. If CH misbehaves, the documented fallback is Kata/QEMU ([isolation.md §4.2](../platform/isolation.md)) — T2's job is to tell us whether we'll need it.

**Priority. Low — de-risk only.** Unlike T1 and T4, nothing in Phase 1 rides on T2. It exists so the Tier-A path isn't a cold start when Phase 3 arrives.

**Non-gating note.** T2 is **explicitly deferred** and is not on the Phase-1 path. Tier A is a future tier; the self-host core ships without it. This track is pure future-de-risk: worth doing early because dedicated rigs and IOMMU debugging are slow to set up, but it feeds no Phase-1 PR and blocks nothing.

---

## T3 — vLLM cold-start on residential

*Spike S3 · GitHub issue #13*

Serving's own conclusion is blunt: "even the optimized floor is minutes-scale (download-dominated); scale-to-zero is inherently a minutes-scale cold start" ([serving.md §4](../ml-lifecycle/serving.md)). That claim is load-bearing for the entire serverless product and its pricing, but it rests on estimated bandwidth math, not a measured breakdown on a real home connection. T3 measures it.

**Question.** On a real residential connection, how long is a cold start — decomposed into weight-pull, engine-load, and CUDA-graph capture — for representative models, and is it minutes-scale enough that **keep-warm is mandatory** rather than a nice-to-have?

**Method.** On a real home connection (~30 Mbps up / ~300 Mbps down), cold-start **vLLM for a 14B and a 32B AWQ model** and record the honest breakdown from [serving.md §4](../ml-lifecycle/serving.md): (1) **weight download** — tens of GB over the residential downlink; (2) **engine load + CUDA-graph capture** — vLLM graph capture across batch sizes plus any `torch.compile`/Inductor first-run compilation. Run each with graph capture on and with `--enforce-eager`, and re-run to measure the on-node compiled-graph cache hit ([serving.md §4 mitigations](../ml-lifecycle/serving.md)). The two model sizes bracket the shared-endpoint sweet spot (14B) and the tight-fit high-end (32B).

**Feeds.** [PR-19](./README.md#wave-3--thicken-verticals) (`serve-vllm`) UX — specifically the "warming" response, the long-poll behavior, and how the CLI communicates a minutes-scale cold start honestly — and the **Phase-2 keep-warm design** ([serving.md §4 conclusion](../ml-lifecycle/serving.md)).

**Output / Decision.** A **cold-start breakdown** — real seconds/minutes per stage per model — and the decision it settles: whether keep-warm is the sole viable serverless UX (making scale-to-zero a documented-cold fallback) or whether an optimized cold path is fast enough to offer bare. This is the design review's [F4](../architecture/design-review.md#f4--cold-start-on-cgnat-is-minutes-scale-and-keep-warm-may-be-the-only-viable-serverless-ux--but-keep-warm-pricing-rests-on-an-unvalidated-host-behavior-assumption) cold-start half, measured.

**Priority. Medium.** It informs Phase-1 serve UX ([PR-19](./README.md#wave-3--thicken-verticals)) and de-risks a Phase-2 design pillar, but the self-host serve path works regardless of the number.

**Non-gating note.** T3 does not block [PR-19](./README.md#wave-3--thicken-verticals) or anything else. A self-hoster serving on their own hardware over their own LAN doesn't hit the residential-WAN cold-start floor at all — the weights are local. T3's number shapes the *hosted/marketplace* keep-warm story; for the self-host core it's an informative UX input, never a gate.

---

## T4 — checkpoint-under-eject drill

*Spike S6 · GitHub issue #16*

Checkpoint-resume is the [roadmap's stated #1 risk](../architecture/design-review.md#f10--residential-upload-30-mbps-vs-the-eject-checkpoint-window-is-an-unsolved-sla-collision-for-full-model-checkpoints) and a Phase-1 *exit criterion* ([README hard call #5](./README.md#1-the-five-hard-calls-read-this-before-the-table)). The [build plan proves the mechanics on fakes first](./README.md#1-the-five-hard-calls-read-this-before-the-table) — the fencing, lineage, and exact-step restore are chaos-tested with zero GPUs in [PR-12](./README.md#wave-2--walking-skeleton-the-join)/[PR-17](./README.md#wave-3--thicken-verticals). T4 is the complementary reality check: the fake fleet proves the *logic*; T4 proves the *physics*. The design admits the resume promise is comfortable for tiny LoRA adapters ("seconds") but unproven for the hard case — a multi-GB checkpoint on a slow uplink ([training.md §3](../ml-lifecycle/training.md), [F10](../architecture/design-review.md#f10--residential-upload-30-mbps-vs-the-eject-checkpoint-window-is-an-unsolved-sla-collision-for-full-model-checkpoints)).

**Question.** Does the checkpoint-resume promise hold for the **hard** case — a large (multi-GB) checkpoint, an owner-eject triggered *mid-drain*, on a ~30 Mbps residential uplink — and if so, how much work is lost and how does resume behave?

**Method.** Run a real job that produces a **multi-GB checkpoint** (a full-FT or from-scratch job, not a tens-of-MB adapter). Mid-checkpoint-drain, **trigger an owner-eject** — the owner-wins guarantee ([host-agent.md §8](../platform/host-agent.md)) firing at the worst possible moment — on a ~30 Mbps uplink. Measure: **work lost** (steps between last completed drain and the eject), whether the incremental/async-shard-upload design from [training.md §3](../ml-lifecycle/training.md) leaves a resumable partial, and the **resume behavior** on requeue elsewhere (exact-step + RNG restore, fence strictly greater). Contrast against the same drill with a small adapter to quantify the gap the design predicts.

**Feeds.** [PR-17](./README.md#wave-3--thicken-verticals) (`checkpoint-resume`) — T4 validates PR-17's incremental/async-upload design against real upload physics, and tells it whether the Phase-1 exit criterion's real-GPU leg must cover a full-model checkpoint (not just an adapter) to be honest.

**Output / Decision.** A **work-lost number** and a resume-behavior verdict for the hard case, plus the policy decision [training.md](../ml-lifecycle/training.md) leaves open: gate full-FT recipes to higher-uplink nodes, lengthen the checkpoint interval so eject-window loss is bounded and priced, or build peer-handoff. This directly de-risks the roadmap's #1 risk.

**Priority. Critical (with T1).** It is the reality check on *the* core differentiator and Phase-1 exit criterion.

**Non-gating note.** T4 does not block [PR-17](./README.md#wave-3--thicken-verticals) or the core. The checkpoint *mechanics* are built and chaos-tested on the fake fleet independently of T4 ([PR-12](./README.md#wave-2--walking-skeleton-the-join)/[PR-17](./README.md#wave-3--thicken-verticals)); T4 supplies the real-uplink dataset that tunes grace windows and sets the full-FT policy. If T4 shows full-model checkpoints can't drain inside a viable eject window on 30 Mbps, the response is a **scoping/pricing decision** (gate full-FT to higher-uplink nodes, or bound-and-price the loss) — the checkpoint-resume feature still ships for the adapter case that is [genuinely fine](../architecture/design-review.md#f10--residential-upload-30-mbps-vs-the-eject-checkpoint-window-is-an-unsolved-sla-collision-for-full-model-checkpoints).

---

## How research tracks interact with the coding waves

The tracks and the [waves](./README.md#2-shape-of-the-plan) run on **two separate clocks that never form a dependency edge.**

- **They start at Wave 0 and run continuously.** T1's GPUs, T2's dedicated rig, T3's home connection, and T4's multi-GB job all take real setup time and real hardware, so they begin the moment the workspace exists — in parallel with contracts and skeletons — and stream results throughout the build. They do not wait for a PR and no PR waits for them.
- **Their outputs land as data and decisions, not as blockers.** A track's deliverable is a matrix (T1), a go/no-go (T2), a breakdown (T3), or a number-plus-policy (T4). That artifact *informs* its target PR — PR-07/PR-16 (T1), the future CH driver (T2), PR-19 (T3), PR-17 (T4) — the way a spec informs an implementation, not the way a merged dependency unblocks one. Every one of these PRs can be written, reviewed, and merged against the **plain-runc / fake-fleet / local-serve** baseline before its track finishes. The core is provable without any of them.
- **A surfaced blocker triggers a scoping decision, never a core stall.** This is the whole discipline. If T1 finds nvproxy broken on the 50-series driver band, the answer is "Blackwell is untrusted-tier-ineligible until upstream lands support" — a note against the *future* tier, not a halt on [PR-07](./README.md#wave-1--seams--skeletons), which ships the hardened runc driver regardless. If T4 finds full-model checkpoints can't drain on 30 Mbps, the answer is a full-FT uplink gate or a priced work-loss bound — [PR-17](./README.md#wave-3--thicken-verticals) still ships checkpoint-resume for adapters. If T3 finds cold-start is 12 minutes, [PR-19](./README.md#wave-3--thicken-verticals) says so honestly in its UX. The core [ships on plain hardened-runc](./README.md#1-the-five-hard-calls-read-this-before-the-table) throughout; the tracks change *what we scope into the future untrusted/hosted tiers*, never whether the Phase-1 spine lands.

This mirrors the [build plan's own framing](./README.md#1-the-five-hard-calls-read-this-before-the-table): *"Isolation research runs from day one but gates nothing in the core."* The same holds for all four tracks.

---

## What we are NOT researching in Phase 1

The [design review's spike backlog](../architecture/design-review.md#4-recommended-pre-build-spikes) has eight entries (S1–S8). Only the four above touch the Phase-1 self-host path. The rest are real and filed as GitHub issues, but they de-risk the deferred marketplace/hosted layers and are **explicitly out of Phase-1 scope**:

- **ROCm / AMD qualification** — reset-probe reliability and vLLM-on-ROCm coverage ([isolation.md OQ #6](../platform/isolation.md#9-open-questions)). Phase 1 is [NVIDIA-first](../platform/isolation.md); ROCm is a fast-follow.
- **TEE / Tier-C attestation** (S8-adjacent) — SEV-SNP/TDX confidential-compute qualification. Tier C is Phase 5 and years out.
- **Multi-region relays / NAT-traversal probe** (S2, issue #12) — real direct-vs-relay punch rate for our population ([networking.md OQ #1](../platform/networking.md)). A marketplace-transport concern; the self-host core runs over LAN/WireGuard with no relay.
- **Marketplace fraud & integrity economics** (S5/S7, issues #15/#17) — support-cost-per-account ([F3](../architecture/design-review.md#f3--support-cost-per-account-is-a-guess-that-dominates-margin-flips-whole-gpu-classes-negative-and-has-no-headcount-in-the-plan)) and result-integrity sampling economics ([F6](../architecture/design-review.md#f6--result-integrity-spot-checking-economics-are-asserted-never-solved-at-a-20-take-that-cant-afford-redundancy)). Both are marketplace-layer, deferred with the hosted product.
- **Compliance / liability legal read** (S8, issue #18) — sub-processor liability under honest labeling ([F11](../architecture/design-review.md#f11--legal-exposure-operating-a-marketplace-where-hosts-can-demonstrably-read-renter-data--is-honest-labeling-a-sufficient-legal-shield)). Needed before public launch (Phase 3), not before the self-host core.

Per the [post-pivot addendum](../architecture/design-review.md), the self-host-first refocus moves all of these to the deferred layers — they remain open questions, but none blocks the Phase-1 self-hostable core.

---

*Cross-references: [./README.md](./README.md) (the authoritative PR DAG and hard calls) · [../architecture/design-review.md](../architecture/design-review.md) (the S1–S8 spike backlog and findings F2/F4/F10) · [../platform/isolation.md](../platform/isolation.md) (gVisor/nvproxy + Cloud Hypervisor tiers) · [../ml-lifecycle/serving.md](../ml-lifecycle/serving.md) (vLLM cold-start).*
