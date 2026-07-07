# 0005 — Outbound-only agent connectivity; QUIC primary, WSS fallback, relay + WireGuard upgrade

**Status:** Accepted — 2026-07-07

## Context

Home machines sit behind NAT, often CGNAT, and their owners will not — and should not — port-forward. An agent that required an inbound port would be uninstallable for most of our supply and would hand every host an inbound attack surface. Separately, the data plane (renter ↔ node, gateway ↔ node, node ↔ node) wants to go direct when possible because relaying costs us operator egress. This is the same box Tailscale, ngrok, and every reverse-tunnel product live in.

## Decision

**Every connection an agent makes is outbound.** The agent holds exactly one long-lived control channel to a connection-gateway ([`../platform/networking.md`](../platform/networking.md) §2):

- **Transport: QUIC over UDP/443 (quinn), primary.** Gives stream multiplexing, head-of-line-blocking avoidance, and connection migration across residential IP changes. **WSS over TCP/443 is the fallback** for UDP-hostile middleboxes — it looks like ordinary HTTPS and gets through nearly everywhere. The agent probes QUIC first and re-probes periodically ([`../platform/host-agent.md`](../platform/host-agent.md) §5).
- **Auth: mTLS** with per-enrollment identity keys; possession of the enrollment private key *is* the identity ([`../platform/networking.md`](../platform/networking.md) §2).
- **Data plane: DERP-style relay first, WireGuard direct upgrade.** Every session starts relayed through an operator relay; the endpoints attempt UDP hole-punching and, on success, upgrade to a direct WireGuard tunnel. Relays only forward WG-encrypted ciphertext ([`../platform/networking.md`](../platform/networking.md) §3). This mirrors Tailscale's start-relayed-then-upgrade model.

## Consequences

- No host ever exposes a port; the agent has no inbound attack surface, and NAT/CGNAT hosts join without router config.
- Direct WireGuard upgrades avoid operator egress cost where punching succeeds; relay guarantees connectivity even under symmetric/hard NAT.
- QUIC connection migration lets a running job survive a DHCP renewal or Wi-Fi→wired switch without re-handshaking.

**What we give up:**

- **The relay rate for our population is unknown and likely worse than Tailscale's ~90%-direct figure** — consumer GPU owners skew toward CGNAT, the hard case. Every relayed session costs operator egress, a real unit-economics line we must price in (ADR-0012; [`../platform/networking.md`](../platform/networking.md) §3).
- WSS fallback reintroduces TCP head-of-line blocking across our multiplexed streams and has no connection migration (a TCP reset forces full reconnect).
- QUIC connection-migration robustness must be proven per quinn version with a forced integration test; we do not assume it is transparent ([`../platform/networking.md`](../platform/networking.md) §2.3).

## Revisit when

Measured relay rates make operator egress cost unsustainable (drive a "direct-connectable" pricing signal or more traversal tricks), or the userspace WireGuard datapath choice (boringtun vs. GotaTun vs. kernel WG) needs revisiting as GotaTun matures ([`../platform/networking.md`](../platform/networking.md) §9). Outbound-only is not negotiable.
