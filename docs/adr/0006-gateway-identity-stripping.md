# 0006 — Gateway identity-stripping as the primary renter-privacy mechanism

**Status:** Accepted — 2026-07-07

## Context

On a consumer GPU in a stranger's machine, in-use encryption of renter data against the machine's owner is **physically unavailable**: the owner sits at the bottom of the privilege stack (ring 0, hypervisor, firmware, physical DRAM/PCIe access), and software above an adversary cannot hide a secret from it. The dead-end alternatives are explicitly rejected in [`../platform/security.md`](../platform/security.md) §0: FHE/MPC are four-to-six orders of magnitude too slow for dense ML; white-box crypto and weight obfuscation are broken-by-design against an adversary who can single-step and dump memory. Model sharding across hosts is deferred — it multiplies the failure surface over residential links (contradicting whole-model-per-node, ADR-0009), leaks structure across the shard boundary, and doesn't stop collusion ([`../platform/security.md`](../platform/security.md) §10). So on the consumer tier we need a privacy mechanism that holds *even against a fully malicious host*, without pretending to encrypt content.

## Decision

Make **gateway identity-stripping** the primary renter-from-host protection on the consumer tier, for the serverless inference product ([`../platform/security.md`](../platform/security.md) §3.2, §4). The renter's identity — API key, account, source IP, billing linkage — terminates at the operator-run gateway. A serving node receives only an anonymized prompt, a one-time unlinkable request ID, and a trust-tier label ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §3). Requests from one account are **scattered** across the warm-node pool via anti-affinity so no host accumulates one buyer's corpus. The gateway becomes the single most sensitive store (the linkage map): encrypted at rest, short retention, access-audited, break-glass only, prompt content not logged by default. Software in-use encryption is rejected as impossible; real content confidentiality is deferred to Tier C (ADR-0008).

## Consequences

- The guarantee is **structural** — it holds against an active malicious host. A dumped prompt is an orphan: no key, no account, no way to correlate or resell "Customer X's traffic." The host's dump-VRAM capability is intact; its *value* is destroyed.

**What we give up:**

- Identity-stripping hides *who*, not *what*. **The serving host still sees the prompt text verbatim**; user-controlled PII inside the payload leaks regardless. Standard-tier serving is the wrong tool for content-sensitive workloads ([`../platform/security.md`](../platform/security.md) §4 limits, R2).
- Two residual re-linkage channels survive and are bounded, not eliminated: content self-identification, and traffic/timing correlation. Scattering raises the cost of the latter but is not information-theoretic (R12).
- Request scattering **trades against KV-cache prefix reuse** — the anti-affinity policy caps how much warm-cache continuity any one host gets, a serving tuning knob with a cost/latency price ([`../ml-lifecycle/serving.md`](../ml-lifecycle/serving.md) §3).
- Identity-stripping does nothing against response tampering; that needs the separate result-integrity machinery (canaries, spot-checks, reputation stakes — [`../platform/security.md`](../platform/security.md) §3.5).

## Revisit when

Tier C capacity (ADR-0008) becomes broad enough that content-confidential serving is the default answer for sensitive workloads, or measured re-linkage via timing correlation proves strong enough to demand a harder scattering scheme. Applies to serverless inference only; managed jobs rely on ephemeral hygiene (ADR-0007).
