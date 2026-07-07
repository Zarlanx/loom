# Unit economics: the operator's cost and margin model

Status: design draft · July 2026 · owner: product/finance

This is the operator-side financial model for Loom: what a GPU-hour and a million tokens actually *cost us* to broker, what contribution margin survives after the fees and bandwidth and storage we can't avoid, what our fixed burn looks like per [roadmap](./roadmap.md) phase, and how big the fleet has to be before contribution covers burn. It is a **founder-grade reasoning doc, not a pitch** — every number is either pulled from the competitor table in [marketplace.md](./marketplace.md), a publicly checkable infrastructure price (cited inline), or an explicitly labelled assumption in the table below. Where a decision is unsettled we show both branches' math and flag it.

Cross-references: pricing decisions live in [marketplace.md](./marketplace.md) (fixed ask, 20% take, per-token derivation, keep-warm, relay surcharge, net-30); phases in [roadmap.md](./roadmap.md); keep-warm / cold-start / utilization in [../ml-lifecycle/serving.md](../ml-lifecycle/serving.md); relay egress mechanics in [../platform/networking.md](../platform/networking.md).

The through-line, restated because it drives the whole model: **on raw compute we take a thin 20% clip and must not let fees, relay egress, and storage erode it to zero; the real margin engine is the lifecycle/inference layer.** This doc's job is to prove the raw-compute clip is at least non-negative and to size how much the inference layer has to carry.

---

## 1. Assumptions

Everything downstream is derived from this table. Sourced numbers cite; guesses say GUESS; anything from another doc points to it.

