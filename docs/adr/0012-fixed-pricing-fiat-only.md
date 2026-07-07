# 0012 — Fixed ask pricing + 20% take + fiat only

**Status:** Accepted — 2026-07-07

## Context

Loom bootstraps a two-sided market against an entrenched, cheap consumer-GPU field (Vast.ai, RunPod Community, SaladCloud). Three economic-model choices are coupled: the price-discovery mechanism, the platform take rate, and the money rail. The core positioning is that Loom sells a **managed ML lifecycle** on top of marketplace-priced silicon — margin comes from checkpoint/failover/eval and serverless-on-consumer-cards, not from winning a raw-hourly price war ([`../product/marketplace.md`](../product/marketplace.md) §1). The overnight-training use case we court needs **predictable, forecastable cost**, and the top business risk is liquidity, which favors pulling supply with a low take and avoiding surfaces that don't solve the real problem (trust).

## Decision

At v1 ([`../product/marketplace.md`](../product/marketplace.md) §2–§3):

- **Fixed ask pricing, no auction.** Hosts set a fixed per-GPU-hour ask; the platform shows suggested bands from live supply/demand (the Vast.ai model). **No auction at v1** — auctions add bid-strategy friction and price volatility that make cost forecasting impossible for overnight training, and complicate billing.
- **20% take rate** on GPU-time, on the host's ask — deliberately undercutting Vast's ~25–30% because supply is the scarce side and the real margin engine is the lifecycle/inference layer. Prepare/download phases bill at a cheap flat CPU rate, not GPU rate; relay bandwidth above a free tier is surcharged; keep-warm is a paid feature (ADR-0009).
- **Fiat only — no token.** A marketplace token adds a speculation/tokenomics surface without solving trust (a token doesn't make a benchmark honest or a card real), invites volatility and regulatory ambiguity, and a no-token design defuses whole classes of incentive-farming and self-dealing attacks for free. Renters prepay credits via Stripe; hosts are paid net-30 above a threshold (the chargeback buffer). Crypto is held at arm's length.

## Consequences

- Predictable, forecastable pricing for the overnight-training happy path; simpler billing; a take rate that gives hosts a reason to prefer Loom.
- The no-token design makes self-dealing a net loss to the attacker and removes a purely-financial participant class ([`../product/marketplace.md`](../product/marketplace.md) §5).

**What we give up:**

- Fixed asks capture only **~80% of the price-discovery benefit** — no spot/preemptible lane to clear supply-constrained or demand-constrained imbalances automatically; band-tuning is manual ([`../product/marketplace.md`](../product/marketplace.md) §2).
- 20% is thinner margin than competitors take, betting the difference is recovered in the lifecycle layer — unproven against live supply elasticity ([`../product/marketplace.md`](../product/marketplace.md) §7).
- Fiat-only and net-30 payouts forgo the funnel-widening of crypto intake and instant payouts, and exclude non-card geographies at v1; the net-30 delay is priced for the chargeback window, not host convenience.

## Revisit when

A popular GPU class holds >85% sustained utilization (supply-constrained) or <30% (demand-constrained) and manual band-tuning can't keep up — then a spot/preemptible **auction lane** is worth building. Nudge the take toward 22–25% on premium/Tier-A capacity once supply is healthy. Revisit a token **strictly as a payout rail** where fiat host payout is genuinely blocking in a geography — never as unit of account or fundraise ([`../product/marketplace.md`](../product/marketplace.md) §2–§3; roadmap punt list).
