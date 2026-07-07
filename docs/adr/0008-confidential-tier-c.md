# 0008 — Confidential Tier C via SEV-SNP/TDX + H100/Blackwell CC

**Status:** Accepted — 2026-07-07

## Context

The consumer-tier protections (identity-stripping ADR-0006, ephemeral hygiene ADR-0007) do not, and cannot, provide in-use confidentiality against an active malicious host on consumer silicon — accepted risk R1 in [`../platform/security.md`](../platform/security.md) §9. Some renters (regulated personal data, crown-jewel weights) need a real guarantee that the host cannot read weights or activations. The only mechanism that genuinely delivers this is hardware: a CPU TEE paired with a confidential-computing GPU, with remote attestation gating key release. Consumer RTX cards have none of this silicon, so this cannot be a feature flag on the mainstream marketplace — it is a distinct, smaller, differently-sourced supply pool.

## Decision

Add **Tier C** as a separate confidential-compute tier ([`../platform/security.md`](../platform/security.md) §6; roadmap Phase 5):

- **CPU TEE:** confidential VM on **AMD SEV-SNP** or **Intel TDX**, guest memory hardware-encrypted and opaque to the host hypervisor/kernel. Reuses the Tier A VMM path (ADR-0002). SEV-SNP is the lead path; Cloud Hypervisor TDX maturity is tracked, not yet committed.
- **GPU CC:** **NVIDIA H100 (Hopper)** first — VRAM firewall on, SPDM-attested session, encrypted PCIe bounce buffers. First target is **single-GPU H100** (no cross-GPU NVLink exposure); hardware-encrypted multi-GPU confidential arrives with **Blackwell** (TEE-I/O, encrypted NVLink under MPT CC).
- **Attestation-gated key release:** the weight-decryption key is released only after a key service verifies a hardware-signed measurement of the exact enclave and GPU CC state. Weights arrive encrypted; without a passing attestation the enclave gets ciphertext it cannot use. Attestation also solves work verification on this tier ([`../platform/security.md`](../platform/security.md) §6.2).
- **Protocol reserves `trust_tier` and attestation fields from day one** so Tier C slots into the existing wire format without a breaking change ([`../architecture/overview.md`](../architecture/overview.md); [`../platform/security.md`](../platform/security.md) §6.5).

## Consequences

- Tier C is the sole configuration where content is protected from the host — the compliant path for personal-data and sensitive-weight workloads, supporting an honest DPA ([`../platform/security.md`](../platform/security.md) §8).
- Reserving protocol fields now means the mainstream marketplace ships forward-compatible with a tier we won't build until Phase 5.

**What we give up:**

- Tier C supply is **not gaming rigs** — it needs SEV-SNP/TDX CPUs plus H100/Blackwell GPUs, a thin pool of small operators and colocation tenants, priced accordingly ([`../platform/security.md`](../platform/security.md) §6.5; roadmap Phase 5 risk).
- The bar is **not infinite**: CPU TEEs have a live side-channel/physical-attack literature (BadRAM, the TEE.fail DDR5 interposer that forged attestation quotes). Because GPU-CC attestation chains to the CPU-TEE root, a forged CPU quote can undermine GPU key release — attestation is only as strong as its weakest composed root (R9). We will not market Tier C as unbreakable.
- CC mode carries encryption/bounce-buffer overhead on host↔GPU transfers; first Tier C is single-GPU only until Blackwell.

## Revisit when

Cloud Hypervisor TDX reaches GA (currently SEV-SNP-led), Blackwell CC supply makes multi-GPU confidential real, or Tier C attestation becomes the escape valve that lets renters run verified custom images (tying ADR-0010's graduation path to the trust tier) ([`../platform/security.md`](../platform/security.md) §10).