| Symbol | Meaning | Value | Source |
|---|---|---|---|
| `ask_4090` | Host ask, RTX 4090-class, per GPU-hr | **$0.35** (band $0.29–0.50) | [marketplace.md §1](./marketplace.md) competitor table (Vast on-demand band), retrieved 2026-07-07 |
| `ask_5090` | Host ask, RTX 5090-class | **$0.45** (band $0.27–0.99) | [marketplace.md §1](./marketplace.md) |
| `ask_3090` | Host ask, RTX 3090-class | **$0.22** (below-4090 seed supply) | GUESS — no 3090 row in the table; priced under 4090 floor |
| `ask_h100` | Host ask, H100-class | **$1.90** (band $1.03–2.89) | [marketplace.md §1](./marketplace.md) |
| `take` | Platform take rate on host ask | **20%** | [marketplace.md §2](./marketplace.md) — decision |
| `u_pess / u_base / u_good` | Fleet utilization scenarios | **20% / 40% / 60%** | Scenario definition; base/good bracket the [marketplace.md §6](./marketplace.md) 40–60% target |
| `f_pct / f_fix` | Stripe fee | **2.9% + $0.30** per charge | [Stripe pricing](https://stripe.com/pricing) |
| `credit_buy` | Mean renter credit top-up (amortizes the fixed 30¢) | **$50** | GUESS — prepay credit model, [marketplace.md §3](./marketplace.md) |
| `relay_share` | Fraction of data-plane bytes that go over our relay vs. direct WireGuard | **10% / 20% / 30%** (base 20%) | [networking.md §3](../platform/networking.md) — "materially higher than 10%" for CGNAT-heavy consumer fleet |
| `egress_rate` | Our marginal relay egress cost | **$0.001/GB** (€1/TB) | [Hetzner Traffic docs](https://docs.hetzner.com/robot/general/traffic/) — €1/TB EU/US overage |
| `store_rate` | Object-store storage (checkpoints/weights) | **R2 $0.015/GB-mo**, S3 $0.023/GB-mo | [R2 pricing](https://developers.cloudflare.com/r2/pricing/), [S3 pricing](https://aws.amazon.com/s3/pricing/) |
| `store_egress` | Object-store egress | **R2 $0/GB**, S3 $0.09/GB (first 10TB) | [R2 pricing](https://developers.cloudflare.com/r2/pricing/), [S3 pricing](https://aws.amazon.com/s3/pricing/) |
| `support` | Support/abuse handling cost per active host+renter per month | **$3.00** | GUESS — labelled; one part-time ops person / a few hundred accounts |
| `CAC` | Customer acquisition cost | **Ignored at this stage** | Deliberate — supply is seeded (founder rigs + recruited enthusiasts, [marketplace.md §6](./marketplace.md)); demand is OSS free-credits + GPU-CI, not paid. CAC becomes real at public launch (Phase 3) and is out of scope here. |
| `tps_8b / tps_14b / tps_32b` | Aggregate decode throughput per replica | **2,500 / 1,200 / 500 tok/s** | ENGINEERING ESTIMATE; anchored to measured single-4090 vLLM: ~2,700 tok/s for an 8B and ~4,000 for a 7B at high concurrency ([databasemart RTX 4090 vLLM](https://www.databasemart.com/blog/vllm-gpu-benchmark-rtx4090)). Fleet benchmarks (§3) will replace these. |

Two structural notes carried throughout:

- **Take is on the host's ask, not on our cost.** We never pay the host's ask ourselves — the renter does. Our revenue is `take × ask × GPU-hr`. Our *costs* are the fees/bandwidth/storage/support below. Contribution = revenue − those costs.
- **Utilization is a fleet property, not a per-GPU-hour property.** A GPU-hour that gets rented earns; an idle one earns nothing but (for keep-warm) may still cost. The per-GPU-hour P&L in §2 is *for a rented hour*; §5 multiplies by utilization to get monthly contribution per listed GPU.

---

## 2. Per-GPU-hour marketplace P&L (batch/rental)

### The formula

For one **rented** GPU-hour at ask price `A`:

```
renter_price      = A                         # host-ask, take is inclusive per marketplace.md §2
host_payout       = A × (1 − take)            # = 0.80 A
gross_take        = A × take                  # = 0.20 A  — our top line

# --- costs we bear out of gross_take ---
stripe_amortized  = (f_pct × A) + (f_fix × A / credit_buy)
                    # % fee applies to the charge; the 30¢ is amortized over a $50 top-up
relay_cost        = relay_GB_per_hr × relay_share × egress_rate
store_cost        = (ckpt_GB × store_rate/730) + (ckpt_GB × store_egress_on_read)
support_amort     = support_per_acct_month / (active_hrs_per_acct_month)

contribution      = gross_take − stripe_amortized − relay_cost − store_cost − support_amort
```

The bandwidth and storage terms need job-shape inputs. For a **typical batch/training GPU-hour** (fine-tune or eval, not pathological data movement) we assume:

- `relay_GB_per_hr = 2 GB/hr` — steady-state checkpoint deltas + logs over the wire. Weights-in is a one-time prepare-phase cost, not per-hour (and is billed separately at the CPU rate per [marketplace.md §2](./marketplace.md)). ENGINEERING ESTIMATE.
- `ckpt_GB = 5 GB` resident per active job, read back ~once on requeue. Keep-last-N (see §6, and [training.md](../ml-lifecycle/training.md) checkpoint retention) bounds this. ENGINEERING ESTIMATE.
- `active_hrs_per_acct_month = 60` — a modestly active renter. GUESS.

### Worked example — RTX 4090, Tier B, base scenario

`A = ask_4090 = $0.35`.

```
renter_price   = $0.350
host_payout    = 0.80 × 0.350          = $0.2800
gross_take     = 0.20 × 0.350          = $0.0700   ← our revenue for this hour

stripe_amort   = 0.029 × 0.350         = $0.01015
               + 0.30 × (0.350 / 50)   = $0.00210   (30¢ spread over a $50 top-up)
               = $0.01225
relay_cost     = 2 GB × 0.20 × $0.001  = $0.00040
store_cost     = 5 GB × ($0.015/730)   = $0.00010   (R2, per-hour slice of monthly; egress $0)
support_amort  = $3.00 / 60 hr         = $0.05000   ← the dominant cost line

contribution   = 0.0700 − 0.01225 − 0.00040 − 0.00010 − 0.05000
               = $0.00725 per rented GPU-hour
```

**The finding jumps out immediately: on a single 4090 rental hour, contribution is ~$0.007 — barely positive — and it is dominated entirely by the support amortization guess, not by fees or bandwidth.** The raw-compute clip on cheap consumer cards is structurally near-break-even. This is not a surprise; it is exactly why [marketplace.md](./marketplace.md) puts the margin engine in lifecycle/inference. The batch marketplace's job is to fill the fleet and generate liquidity, not to print money.

Two sensitivities worth internalizing now:

- If `support` is really $1/account-month (better tooling, self-serve disputes), support_amort drops to $0.0167 and contribution jumps ~5.6× to ~$0.041/hr.
- On a **higher-ask card the clip scales linearly** because gross_take is 20% of a bigger number while support/fees are near-fixed. This is why H100-class rescues the blended margin.

### Contribution across classes and scenarios

Per **rented** GPU-hour (i.e. before applying utilization), R2 storage, base relay_share 20%, support $3/60hr:

| GPU class | Ask `A` | gross_take (0.20A) | Stripe | relay+store | support | **contribution/rented-hr** |
|---|---|---|---|---|---|---|
| 3090 | $0.22 | $0.0440 | $0.0077 | $0.0005 | $0.0500 | **−$0.0142** |
| 4090 | $0.35 | $0.0700 | $0.0123 | $0.0005 | $0.0500 | **+$0.0072** |
| 5090 | $0.45 | $0.0900 | $0.0158 | $0.0005 | $0.0500 | **+$0.0237** |
| H100 | $1.90 | $0.3800 | $0.0665 | $0.0005 | $0.0500 | **+$0.2630** |

**The 3090 line is negative** — at a $0.22 ask, 20% take doesn't cover a $0.05/hr support load. Read plainly: *the fixed-ish support cost per active account is the real floor, and the cheapest cards can't clear it on rental alone.* Mitigations are (a) drive support cost down, (b) let inference (§3) carry those low-ask nodes, (c) don't over-recruit 3090s as pure rental supply. Now sweep utilization to see monthly contribution per **listed** GPU (contribution/hr × 730 hr × u):

| GPU class | contribution/rented-hr | pess (20%) | base (40%) | good (60%) |
|---|---|---|---|---|
| 3090 | −$0.0142 | −$2.07/mo | −$4.15/mo | −$6.22/mo |
| 4090 | +$0.0072 | +$1.06/mo | +$2.12/mo | +$3.17/mo |
| 5090 | +$0.0237 | +$3.47/mo | +$6.93/mo | +$10.40/mo |
| H100 | +$0.2630 | +$38.4/mo | +$76.8/mo | +$115.2/mo |

The blended contribution of a seed fleet is carried almost entirely by 5090/H100-class supply. A 4090-heavy fleet is roughly break-even on rental and needs inference to be the business.

---

## 3. Serverless inference economics

Inference is where the margin is supposed to live. The per-token cost build-up follows the [marketplace.md §2](./marketplace.md) methodology exactly; we extend it to margins and keep-warm coverage.

### Throughput assumptions (engineering estimates — fleet benchmarks will replace)

> **These are estimates.** They are anchored to published single-card vLLM figures but *will be replaced by fleet benchmarks* fed from the replica table ([serving.md §3](../ml-lifecycle/serving.md), measured tok/s not advertised). Consumer-card, high-concurrency aggregate decode throughput:

| Model class | `tps` (aggregate) | Anchor |
|---|---|---|
| 7–8B (AWQ) | **2,500 tok/s** | ~2,700 tok/s measured for 8B, ~4,000 for 7B on a single 4090 ([databasemart](https://www.databasemart.com/blog/vllm-gpu-benchmark-rtx4090)) — we take a conservative 2,500 |
| 14B (AWQ) | **1,200 tok/s** | scaled ~½ of 8B; the [serving.md](../ml-lifecycle/serving.md) "sweet spot" card |
| 32B / small-MoE (INT4) | **500 tok/s** | tight VRAM, lower concurrency ([serving.md §1](../ml-lifecycle/serving.md)) |

### Per-1M-token cost build-up

```
gpu_sec_cost      = ask / 3600
cost_per_tok      = gpu_sec_cost / (tps × billable_util)
cost_per_1M       = cost_per_tok × 1e6
```

`billable_util = 60%` per [marketplace.md §2](./marketplace.md) (the keep-warm tax: idle gaps aren't billable to renters but the replica still costs us).

**7–8B on a 4090 (`ask=$0.35`):**

```
gpu_sec_cost = 0.35 / 3600            = $9.72e-5 /GPU-sec
cost_per_tok = 9.72e-5 / (2500 × 0.60) = $6.48e-8 /tok
cost_per_1M  =                          = $0.065 per 1M output tokens
```

This reproduces the $0.065/1M cost figure in [marketplace.md §2](./marketplace.md). Now the margin, against that doc's **$0.15–0.30 per 1M** list band:

| Model class | cost/1M (at cost) | list/1M ([marketplace.md](./marketplace.md)) | gross margin |
|---|---|---|---|
| 7–8B | $0.065 | $0.15 – $0.30 | 57% – 78% |
| 14B | $0.135 | ~$0.30 – $0.50 (scales with size) | ~55%–73% |
| 32B/MoE | $0.324 | ~$0.60 – $1.00 | ~46%–68% |

The `cost/1M` basis already contains the host's payout — it is built from the full per-second GPU price — so the margin column is genuinely gross margin, not margin-before-payout. List-vs-cost leaves a **healthy gross margin** — because per-token pricing lets us clip both the take *and* the utilization arbitrage (renters pay for tokens; we pay for reserved GPU-seconds and pocket the difference when concurrency is high). **This is the margin engine.** Note the relay term is negligible for inference: token streams are "a few KB/s" ([networking.md §4](../platform/networking.md)), so relay egress on inference is effectively free even at 100% relay_share.

### Keep-warm and the idle-burn question

Cold starts on CGNAT consumer nodes are minutes-scale and download-dominated ([serving.md §4](../ml-lifecycle/serving.md)), so keep-warm replicas are a paid product ([marketplace.md §2](./marketplace.md): billed at ~50–70% of active per-second). The economic question the model must answer:

**Open question — is the host paid while a replica is warm-idle?** [marketplace.md §2](./marketplace.md) says keep-warm "reserves the GPU but not necessarily saturated" and bills the renter a discounted per-second floor, but does **not** explicitly state whether the host earns during warm-idle seconds. Both branches, with math for a 4090 warm-idle hour, keep-warm priced at 60% of active per-second (active per-sec = ask/3600 = $9.72e-5; warm rate = $5.83e-5/sec = **$0.21/hr** billed to renter):

```
Branch A — host IS paid while warm-idle (host's GPU is reserved, they should earn):
  renter pays        = $0.21/warm-hr
  host payout (80%)  = $0.168/warm-hr
  our gross take     = $0.042/warm-hr
  our costs          ≈ stripe (2.9%) $0.006 + support amort  → contribution ~ +$0.036 minus support
  → keep-warm fee covers host idle burn AND leaves us a clip. Clean.

Branch B — host NOT paid while warm-idle (warm rate is pure platform/relay coverage):
  renter pays        = $0.21/warm-hr
  host payout        = $0            (host eats the electricity of a reserved-but-idle card)
  our gross take     = $0.21/warm-hr
  → far better for us, but hosts will refuse to pin warm capacity for free —
     a reserved idle GPU still burns ~350W and blocks the owner's own use.
     This branch is not incentive-compatible unless warm-idle time is short.
```

**Recommendation to resolve in [marketplace.md](./marketplace.md): Branch A.** A host will not hold a replica resident for zero pay; the keep-warm fee exists precisely to pay the host for reserved-but-idle silicon and cover our relay/warmth cost. The renter's keep-warm floor must therefore be `≥ host_warm_payout / 0.80`. Flagged as OPEN in §8.

**Batch-tier margins.** Batch/offline inference ([serving.md §1](../ml-lifecycle/serving.md), cheapest tier, no keep-warm, preemptible) is priced below shared endpoints but has *zero keep-warm tax* — it soaks idle capacity that would otherwise earn nothing. Contribution is whatever we clip above host cost at high billable utilization; because there's no idle-replica burn, even a thin per-token markup is pure upside on capacity that had no opportunity cost. Batch exists to lift fleet utilization (§ liquidity in [marketplace.md §6](./marketplace.md)), and every batch token is contribution we would not otherwise have.

---

## 4. Operator fixed costs by phase

No owned GPUs — that's the whole model. Fixed costs are control plane, relay fleet, object store, observability, and on-call. Mapped to [roadmap.md](./roadmap.md) phases.

### Control plane

API + scheduler + agent-gateway + metering + Postgres + NATS ([control-plane.md](../platform/control-plane.md)). Concrete Hetzner config:

- 2× **AX52** (Ryzen 7 7700, 64GB) at **€64/mo** each ([Hetzner AX52](https://www.hetzner.com/dedicated-rootserver/ax52/)) = €128/mo — app/gateway/NATS.
- 1× **CCX33** dedicated-vCPU cloud for managed-ish Postgres primary at **€138.49/mo** (post-June-2026 pricing, [Hetzner price adjustment](https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/)) — or self-managed Postgres on a third AX52 (€64) to save cash early.

Phase 1 control plane ≈ **€200/mo (~$220)**. Phases 3–4 grow to HA pairs + read replicas → ~€600/mo (~$660).

### Relay fleet (DERP-style, bandwidth-heavy)

Relays forward WG ciphertext for sessions that can't punch NAT ([networking.md §3](../platform/networking.md)). Cost is **egress-dominated**. Two hosting models:

- **Hetzner metered:** €1/TB egress over the 20TB included allowance ([Hetzner Traffic](https://docs.hetzner.com/robot/general/traffic/)). Predictable and cheap for the relay's traffic profile.
- **OVH "unmetered":** flat-rate, port-speed-limited unlimited egress on US/EU dedicated servers ([OVH bare metal](https://us.ovhcloud.com/bare-metal/prices/)). For a bandwidth-heavy relay this can beat metered — you pay for the pipe, not the bytes — but is fair-use-throttleable.

Relay egress volume estimate at base scenario. Suppose a fleet moving `D` GB/mo of data-plane traffic (checkpoints, interactive, bulk relayed-fallback), of which `relay_share = 20%` traverses our relay:

```
relay_GB/mo   = D × relay_share
relay_egress  = relay_GB/mo × $0.001   (Hetzner) or  fixed server rental (OVH)
```

For a Phase-1 seed fleet of ~75 GPUs at 40% util, assume ~200 GB/GPU/mo of data-plane bytes → D ≈ 15 TB/mo, relayed ≈ 3 TB/mo. On Hetzner that's ~€3/mo of *egress* over the included tier — trivial. The relay cost is the **server rental**, not the bytes, at this scale: 2× relay nodes (AX52-class, €64) for geographic spread ≈ €128/mo. **Relay bytes only become a real line item at scale** (Phase 4 multi-region) — which is exactly why the [marketplace.md](./marketplace.md) relay surcharge exists to make pathological jobs pay their own egress rather than us absorbing it.

### Object store

Checkpoints + weight-cache origin seed. **R2's zero egress is materially better** for our access pattern (nodes pull weights P2P but fall back to origin; checkpoints get read on requeue):

| | Storage /GB-mo | Egress /GB | 20TB stored + 30TB egress/mo |
|---|---|---|---|
| **R2** | $0.015 | **$0** | $300 + $0 = **$300/mo** |
| **S3** | $0.023 | $0.09 (first 10TB) | $460 + ~$2,700 = **~$3,160/mo** |

The egress line is a **10×+ swing**. Because the weight-cache origin is a fallback seed that many nodes pull from ([serving.md §4](../ml-lifecycle/serving.md)), egress volume is real and unpredictable — **R2 is the default; S3 would put a four-figure egress bill on a marketplace clipping cents per GPU-hour.** This is a decision, not a preference.

### Observability + on-call reality

- Observability: self-hosted Grafana/Loki/Prometheus on an existing AX52 (marginal), or a managed tier ~$100–300/mo early. Budget **$150/mo**.
- On-call: at a 2–3 engineer team ([roadmap.md](./roadmap.md) sizing) on-call is *the founders*, cost already in headcount — but it's a real human load: consumer nodes die constantly, so alerting must distinguish "a node vanished, requeue fired, nothing to do" from "the gateway is down." Modeled as $0 incremental cash, non-zero attention.

### Monthly burn table (cash, ex-headcount)

| Line | Phase 1 (MVP) | Phase 2 (inference) | Phase 3 (marketplace/launch) | Phase 4 (scale-out) |
|---|---|---|---|---|
| Control plane | $220 | $350 | $550 | $660 |
| Relay fleet | $140 (2 nodes) | $210 (3) | $420 (6, multi-POP) | $840 (12, multi-region) |
| Object store (R2) | $150 | $350 | $600 | $1,200 |
| Observability | $150 | $200 | $300 | $400 |
| Stripe fixed/platform | ~$0 (manual payouts) | $50 | $100 | $150 |
| **Total cash burn/mo** | **~$660** | **~$1,160** | **~$1,970** | **~$3,250** |

Headcount (2–3 engineers) dwarfs all of it — but *that's the point*: the infra burn is deliberately kept in the low four figures so a margin-compressed stretch (a price war, §6) doesn't sink us on fixed cost. The [roadmap.md](./roadmap.md) "keep fixed costs low (no owned hardware)" thesis holds numerically.

---

## 5. Break-even analysis

Break-even on **infra cash burn** (ex-headcount): how many listed GPUs, at base utilization, does contribution have to cover?

```
GPUs_needed = fixed_burn_monthly / (contribution_per_rented_hr × 730 × u)
```

At base `u = 40%`, using the §2 monthly-per-listed-GPU contributions:

**Batch-rental only (no inference), Phase 1 burn $660/mo:**

| Fleet mix | contribution/GPU/mo (base) | GPUs to cover $660 |
|---|---|---|
| all-4090 | $2.12 | **312 GPUs** |
| all-5090 | $6.93 | **95 GPUs** |
| all-H100 | $76.8 | **9 GPUs** |
| blended seed (mostly 4090/5090, few H100) | ~$8/GPU | **~83 GPUs** |

The seed fleet is 50–100 GPUs ([marketplace.md §6](./marketplace.md)). So **a blended seed fleet at 40% util roughly covers Phase-1 infra burn on batch rental alone — but only if it isn't 4090/3090-heavy.** An all-4090 fleet would need 312 GPUs to cover burn on rental alone, which is why inference is not optional.

**With inference carrying its share.** A single 8B replica at 40% billable serving ~2,500 tok/s aggregate, at a $0.10/1M *margin* (list $0.15 − cost $0.065, conservative), earns:

```
tokens/mo   = 2500 tok/s × 0.40 × 730hr × 3600s = 2.63e9 tok/mo
margin/mo   = 2.63e9 × ($0.10/1e6) = $263/mo per warm replica
```

**One warm 8B inference replica contributes ~$263/mo — more than 100× a 4090 rental hour's monthly contribution.** A handful of warm shared-endpoint replicas covers the entire Phase-1/2 infra burn. This is the single most important number in the doc: *inference contribution per GPU dominates rental contribution per GPU by two orders of magnitude*, which is the whole strategic reason both products share one fleet.

### Sensitivity table

Batch-rental contribution/rented-hr for a **4090**, varying the levers (base cell bold):

| | take 15% | take 20% | take 25% |
|---|---|---|---|
| **util 20% (pess)** | −$0.0103 | +$0.0072 | +$0.0247 |
| **util 40% (base)** | −$0.0103 | **+$0.0072** | +$0.0247 |
| **util 60% (good)** | −$0.0103 | +$0.0072 | +$0.0247 |

(Contribution/*rented-hr* is util-independent; util scales the *monthly* total in §2. Take-rate moves it linearly: at 15% the 4090 goes negative, at 25% it's healthy — a live argument for the [marketplace.md §2](./marketplace.md) "nudge to 22–25% on premium capacity" revisit trigger.)

Relay-share sensitivity (4090, base): relay cost swings from $0.0002/hr (10%) to $0.0006/hr (30%) — **immaterial to batch contribution.** Relay share matters for *fixed* relay-fleet sizing and for pathological jobs, not for the marginal rental hour. The dangerous relay scenario is §6.

---

## 6. Dangerous scenarios, quantified

**(a) Price war — Salad-level $0.10–0.20/hr floors.** If the 4090 ask is dragged to **$0.15/hr**:

```
gross_take   = 0.20 × 0.15 = $0.030
minus stripe $0.0053 + relay/store $0.0005 + support $0.05
contribution = 0.030 − 0.0558 = −$0.026 /rented-hr   ← negative
```

At Salad-floor pricing the batch clip **goes underwater** — the 20% take on $0.15 can't cover per-account support. **This confirms the [roadmap.md](./roadmap.md) risk-5 stance: we do not follow the floor.** The defense is not matching price; it's that our margin lives in inference (§3, unaffected by raw-hour price wars — a $0.15 GPU-hr makes inference *cheaper to source*, widening token margin) and lifecycle value. A pure raw-compute price war *improves* our inference cost base while it destroys the batch clip — so we lean into inference and let batch ride at break-even.

**(b) Relay-share blowout — CGNAT-heavy fleet.** [networking.md §3](../platform/networking.md) warns our population skews worse than Tailscale's 90%-direct. Suppose relay_share hits **60%** and a subset of jobs are bandwidth-heavy (interactive port-forwards, un-throttled bulk) at 20 GB/hr relayed:

```
relay_cost = 20 GB × 0.60 × $0.001 = $0.012/hr
```

Still only 1.2¢/hr on Hetzner egress — but across a fleet doing this continuously it's the *fixed relay-fleet* line that balloons (more relay nodes, more bytes over the included tier). The real risk is a few abusive jobs, which is exactly what the **relay surcharge above a free tier** ([marketplace.md §2](./marketplace.md)) is designed to bill back. The model says: instrument relay % per job (§7) and make the surcharge free-tier sizing (open in [marketplace.md §7](./marketplace.md)) tight enough that exfil-abuse pays.

**(c) Checkpoint-storage abuse — unbounded checkpoints.** Without retention, a training job writing a 5GB checkpoint every 10 min for a week = ~5TB. At R2 $0.015/GB = **$75/mo per abusive job**, dwarfing the job's entire contribution. This is the concrete financial motivation for the **keep-last-N prune policy** already in [training.md](../ml-lifecycle/training.md) ("we keep-last-N and prune older checkpoints"): keep-last-3 caps a job at ~15GB = $0.23/mo. The policy isn't just tidiness — it's the difference between storage being a rounding error and a per-job loss leader.

**(d) Refund/fraud rate spikes.** Prepay + net-30 host payout ([marketplace.md §3](./marketplace.md)) means a renter chargeback lands *before* host cash leaves. But chargebacks still cost: Stripe dispute fee (~$15 each) + the compute we already delivered. If chargeback rate hits **2% of charged volume**, on a $50 mean top-up that's $0.30/account in dispute fees alone plus the take we refund. At our thin batch clip, a 2% fraud rate can erase batch contribution entirely — which is why KYC-lite thresholds and new-account spend caps ([marketplace.md §5](./marketplace.md)) are load-bearing, not optional. The net-30 buffer converts most stolen-card compute into a caught reversal rather than a paid-out loss.

---

## 7. What we must instrument from day one

The whole model above runs on GUESS-labelled inputs. For it to have real inputs by Phase 2, these must be metered from the first job (metering pipeline in [control-plane.md](../platform/control-plane.md), signed usage records already in [networking.md §2](../platform/networking.md)):

1. **Fleet utilization** per GPU class — the single biggest lever in §2/§5. Advertised-hours vs. billed-hours, per node.
2. **Direct-vs-relay ratio** per session (`relay_share`) — [networking.md §9](../platform/networking.md) open-question #1; drives relay-fleet sizing and surcharge policy. Instrument the punch-success rate from the first tunnel.
3. **Storage per job over time** — checkpoint bytes resident and their age, to validate keep-last-N and catch abuse (§6c) before it bills.
4. **Support tickets / disputes per 100 jobs** — turns the $3/account GUESS into a measured number; it's the dominant cost line in §2, so getting it real is worth more than any other single input.
5. **Measured inference throughput per (model, GPU class)** — replaces the §3 estimates; already surfaced in the replica table ([serving.md §3](../ml-lifecycle/serving.md)), just needs logging into the cost model.
6. **Stripe effective fee %** and mean top-up size — to replace the $50 amortization assumption.

Design principle: every GUESS in §1 should have a metric that replaces it by Phase 2. If a number in this doc can't be measured, that's a bug in the instrumentation plan.

---

## 8. Open questions

- **Keep-warm host payout (§3).** Does the host earn during warm-idle seconds? Recommendation Branch A (yes); needs a decision in [marketplace.md §2/§7](./marketplace.md), and it sets the minimum keep-warm price floor.
- **Support cost per account.** The $3/account-month GUESS dominates batch contribution and flips the 3090 (and price-war 4090) negative. What is it really? Blocks any confident statement about batch-tier profitability.
- **3090-class supply.** At a sub-$0.25 ask, 3090 rental is contribution-negative under our support assumption. Do we recruit them anyway (for inference-only supply), price them higher, or decline them? Interacts with the liquidity seed-fleet mix ([marketplace.md §6](./marketplace.md)).
- **Blended fleet mix target.** §5 shows break-even is entirely mix-dependent (9 H100s vs 312 4090s for the same burn). What GPU-class mix are we actually recruiting, and does the seed fleet clear infra burn on batch alone or must inference carry it from Phase 1?
- **Relay free-tier sizing** (mirrors [marketplace.md §7](./marketplace.md)) — the per-job GB allowance that keeps honest jobs out of the surcharge while catching exfil. The model can't price relay contribution until this is set.
- **Batch-tier floor price.** How far below shared-endpoint per-token pricing can batch go before it's not worth the metering overhead, given it's pure idle-fill upside?
- **When does take-rate move?** §5 sensitivity shows 15% is underwater on 4090s and 25% is healthy. The [marketplace.md §2](./marketplace.md) "nudge to 22–25% on premium capacity" trigger needs a concrete utilization/supply threshold tied to these numbers.

---

*This model is deliberately conservative and deliberately incomplete: it exists to be falsified by the §7 instrumentation. Numbers not cited or in §1's assumptions table are derived arithmetic from those; every infrastructure price is dated 2026-07-07 and will drift. Re-run against real fleet inputs at the Phase 2 gate.*
