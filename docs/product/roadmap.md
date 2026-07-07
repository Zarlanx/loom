# Roadmap: from design to confidential tier

This is the phased build plan for Loom. It sequences the [marketplace mechanics](./marketplace.md), the [isolation tiers](../platform/isolation.md) and [security model](../platform/security.md), the [serving path](../ml-lifecycle/serving.md), and the [host/renter UX](./deployment.md) into shippable increments. Each phase ships something a real user touches; each has explicit non-goals so we don't gold-plate a phase we haven't validated demand for.

**Sizing assumption throughout:** a team of **2–3 strong engineers** (systems-Rust for the agent, distributed-systems for the control plane, ML-infra for the serving/lifecycle layer). Engineer-month (EM) figures are calendar-inclusive of design, build, and hardening — deliberately rough. "Nodes are cattle" is assumed at every phase, not a phase of its own.

## Phase table

### Phase 0 — Design (now)

- **Scope:** This repo. Full design docs across architecture, platform, ML lifecycle, and product. Threat model, trust tiers, marketplace mechanics, roadmap all written down and internally consistent.
- **Non-goals:** No code. No infra. No premature framework choices baked past what the docs commit to.
- **Exit criteria:** Design docs complete and cross-consistent; a new engineer can read `docs/` and understand what we're building and why; the Phase 1 scope is unambiguous.
- **Sizing:** ~1–2 EM (mostly already spent).
- **Biggest risk:** Designing past our knowledge — over-specifying things (auctions, tokens, multi-node) we've explicitly punted. Mitigated by the punt list below.

### Phase 1 — MVP: batch jobs on vetted hosts

- **Scope:** Rust host agent, **Tier B containers only** (nvidia-container-toolkit; no gVisor yet), outbound-only agent connections + relay/WireGuard data plane ([`../platform/networking.md`](../platform/networking.md)), the control plane (API, scheduler, agent-gateway, metering, Postgres, NATS — [`../platform/control-plane.md`](../platform/control-plane.md)), a renter **CLI**, **batch jobs** with checkpoint-resume, **5 curated images** ([`../ml-lifecycle/environments.md`](../ml-lifecycle/environments.md)), **vetted friendly hosts** only (the 50–100 GPU seed fleet from [`marketplace.md`](./marketplace.md) §6), and **manual payouts** (spreadsheet + Stripe transfers).
- **Non-goals:** No serverless inference. No gVisor or microVMs. No self-serve host onboarding (hosts are hand-vetted). No public signup. No reputation engine (hosts are trusted by hand). No auction. No web console beyond bare minimum — CLI is the interface.
- **Exit criteria:** **25 external jobs from 10 users complete successfully**, with **checkpointed-resume demonstrated** (a job survives a mid-run node death and resumes on another node without renter intervention). Metering is accurate to the second and reconciles against manual payout math.
- **Sizing:** ~9–12 EM. The agent + isolation + control-plane triangle is the heavy lift.
- **Biggest risk:** Checkpoint-resume across vanishing consumer nodes is genuinely hard and is our core promise — if it's flaky, we have nothing. Mitigate by making it the exit criterion, not a nice-to-have.

### Phase 2 — Serverless inference

- **Scope:** OpenAI-compatible **gateway**, content-addressed **weight cache**, **3–5 popular models** served via vLLM, **keep-warm replicas**, and **mid-stream failover** ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md)). Per-token and per-second inference billing ([`marketplace.md`](./marketplace.md) §2).
- **Non-goals:** No disaggregated prefill/decode (Dynamo-style). No custom model uploads yet — curated models only. No multi-region. No autoscaling beyond keep-warm N.
- **Exit criteria:** **p95 TTFT target on warm replicas** met (concrete number TBD during build — order of low-hundreds of ms for small models on 4090-class), and **mid-stream failover demonstrated** (a replica dies mid-generation and the stream continues from another replica without a client-visible error).
- **Sizing:** ~6–9 EM.
- **Biggest risk:** Cold-start UX on CGNAT hardware (§ risks below). Warm replicas hide it but cost money; scale-to-zero exposes it. Getting the keep-warm economics and the failover seam right is the whole ballgame.

### Phase 3 — Hardening + marketplace

