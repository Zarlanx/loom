# 0014 — Three deployment profiles; self-hostable core, marketplace optional and deferred

**Status:** Accepted — 2026-07-07

## Context

Loom was originally designed as a distributed GPU marketplace — strangers renting idle GPUs to strangers, with billing, identity-stripping, reputation, fraud defense, and a relay fleet as first-class, always-present machinery. The product has been **refocused**: the core is now a **self-hostable GPU compute stack** — anyone deploys the whole backend on their own GPUs and gets the full ML lifecycle (data, training, evaluation, inference/serving) without the marketplace. The marketplace becomes an *optional layer on top*, and its development is deferred.

The trap this decision avoids: the marketplace apparatus exists to make *mutually-untrusting strangers* transact safely. Most of it (billing-for-money, gateway identity-stripping, reputation, benchmark-fingerprint fraud defense, relay fleet) is pure overhead when there are no strangers — a single trusted user or a fleet you own. Baking it into the core would make self-host heavy and violate the lightweight constraint.

## Decision

Ship **three deployment profiles** ([`../architecture/profiles.md`](../architecture/profiles.md)), enabled by the single-binary control plane of [ADR-0013](0013-single-binary-self-host-control-plane.md):

1. **Standalone** — one machine, single trusted user: `loomd` (control plane + scheduler + embedded gateway) and `loom-hostd` side-by-side, SQLite + in-proc queue, no billing/identity-stripping/relay, CLI → localhost.
2. **Private fleet** — machines you own: one `loomd`, the rest `loom-hostd` over LAN/VPN/WireGuard; SQLite default, Postgres optional; trust implicit but sandbox isolation stays on.
3. **Hosted marketplace** — the operator-run service in the existing docs: billing, reputation, identity-stripping, relay fleet, Postgres+NATS. **Development deferred.**

**Core = the self-hostable stack; marketplace = an optional, config/compile-gated layer.** Marketplace-only features are modules, not core. Sandbox isolation is *not* marketplace-gated — it defends against malicious workload code and stays on in every profile ([`../platform/security.md`](../platform/security.md) Direction 1).

## Consequences

- **The existing marketplace docs remain valid but deferred.** [`control-plane.md`](../platform/control-plane.md), [`marketplace.md`](../product/marketplace.md), [`security.md`](../platform/security.md), and the [roadmap](../product/roadmap.md) marketplace phases describe the hosted profile accurately; they are not rewritten, only re-scoped as the deferred third profile.
- **Core APIs must not hard-depend on billing or identity-stripping.** Job submission, scheduling, serving, and the lifecycle must function with those modules absent. Any core path that assumes a balance hold, an anonymized renter, or a reputation score is a bug against the self-host profiles and must be gated.
- The upgrade path is asymmetric by design: standalone → private fleet is configuration; private fleet → marketplace is *enrollment* into a different trust environment ([`../architecture/profiles.md`](../architecture/profiles.md)).

**Open (not decided here):** the **licensing / open-source question** for the self-hostable core is deliberately left unresolved. Making the backend self-hostable raises whether it ships under an open-source, source-available, or proprietary license, and how that interacts with the operator-run marketplace — but that is a separate decision and is **not** settled by this ADR.

## Revisit when

Marketplace work resumes. At that point re-validate that the core APIs still function marketplace-free (the gating held), and re-open the licensing question as its own ADR before any public self-host distribution.
