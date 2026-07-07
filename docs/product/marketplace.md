# Marketplace: pricing, billing, reputation, verification

This document defines the economic layer of Loom: how compute is priced, how money moves, how we keep hosts honest and renters safe, and how we bootstrap a two-sided market from zero. It assumes the technical seams described in [`../platform/control-plane.md`](../platform/control-plane.md) (metering, billing pipeline, reliability scoring), [`../platform/security.md`](../platform/security.md) (trust tiers, work verification), and [`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) (serverless inference, keep-warm). Renter- and host-facing UX lives in [`deployment.md`](./deployment.md).

The guiding principle: **Loom sells a managed ML lifecycle on top of marketplace-priced silicon.** Competitors rent you a raw box; we rent you a job that checkpoints, an inference endpoint that fails over, and an eval harness that runs the same model across five GPU classes. That difference is where our margin has to come from, because on raw hourly price we are competing with hosts who will undercut anyone.

## 1. Market positioning

The consumer-GPU rental market in mid-2026 is crowded and cheap. Representative rates for 4090/5090-class cards and H100s (retrieved 2026-07-07, spot/on-demand vary widely by host and reliability tier):

| Provider | RTX 4090 | RTX 5090 | H100 | Model |
|---|---|---|---|---|
| Vast.ai | ~$0.29–0.50/hr | ~$0.27–0.99/hr | ~$1.60–2.50/hr | Open marketplace, host-set |
| RunPod Community | ~$0.34–0.69/hr | — | ~$2.89/hr (Secure) | Vetted community pool |
| SaladCloud | ~$0.20/hr (batch) | ~$0.27–0.29/hr (batch) | n/a (consumer-only) | Interruptible batch |
| io.net / market median | ~$0.17–0.55/hr | — | ~$1.03 (spot)–2.69/hr | Aggregated/DePIN |

Sources: [Vast.ai pricing](https://vast.ai/pricing) and [RTX 4090 page](https://vast.ai/pricing/gpu/RTX-4090); [RunPod pricing](https://www.runpod.io/pricing); [SaladCloud pricing](https://salad.com/pricing) and [RTX 5090 launch post](https://blog.salad.com/rtx5090/); cross-market indices [getdeploying RTX 4090](https://getdeploying.com/gpus/nvidia-rtx-4090) / [RTX 5090](https://getdeploying.com/gpus/nvidia-rtx-5090) and [Spheron H100 comparison](https://www.spheron.network/blog/gpu-cloud-pricing-comparison-2026/). All retrieved 2026-07-07; treat as ranges, not quotes — live rates move daily.

**Where Loom does not compete: the floor.** SaladCloud's ~$0.20/hr batch 4090 is a race we will not win and should not enter. Those prices assume fully interruptible, best-effort placement with no lifecycle guarantees. We will land **at or slightly above the Vast.ai on-demand band** for equivalent reliability tiers, and justify the delta with what sits on top.

**Where Loom differs:**

1. **Managed ML lifecycle, not raw boxes.** Vast/RunPod/Salad hand you an SSH prompt or a container and wish you luck. Loom ships checkpoint-resume, automatic requeue on node death, curated images, and an eval product that treats a vanishing consumer GPU as expected, not exceptional. A renter fine-tuning overnight on a gaming rig that reboots at 2 a.m. is our happy path.
2. **Serverless inference on consumer cards.** An OpenAI-compatible gateway (see [`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md)) fronting 4090/5090-class hardware, with keep-warm replicas and mid-stream failover, is a category most consumer-GPU marketplaces do not offer — they sell pods, not tokens.
3. **The test-across-GPU-models story.** Because our fleet is heterogeneous by nature, "run this eval on a 3090, a 4090, and a 5090 and diff the numbers" is a first-class product, not a chore. Homogeneous datacenter providers structurally can't sell this.

## 2. Pricing model

### Fixed ask, not an auction (at v1)

Hosts set a fixed ask price per GPU-hour. The platform shows **suggested bands** derived from live supply/demand per GPU class (e.g. "5090s are renting at $0.30–0.45/hr this week; you're listed at $0.60 and haven't been booked"). This is the Vast.ai model — host-set prices, platform-computed guidance — and it works.

