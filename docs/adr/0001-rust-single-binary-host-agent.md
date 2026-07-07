# 0001 — Rust single-binary host agent

**Status:** Accepted — 2026-07-07

## Context

The host agent is the one piece of Loom software that runs on a stranger's machine — a gaming rig, a workstation, an ex-mining box. It must be invisible when idle (under 30 MB RSS, near-zero CPU), install in one command, self-update unattended, and open no inbound ports. It also has to drive real privileged machinery: containers, Cloud Hypervisor microVMs, cgroups, mounts, VFIO binding, GPU reset. The install target is heterogeneous and untrusted, and the owner will not babysit it.

Three shapes were considered: a Go daemon, an Electron-style app, and a single statically-linked native binary. A Go daemon carries a garbage-collected runtime and a larger idle footprint; an Electron app bundles a browser and is absurd for a headless service. Neither meets the "unnoticeable guest with no runtime deps on a stranger's box" bar.

## Decision

Ship the agent as a **single statically-linked Rust binary** on a tokio runtime, a few megabytes, a handful of long-lived tasks communicating over channels. Crate choices are fixed in [`../platform/host-agent.md`](../platform/host-agent.md) §5: `tokio`, `rustls` (no OpenSSL), `quinn` for QUIC, `tokio-tungstenite` for WSS fallback, `nvml-wrapper` for inventory, `bollard`/`youki` for containers, `cloud-hypervisor-client` for microVMs. Install is `curl … | sh` fetching a signed binary plus a systemd unit ([`../platform/host-agent.md`](../platform/host-agent.md) §1, §9). Privilege is split: the large network-facing process runs unprivileged, and a small auditable `loom-hostd-helper` holds the root primitives behind a tight UNIX-socket command API ([`../platform/host-agent.md`](../platform/host-agent.md) §9).

## Consequences

- Tiny idle footprint and a trivial install make the agent an acceptable houseguest — the precondition for supply.
- No inbound ports means no inbound attack surface ([`../platform/networking.md`](../platform/networking.md)).
- The privilege split keeps the blast radius of a bug in the big process off root, while being honest that the system as a whole needs real privilege.

**What we give up:**

- Rust's smaller ML/infra ecosystem versus Go: some bindings are immature. `cloud-hypervisor-client` is unofficial and thin (we pin and are ready to talk the REST API directly), and there is no mature safe ROCm-SMI binding, gating the AMD fast-follow ([`../platform/host-agent.md`](../platform/host-agent.md) §11).
- A single binary means less runtime introspection than a scripted daemon; debugging is pull-style telemetry only, by design (no remote shell).
- Static linking and cross-platform QoS/egress primitives are Linux-first; non-Linux hosts need separate work.

## Revisit when

A crate we depend on (notably `cloud-hypervisor-client`) is abandoned and no in-house replacement is viable, or a concrete need for on-host dynamic behavior emerges that a static binary genuinely cannot serve. The privilege-split model itself is not up for revision.