- **Scope:** **gVisor/nvproxy default** for Tier B ([`../platform/isolation.md`](../platform/isolation.md)), **Tier A Cloud Hypervisor + VFIO microVMs** on dedicated rigs, the **reputation engine** (node reliability feeding scheduling + pricing, spec-fraud re-bench, verified badge — [`marketplace.md`](./marketplace.md) §4), **self-serve payouts** (net-30 automated), and **public launch** with self-serve host onboarding.
- **Non-goals:** No ROCm. No confidential/TEE tier. No multi-node training. No auction pricing.
- **Exit criteria:** gVisor runs the 5 curated images with no workload-breaking regressions; Tier A microVM boots and passes a GPU workload end-to-end; reputation scores demonstrably move scheduling and price bands; a host onboards self-serve in <10 min and receives an automated net-30 payout; public signup open with fraud controls live.
- **Sizing:** ~10–14 EM. gVisor compat + microVM bring-up + reputation + payments automation is a lot of independent hard things.
- **Biggest risk:** gVisor/nvproxy compat gaps silently breaking real ML workloads (§ risks). Mitigate by keeping non-gVisor Tier B as a fallback lane during rollout.

### Phase 4 — Scale-out

- **Scope:** **ROCm allowlist** (AMD fast-follow — curated, allowlisted cards only, not open ROCm support), **eval product polish** (the cross-GPU-model story from [`marketplace.md`](./marketplace.md) §1 as a first-class flow — [`../ml-lifecycle/evaluation.md`](../ml-lifecycle/evaluation.md)), a **GitHub Action** for GPU-CI (the demand-seeding lever), and **multi-region relays** ([`../platform/networking.md`](../platform/networking.md)) to cut latency and egress cost.
- **Non-goals:** No open/arbitrary ROCm hardware. No Windows hosts. No confidential tier. No multi-node training.
- **Exit criteria:** An allowlisted AMD card completes a real fine-tune; the GitHub Action runs an ML repo's test suite on Loom in CI; multi-region relay measurably cuts p95 latency for a distant region; eval product runs one model across ≥3 GPU classes and reports a clean diff.
- **Sizing:** ~8–11 EM.
- **Biggest risk:** ROCm's ML-stack maturity — allowlisting contains the blast radius, but even allowlisted cards can surface framework gaps. Keep the allowlist small and evidence-driven.

### Phase 5 — Confidential tier (Tier C)

- **Scope:** **SEV-SNP / TDX** confidential VMs plus **H100 confidential-computing partners** (small operators with CC-capable hardware), **attestation + key-release** so renters can prove their workload ran on genuine unsnoopable hardware before weights are released ([`../platform/security.md`](../platform/security.md)).
- **Non-goals:** Not consumer hardware (Tier C is small-operator H100-class). No general availability promise — it's a premium lane for sensitive workloads.
- **Exit criteria:** A renter workload runs on an attested confidential node; remote attestation verifies before key/weight release; a sensitive-weight inference endpoint runs with the host provably unable to read weights or activations.
- **Sizing:** ~8–12 EM. Attestation + key-release plumbing is fiddly and security-critical.
- **Biggest risk:** CC-capable H100 supply from *small* operators is thin, and the attestation/key-release chain has to be exactly right — a subtle bug voids the entire privacy promise. Mitigate with external review of the attestation design.

## Critical-path risks (ranked)

**1. Marketplace liquidity chicken-and-egg.** No supply → no renters → no supply. This is the top *business* risk and it gates everything. Mitigation is the entire [`marketplace.md`](./marketplace.md) §6 strategy: seed 50–100 curated GPUs so renters never see an empty market, seed demand with OSS free credits and GPU-CI, keep the take rate low (20%) to pull supply, and run inference + batch on one fleet so utilization stays healthy through demand troughs. We control both sides at Phase 1 by fiat (friendly hosts, invited renters) and only open the loop in Phase 3 once we've proven the flywheel manually.

**2. gVisor/nvproxy compat gaps breaking real workloads.** gVisor's GPU passthrough (nvproxy) doesn't cover every CUDA/driver code path; a real training or inference workload can hit an unimplemented ioctl and fail in ways that are hard to attribute. This is the top *technical* risk for Phase 3. Mitigation: gVisor is introduced *after* Phase 1 proves the product on plain containers, we keep non-gVisor Tier B as an explicit fallback lane during rollout, and we test the 5 curated images against gVisor as an exit gate rather than assuming compatibility. See [`../platform/isolation.md`](../platform/isolation.md).