We explicitly **do not run an auction at v1.** Auctions add UX friction (renters must reason about bid strategy), introduce price volatility that makes cost forecasting impossible for the exact overnight-training use case we're courting, and complicate the billing pipeline. The suggested-band mechanism captures ~80% of the price-discovery benefit with none of the volatility. **Revisit trigger:** if utilization on a popular GPU class exceeds ~85% sustained (supply-constrained) or drops below ~30% (demand-constrained) and manual band-tuning can't keep up, a spot/preemptible auction lane becomes worth building.

### Take rate

Vast.ai's effective commission runs **~25–30%** ([Vast pricing docs](https://docs.vast.ai/documentation/instances/pricing)); RunPod's Hub adds up to a 7% take on top of infra spend ([RunPod revenue-sharing docs](https://docs.runpod.io/hub/revenue-sharing)). The consumer-GPU norm is a hosts-keep-the-majority split with the platform taking a quarter to a third.

**Loom take rate: 20% on GPU-time at launch, on the host's ask.** We deliberately undercut Vast's ~25–30% because (a) we're the new entrant and supply is our scarce side — hosts should feel they earn more here — and (b) our real margin engine is the lifecycle/inference layer, not the raw-compute clip. Prepare/download phases (see below) and inference carry their own economics. **Revisit trigger:** once supply is healthy (see §6 liquidity), nudge toward 22–25% on premium/Tier-A capacity where we add more value.

### Prepare-phase CPU rate

Jobs have a prepare phase (pull image, download weights/data, warm the cache) and a GPU phase. Billing the prepare phase at full GPU rate punishes exactly the large-model workloads we want. Following the "prepare/download billed cheaper than GPU time" decision: **prepare and download phases bill at a flat CPU-hour rate (target ~$0.02–0.05/CPU-hr equivalent), not the GPU rate**, metered separately by the host agent. This is a differentiator — a 40GB weight download that takes 20 minutes shouldn't cost 20 minutes of 5090 time.

### Inference per-token pricing (derivation)

Per-token inference price derives from the underlying per-second GPU cost, an assumed throughput, and a target utilization. The arithmetic, done once for methodology (illustrative, not a quote):

- Underlying cost: a 4090-class replica at **$0.35/GPU-hr** = **$0.0000972/GPU-sec**.
- Assumed sustained decode throughput for a 7–8B model on that card: **~2,500 output tok/sec** aggregate across concurrent requests.
- Assumed billable utilization: **60%** (idle gaps between requests are not billable to renters but the replica still costs us; this is the keep-warm tax).
- Cost per output token ≈ $0.0000972 / (2,500 × 0.60) ≈ **$6.5e-8/tok** ≈ **$0.065 per 1M output tokens** at cost.
- Apply the platform take + margin + relay overhead → list price in the **~$0.15–0.30 per 1M output tokens** band for small models, scaling up with model size / GPU class.

The point is the **methodology, not false precision**: list price = (per-sec host cost ÷ (throughput × utilization)) × (1 + margin). We publish flat per-token prices per model tier and eat the variance, because token pricing is what the OpenAI-compatible market expects. Renters who prefer predictability can instead buy a replica **per-second** (the other supported inference billing mode) and manage their own utilization.

### Keep-warm pricing

Cold starts on consumer hardware behind CGNAT are brutal (weight pull + engine spin-up). Keep-warm replicas — replicas held resident so TTFT stays low — are a **paid product feature**, billed at a **discounted per-second replica rate** (the GPU is reserved but not necessarily saturated, ~50–70% of active per-sec). A renter serving a latency-sensitive endpoint pays to pin N warm replicas; a renter doing bursty batch inference lets them scale to zero and accepts cold-start latency. This directly monetizes the relay/warmth cost instead of socializing it.

### Relay bandwidth: surcharged, not absorbed

Nodes live behind CGNAT; relaying their data plane costs *us* real egress (see [`../platform/networking.md`](../platform/networking.md)). **Decision: relay bandwidth above a generous per-job free tier is surcharged, not absorbed**, at a transparent per-GB rate roughly tracking cloud egress cost. Justification: absorbing it invites abuse (someone using Loom as a cheap data-exfil relay) and misprices the genuinely bandwidth-heavy jobs. The free tier is sized so normal training/inference (weights in, checkpoints out, tokens over the wire) rarely hits it; only pathological data movement pays. Direct WireGuard paths that succeed at NAT traversal bypass the relay and the surcharge entirely — an incentive aligned with our cost.

## 3. Payments

**Renters prepay credits.** Card via Stripe; balance is a hold-and-debit ledger in Postgres (see billing pipeline in [`../platform/control-plane.md`](../platform/control-plane.md)). Prepay eliminates most collection risk and caps exposure per account.

**Crypto is an open question, held at arm's length.** Accepting crypto for credit purchase widens the funnel (privacy-conscious renters, non-card geographies) but reintroduces exactly the fraud surface prepay was meant to close: irreversible funding of stolen-compute schemes, mixing, and no chargeback recourse *for us* against a bad actor while giving *them* no chargeback risk. Honest tradeoff: crypto-in helps supply-side payouts more than demand-side intake. **Not at v1.** Revisit for host *payouts* (see below) before renter *intake*.

**Hosts are paid out net-30, monthly, above a threshold.** Accrued host earnings settle on a net-30 monthly cycle once the balance clears a minimum (e.g. $25). The delay is a **fraud and chargeback buffer**, and it is deliberate: a renter's card can be charged back weeks after the compute was delivered, and a host who was already paid instantly is money we can't claw back. Net-30 lets renter chargebacks and stolen-card reversals surface before host cash leaves the building. It matches the risk window, not host convenience — and we say so plainly on the host page. As trust in a host accrues (see §4), the threshold and possibly the delay can tighten for proven hosts.

**Currency is fiat. There is no token, and that's a decision, not an omission.** io.net and Akash demonstrate that a marketplace token adds a speculation and tokenomics surface **without solving the actual hard problem, which is trust** — a token doesn't make a host's benchmark honest or a renter's card real. It invites price volatility, regulatory ambiguity, and a class of purely-financial participants who don't want compute. Loom prices in dollars, pays in dollars, and puts all its engineering into verification instead of cryptoeconomics. We may revisit a token strictly as a *payout rail* in geographies where fiat payout is hard — never as the unit of account or a fundraising mechanism.

## 4. Reputation & verification

Reputation is per-node and per-account, feeds **both scheduling and pricing**, and is the mechanism that lets us charge above the floor.

**Host reputation inputs** (feeding the node reliability score in [`../platform/control-plane.md`](../platform/control-plane.md)):

- Uptime / availability against advertised windows.
- Job completion rate (started vs. cleanly finished).
- Checkpoint-loss events (node vanished without a usable checkpoint — heavily penalized; this is our core reliability promise).
- Benchmark drift (re-bench results vs. enrollment fingerprint — see spec-fraud below).
- Work-verification pass rate (canary and spot-check outcomes, §5 / [`../platform/security.md`](../platform/security.md)).

Scores decay and recover, so one bad week doesn't brand a host forever, but sustained bad behavior sinks both their placement priority and the price band they can command.

**New-host cold start (probation).** A freshly enrolled host is untrusted. For the first N hours (target ~48h of active time): it receives **only canary jobs with known outputs and low-stakes real jobs**, never sensitive or long-running work; **earnings are held** (not just delayed by net-30 — actually escrowed pending probation clearance); and it must pass a threshold of canary verifications. This makes Sybil/stolen-spec hosts expensive to run and cheap for us to catch before a real renter is exposed.

**Spec-fraud defenses.** At enrollment the agent captures a **benchmark fingerprint** (throughput, memory bandwidth, VRAM, clocks) for a claimed GPU. We **periodically re-bench** during idle windows and compare against fingerprint and against the population for that GPU class. A host claiming a 5090 whose fingerprint reads 4070 is flagged, down-tiered, and can be delisted. Hosts that pass carry a **renter-visible "verified" badge**; renters can filter to verified-only, which is itself a pricing lever (verified capacity commands more).

**Renter-side reputation, too.** Renters accrue reputation from behavior: abusive workloads (crypto-mining disguised as ML, attempts to escape the sandbox, jobs that trip host-side resource abuse detectors), chargeback history, and payment health. Low-reputation renters face tighter spend limits and are barred from Tier-A/dedicated hosts. This protects hosts, who are our scarce side.

**Disputes.** A renter claiming a job failed due to host fault, or a host claiming a renter workload was abusive, opens a dispute. Signed usage records, canary/spot-check results, and node telemetry are the evidence base — because metering and verification are cryptographically attributable (see [`../platform/control-plane.md`](../platform/control-plane.md)), most disputes resolve mechanically. Genuinely ambiguous cases are adjudicated manually at v1 (low volume expected) with a bias toward refunding the renter in credits and neutrally noting rather than penalizing the host absent evidence of fault. As volume grows this becomes a policy engine, not a person.

## 5. Fraud & abuse economics

**Stolen-card compute.** The classic marketplace attack: fund an account with a stolen card, burn compute (or resell it), vanish before the chargeback. Defenses: **prepay-only** (no post-paid exposure), **low spend limits for new accounts** with **KYC-lite thresholds** (small spends need only email/card; crossing a cumulative threshold or requesting Tier-A/H100-class capacity triggers stronger verification), velocity limits on new-account spend, and the net-30 host payout buffer that means stolen-card compute rarely converts to host cash before reversal. The economics: we cap the loss per fresh account below the cost of acquiring a fresh stolen card + identity to clear KYC-lite.

**Self-dealing.** A host renting their own GPUs to farm incentives. **Without a token, this is mostly harmless** — there are no token emissions or referral bounties to farm, so self-renting just moves the host's own dollars in a circle minus our take rate, which is a net *loss* to them. We watch for it anyway (it can be used to fake utilization/reputation) via payment-graph and network correlation, but we don't over-engineer against an attack that a no-token design already defuses. This is a concrete example of the token decision paying for itself.

**Sybil hosts.** Many fake hosts to game reputation, dominate placement, or launder a stolen-spec rig. Countered by: benchmark fingerprinting (a Sybil still needs real distinct silicon to pass re-bench), probation with held earnings (Sybils don't get paid until they've done real verified work), hardware/network correlation (same MAC/IP/attestation across "different" hosts), and the net-30 buffer. Sybil attacks that survive all of these have, by construction, contributed real verified compute — at which point they're just hosts.

## 6. Marketplace liquidity strategy

The chicken-and-egg problem (no hosts → no renters → no hosts) is the top business risk (see [`roadmap.md`](./roadmap.md)). Plan:

**Seed supply (cold start).** Start with **founder-controlled rigs plus recruited enthusiasts** — target **50–100 GPUs** across 3090/4090/5090-class, sourced from the ML/homelab/ex-mining community (the same population Vast and Salad recruited from). These are vetted, friendly hosts we can debug the agent against, and they give us a curated supply floor so the first renters never see an empty marketplace.

**Seed demand.** Two levers: **free credits to OSS ML projects** (fine-tuning runs, eval sweeps — high-visibility users who generate the checkpoint-resume and cross-GPU-eval testimonials we need), and the **GPU-CI angle** — offering cheap, ephemeral GPU runners for ML repos' test suites via a GitHub Action (see [`roadmap.md`](./roadmap.md) Phase 4). CI demand is bursty, latency-tolerant, and small-per-job — perfect for filling gaps in a young fleet.

**Utilization targets.** Aim for **40–60% fleet utilization** in early phases — high enough that hosts earn meaningfully and stay, low enough that renters rarely hit "no capacity." Below ~30%, hosts churn; above ~85%, renters get turned away and we're leaving supply on the table.

**Why inference demand smooths batch troughs.** Batch/training demand is spiky and human-scheduled (people launch runs during work hours, evals in bursts). Serverless inference demand is comparatively **steady and diurnal**, and it can **scale to zero and back** on the same silicon. Running both products on one fleet lets inference soak up capacity during batch troughs and yield it during batch peaks, flattening utilization and improving host earnings without adding hardware. This is a core reason the two products live on one marketplace rather than two.

## 7. Open questions

- **Auction lane timing.** Exact utilization triggers for introducing a spot/preemptible auction — is 85%/30% right, or should it be per-GPU-class?
- **Take-rate elasticity.** Does 20% actually pull enough supply vs. Vast's 25–30%, or do hosts follow price bands more than take rate? Needs live data.
- **Crypto payout rail.** For which geographies is fiat host payout genuinely blocking, and does a stablecoin payout option there reintroduce unacceptable AML surface?
- **KYC-lite threshold calibration.** Where exactly does the stolen-card break-even sit for our cost structure? Set too low it kills conversion; too high it invites fraud.
- **Keep-warm pricing model.** Flat discounted per-second vs. a subscription "reserved warm capacity" SKU — which do inference customers actually want?
- **Relay free-tier sizing.** What per-job GB allowance keeps 99% of honest jobs out of the surcharge while still catching exfil abuse?
- **Dispute automation threshold.** At what volume does manual dispute adjudication break, and what's the minimum viable policy engine?
