# 0011 — Single-node scope: no WAN multi-node training or distributed data

**Status:** Accepted — 2026-07-07

## Context

Distributed training synchronizes gradients every step; distributed data processing shuffles bulk data between nodes on joins, group-bys, and repartitions. Both assume a fast interconnect — datacenter NVLink (~900 GB/s) or InfiniBand (hundreds of Gb/s). A Loom node has ~30 Mbps residential upload. Gradient sync or a cross-node shuffle over that link spends orders of magnitude more time communicating than computing; the GPUs sit idle waiting on the network. There is no algorithm that closes a five-order-of-magnitude interconnect gap ([`../ml-lifecycle/training.md`](../ml-lifecycle/training.md) §1). This is a physics constraint, not a roadmap gap.

## Decision

Scope both training and data processing to **single-node — scale up, not out** ([`../ml-lifecycle/training.md`](../ml-lifecycle/training.md) §1; [`../ml-lifecycle/data.md`](../ml-lifecycle/data.md) §2):

- **No multi-node WAN distributed training.** Jobs fit on one card (PEFT/LoRA/QLoRA 1–34 B, DPO/GRPO at PEFT scale, small-model full fine-tune and from-scratch), optionally multi-GPU **within one host** where the interconnect is at least PCIe. No pretraining large models from scratch. The scheduler enforces multi-GPU-node-only workloads onto qualifying rigs.
- **No multi-node WAN data clusters.** Single-node engines (HF `datasets`, Polars, DuckDB) cover ~90% of jobs; Ray Data/Daft handle GPU-in-the-loop and multimodal on **one big node or one LAN-local multi-GPU host**; Spark+RAPIDS is a curated single-node image for teams bringing existing pipelines. TB-scale scatter-across-strangers is explicitly refused — do it on a hyperscaler and push cleaned Parquet to Loom.
- This is the same reason as whole-model-per-node serving (ADR-0009) and no-Kubernetes (ADR-0004): residential-bandwidth reality forbids treating scattered NAT'd nodes as a fast cluster.

## Consequences

- The design targets the actual body of demand — fine-tunes and small trainings that fit a 24–32 GB card, and MB–GB datasets — made reliable by a platform that treats interruption as normal.
- Bulk data moves via object store + content-addressed cache + P2P, never node-to-node shuffle, keeping the network model coherent with [`../platform/networking.md`](../platform/networking.md).

**What we give up:**

- **This caps the addressable workload set.** 70 B+ full training, frontier-model pretraining, and genuine TB-scale distributed dedup are out of scope — Loom is the wrong tool and we say so at recipe/estimator time rather than after the renter pays ([`../ml-lifecycle/training.md`](../ml-lifecycle/training.md) §1; [`../ml-lifecycle/data.md`](../ml-lifecycle/data.md) §2).
- We forgo the "train anything at any scale" positioning; the ceiling is the biggest single box (or LAN multi-GPU rig) a host offers.

## Revisit when

We have demonstrated demand for models that don't fit one node **and** a cohort of hosts with adequate LAN interconnect ([`../ml-lifecycle/training.md`](../ml-lifecycle/training.md) §1e; roadmap punt list). On consumer NAT'd hardware this precondition is unlikely, so this may punt indefinitely. Weight/model sharding across nodes shares the same interconnect precondition.