**3. Cold-start UX killing serverless.** Serving a model from a cold CGNAT-bound consumer node means pulling tens of GB of weights and spinning up an engine while the user waits — potentially minutes. If serverless feels slow, nobody uses it, and we lose the utilization-smoothing that liquidity depends on. Mitigation: content-addressed weight cache with placement hints so weights are pre-warmed on likely nodes, keep-warm replicas as a paid feature that eliminates cold start for latency-sensitive users, and scale-to-zero only offered where the renter has accepted the latency tradeoff. See [`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md).

**4. Abuse/fraud eating margins.** Stolen-card compute, Sybil hosts, spec fraud, and relay-exfil abuse can each turn a thin marketplace margin negative. Mitigation is layered and mostly designed in [`marketplace.md`](./marketplace.md) §4–5: prepay-only + KYC-lite thresholds + new-account spend caps, host probation with held earnings, benchmark fingerprinting + re-bench, net-30 payout buffer sized to the chargeback window, and relay surcharge above a free tier. The no-token design defuses a whole class of incentive-farming attacks for free.

**5. A hyperscaler / competitor price collapse.** AWS/GCP or a well-funded competitor could dump GPU prices to crush a young marketplace, or the consumer-GPU floor (Salad at ~$0.20/hr) could drop further. Mitigation: we deliberately do **not** compete on raw hourly floor — our margin comes from the managed lifecycle, serverless-on-consumer-cards, and cross-GPU eval story ([`marketplace.md`](./marketplace.md) §1) that a raw-box price war can't erode. If someone matches our *lifecycle* value at our price, that's a real threat; a pure price cut on raw compute is one we're structurally insulated from. We keep fixed costs low (no owned hardware) so we can survive a margin-compressed stretch.

## Deliberate punts (and triggers to revisit)

We are choosing *not* to build these at v1. Each has a predefined trigger:

- **Auction pricing.** *Punt:* fixed host-ask with suggested bands. *Revisit when:* a popular GPU class holds >85% sustained utilization (supply-constrained) or <30% (demand-constrained) and manual band-tuning can't keep up. See [`marketplace.md`](./marketplace.md) §2.
- **A marketplace token.** *Punt:* fiat only, no cryptoeconomics. *Revisit when:* fiat host payout is genuinely blocking in a target geography — and even then only as a *payout rail*, never a unit of account or fundraise. See [`marketplace.md`](./marketplace.md) §3.
- **Windows hosts.** *Punt:* Linux + NVIDIA first, ROCm fast-follow. *Revisit when:* the Linux/NVIDIA and ROCm supply is saturated and Windows-only gaming rigs are the marginal supply worth the isolation/agent complexity they'd cost.
- **Multi-node (distributed) training.** *Punt:* single-node jobs only. *Revisit when:* we have demonstrated demand for models that don't fit one node *and* a cohort of hosts with adequate interconnect — unlikely on consumer NAT'd hardware, so this may punt indefinitely.
- **Weight/model sharding across nodes.** *Punt:* a model fits on one node or it doesn't run. *Revisit when:* the same interconnect precondition as multi-node training is met.
- **Dynamo-style prefill/decode disaggregation.** *Punt:* co-located prefill+decode per replica. *Revisit when:* serverless inference volume is high enough that disaggregation's throughput win outweighs its coordination cost and cross-node latency on our network. See [`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md).

## Build-vs-buy ledger

**We write (our differentiation lives here):**

- **Host agent** — the Rust daemon: lifecycle, metering, hardware attestation, idle-time policy, sandbox orchestration ([`../platform/host-agent.md`](../platform/host-agent.md)).
- **Scheduler** — filter/score/lease placement against a cattle fleet with reliability-weighted scoring ([`../platform/control-plane.md`](../platform/control-plane.md)).
- **Gateway glue** — the OpenAI-compatible front, request routing, mid-stream failover, keep-warm orchestration (vLLM does the inference; we do the distribution).
- **Weight cache** — content-addressed weight distribution, placement, eviction, pinning ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md)).
- Marketplace/billing/reputation logic, CLI, and the metering pipeline.

**We adopt (boring, proven, not our value-add):**

- **vLLM** — inference engine.
- **Cloud Hypervisor** — Tier A microVMs.
- **gVisor** — Tier B hardened sandbox.
- **WireGuard** — data-plane encryption / NAT traversal.
- **Postgres** — single source of truth.
- **NATS** — control/event bus + JetStream.
- **Stripe** — payments and payouts.

**We watch (not yet, but tracking):**

- **NVIDIA Dynamo** — prefill/decode disaggregation; revisit at inference scale.
- **Lazy-pull / snapshotter tech** (e.g. SOCI/stargz-style lazy image pull) — could materially cut cold-start and prepare-phase time on CGNAT nodes.
- **SGLang** — alternative/complementary inference engine; watch its throughput and structured-output story against vLLM.

---

*Cross-references: [`marketplace.md`](./marketplace.md) (pricing, reputation, liquidity), [`deployment.md`](./deployment.md) (host/renter UX), [`../platform/security.md`](../platform/security.md) (trust tiers, verification, attestation), [`../platform/isolation.md`](../platform/isolation.md) (sandboxing tiers), [`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) (serverless, weight cache, failover).*
