# Self-hosting Loom

**Status:** Design (July 2026)
**Scope:** The self-hosting guide-as-spec. How a single ML engineer, or a small team, runs the *entire* Loom backend on their own hardware — the full data → train → eval → serve loop — with no operator, no marketplace, and no bill. Covers the two self-host profiles (standalone one-box; private fleet), their quickstarts as full command transcripts, ops, resource footprint, and troubleshooting.

**Read alongside:** [../architecture/profiles.md](../architecture/profiles.md) (the three deployment profiles and what each includes), [../platform/host-agent.md](../platform/host-agent.md) (the agent that runs jobs on GPU machines), [./deployment.md](./deployment.md) (the *hosted* marketplace onboarding — a different path), and the ML-lifecycle docs ([training](../ml-lifecycle/training.md), [recipes](../ml-lifecycle/recipes.md), [serving](../ml-lifecycle/serving.md), [environments](../ml-lifecycle/environments.md), [data](../ml-lifecycle/data.md)) for what actually runs inside a job.

---

## 1. Who this is for, and the promise

Loom's hosted marketplace ([deployment.md](./deployment.md)) is one way to use Loom. Self-hosting is the other, and for a large class of users it's the *primary* one: you already own the GPU. You don't want to rent a stranger's 4090, strip your own identity from your own requests, or pay a platform fee to fine-tune on a card sitting three feet away. You want the **managed ML lifecycle** — content-addressed datasets, cost-estimated recipes, interruption-tolerant checkpointing, an OpenAI-compatible endpoint — running on **your** silicon, on **your** network, answerable only to you.

That is exactly what self-hosting delivers. The same recipes, the same curated images, the same inference API — minus the marketplace machinery (billing, identity-stripping, reputation, relay fabric) that only exists because the hosted product runs across untrusted strangers. On your own box there are no strangers.

**Two concrete promises, and we design against them as hard targets:**

- **Standalone, one box, under 15 minutes.** A single ML engineer with a gaming PC or a fresh GPU server goes from nothing to a working train/eval/deploy loop — `loom train --recipe qlora-sft` producing an adapter, `loom deploy` standing up a local endpoint — in under a quarter of an hour, most of which is a download progress bar.
- **Private fleet, three machines, under 30 minutes.** A small team turns three spare rigs into a private training cluster: one machine runs the coordinator; the others enroll as GPU workers; `loom run --gpu all` fans work across all three.

**Setup stays small on purpose.** What you install is two binaries — `loomd` (the coordinator) and `loom-hostd` (the GPU-machine agent), plus the `loom` CLI — each **tens of megabytes**, statically linked, no JVM, no Kubernetes, no Python runtime to manage. The *big* things (multi-GB CUDA runtime images, model weights) do **not** arrive at install time. They're pulled **lazily on first use** — the container image streams in on your first job via Nydus lazy pull ([environments.md](../ml-lifecycle/environments.md) §6), the base model downloads the first time a recipe references it — each with a live progress bar. Setup itself is small and fast; the heavy bytes show up only when a job actually needs them, and only once (they're cached after that).

This is the whole "it shouldn't be huge files to set up" story, stated plainly: **the control plane is small; the ML payloads are large, lazy, cached, and capped.** §7 gives the numbers.

---

## 2. Standalone quickstart — the full transcript

The standalone profile is `loomd` as a **single Rust binary with embedded SQLite, an in-process job queue, an embedded inference gateway, and a local content-addressed artifact store** — no Postgres, no NATS, no MinIO, no relay ([../architecture/profiles.md](../architecture/profiles.md)). One process is the entire backend. `loom-hostd` runs alongside it to drive jobs on the local GPU. This is the "everything on one box" profile.

### 2.1 Prerequisites & `loom doctor`

Loom self-hosts on **Linux + NVIDIA** first (the same floor as the hosted fleet — [deployment.md](./deployment.md) §9). You need:

- Linux (Ubuntu 22.04+/Debian 12+/Fedora 39+, x86-64),
- an NVIDIA driver **≥ the floor for the images you'll run** — the CUDA 12.x family floor is `525.60.13`, and each curated image publishes a higher practical floor ([environments.md](../ml-lifecycle/environments.md) §3),
- a **container runtime — Docker or Podman** — which is required only to *run jobs* on the GPU (it's how the curated image is launched and isolated). The coordinator itself needs nothing but the binary.

If any of that is missing, you don't get a cryptic failure three steps later. `loom doctor` checks it up front and prints guided fixes:

```
$ loom doctor
loom doctor — checking this machine for standalone self-host

  OS            Ubuntu 24.04 (x86-64)                    ✓
  NVIDIA GPU    NVIDIA GeForce RTX 4090 (24 GB)          ✓
  Driver        565.77   ✓ (family floor 525.60.13; loom/train wants ≥ 550)
  Container     ✗ no Docker or Podman found
                → Loom runs jobs inside a container runtime. Install one:
                    Ubuntu/Debian:  sudo apt install podman
                    or Docker:      https://docs.docker.com/engine/install/
                  Rootless Podman is fine and recommended for a desktop.
                  Re-run `loom doctor` when done.
  IOMMU         not enabled in BIOS
                → Optional. Without it you get Tier B (hardened container),
                  which is the standalone default. Tier A (VFIO microVM)
                  needs IOMMU; you do NOT need it to self-host.
  Disk          412 GB free on /var/lib/loom   ✓ (min 50 GB; images/weights grow into this)

One blocker: no container runtime. Fix it above, then re-run `loom doctor`.
```

`loom doctor` is fail-*informative*, never fail-silent: every red line pairs with the exact command to fix it, mirroring the probe-rejection UX the host onboarding uses ([deployment.md](./deployment.md) §2). Driver-too-old and container-runtime-missing are the two common blockers; both print a one-liner. IOMMU-absent is **not** a blocker — it only gates the optional Tier A microVM (§5, §8).

### 2.2 Install

One command installs all three binaries, checksummed and signature-verified (same signed-artifact posture as the host-agent installer — [host-agent.md](../platform/host-agent.md) §9, §10):

```
$ curl -fsSL https://get.loom.dev/self-host | sh

loom self-host installer
  detected: linux/amd64
  fetching loomd        27 MB  ✓ sha256 ✓ signature
  fetching loom-hostd   19 MB  ✓ sha256 ✓ signature
  fetching loom (CLI)   22 MB  ✓ sha256 ✓ signature
  installed to /usr/local/bin   (loomd, loom-hostd, loom)

Next:  loom init --standalone
```

As with the host installer, `curl -fsSL https://get.loom.dev/self-host` **without** `| sh` prints the script so you can read it first; the binaries are SHA-256-checked and signature-verified against a key baked into the installer, and `apt`/`dnf`/Homebrew packages are the package-manager path for anyone who won't pipe curl to a shell ([deployment.md](./deployment.md) §2, [host-agent.md](../platform/host-agent.md) §9). Total download so far: **~68 MB**. No runtime image, no model weight — those are lazy (§1).

### 2.3 `loom init --standalone`

```
$ loom init --standalone

loom init — standalone profile
  writing   /etc/loom/loom.toml            (standalone; embedded SQLite + queue + gateway)
  creating  /var/lib/loom/loom.db          (SQLite — source of truth)
  creating  /var/lib/loom/artifacts/       (local content-addressed store)
  minting   local admin token
  probing   localhost GPU via loom-hostd …
              NVIDIA GeForce RTX 4090 (24 GB) · driver 565.77 · Tier B eligible
  registering localhost as GPU node "local"

  ✓ standalone Loom is up.
      admin token:  loom_admin_7f3a…  (saved to ~/.config/loom/credentials)
      API:          http://127.0.0.1:8443
      dashboard:    http://127.0.0.1:8443/  (single-admin, token-gated)

  Try a job:  loom run --gpu auto -- python -c "import torch; print(torch.cuda.get_device_name())"
```

`init` writes a `loom.toml`, creates the SQLite database and the local artifact directory, mints a **local admin token** (not a `loom_sk_` marketplace key — those exist only in hosted mode; standalone auth is a single trusted admin token), and registers the local GPU by having `loom-hostd` probe it. One command, no account, no browser, no network round-trip to an operator. You are the operator now.

### 2.4 First job

```
$ loom run --gpu auto -- python -c "import torch; print(torch.cuda.get_device_name())"

  [prepare]  node local selected
  [prepare]  image loom/torch:2026.07-cu126-torch2.12 not resident
             lazy-pulling (Nydus) ███████████████░░░░░  71%  4.9/6.9 GB  · 128 MB/s
  [prepare]  image resident · container starting  ✔
  [run]      GPU meter n/a (self-host: no billing) · running
  NVIDIA GeForce RTX 4090
  [done]     exit 0 · 00:41 wall
```

The first job pays the one-time lazy image pull — the `loom/torch` image streams in and starts executing before every layer has landed ([environments.md](../ml-lifecycle/environments.md) §6). Note there is **no cost line**: standalone has no billing (§6). The second run is near-instant because the image is now cached on the node:

```
$ loom run --gpu auto -- python -c "import torch; print(torch.cuda.get_device_name())"
  [prepare]  node local selected · image resident (cache hit)  ✔
  [run]      running
  NVIDIA GeForce RTX 4090
  [done]     exit 0 · 00:03 wall
```

### 2.5 The full loop: data → train → eval → deploy → serve

Now the payoff — the managed lifecycle, identical to the hosted narrative ([deployment.md](./deployment.md) §3b, [recipes.md](../ml-lifecycle/recipes.md) §7), running entirely local:

```
# 1. Push a dataset → immutable, content-addressed manifest (data.md).
$ loom data push ./sft_data.jsonl --name my-sft
  scanning ./sft_data.jsonl ..... 41,207 examples, 47 MB
  chunking + hashing ............ 52 chunks (0 already in store)
  storing 52 chunks → /var/lib/loom/artifacts/   done
  manifest: my-sft@v1  sha256:9f3c… (immutable)

# 2. Fine-tune with a curated recipe. --dry-run first to see the plan.
$ loom train --recipe qlora-sft \
    --base meta-llama/Llama-3.1-8B --data my-sft@v1 \
    --gpu auto --epochs 3 --lora-r 16 --dry-run
  Recipe     qlora-sft@3   (image sha256:d91f4c…)
  Base       meta-llama/Llama-3.1-8B  (8B, QLoRA nf4-double)  — will download on first use
  Dataset    my-sft@v1  ~34M tokens (3 epochs)
  Placement  1× local rtx4090 (24GB) · est. peak VRAM ~13.5/24 GB  ✓ fits
  Eval       instruction-following (auto, after train)
  Dry run only — nothing scheduled. (No cost estimate: self-host has no billing.)

# 3. Run it for real. Resumable-by-default; streams loss.
$ loom train --recipe qlora-sft \
    --base meta-llama/Llama-3.1-8B --data my-sft@v1 \
    --gpu auto --epochs 3 --lora-r 16 --yes
  [prepare]  base meta-llama/Llama-3.1-8B not cached
             downloading weights ██████████░░░░░░░░░  53%  8.5/16 GB · 240 MB/s
  [prepare]  base resident · image resident  ✔
  [run]      QLoRA · micro_batch=8 (auto) · grad_ckpt on
  [step 200] loss 1.412 · ckpt@… saved locally · 00:18 elapsed
  [step 3600] loss 0.887 · 01:52 elapsed
  [done]     checkpoint ckpt@a17e9f  ·  adapter (74 MB) + model card
  [eval]     instruction-following → report ev@7b1a (score 0.68)  ✔
  [lineage]  my-sft@v1 + qlora-sft@3 + loom/train@sha256:d91f… + base:llama-3.1-8b → ckpt@a17e9f

# 4. Deploy the adapter behind a local OpenAI-compatible endpoint.
$ loom deploy adapter:a17e9f --name my-model
  adapter placed on local base replica (llama-3.1-8b)
  → http://127.0.0.1:8443/v1   (model = "my-model")

# 5. Call it — one line different from OpenAI, pointed at localhost.
$ curl http://127.0.0.1:8443/v1/chat/completions \
    -H "Authorization: Bearer $LOOM_ADMIN_TOKEN" \
    -d '{"model":"my-model","messages":[{"role":"user","content":"hi"}]}'
  {"choices":[{"message":{"role":"assistant","content":"Hello! …"}}], …}
```

The base model downloads once (step 3, cached thereafter); the adapter is 74 MB; the endpoint is the **embedded inference gateway** inside `loomd` — no separate serving process to stand up, no relay, because the caller and the node are on the same box. The `loom deploy` → OpenAI-call path is byte-for-byte the hosted one, with `http://127.0.0.1:8443` in place of `https://inference.loom.dev` and the local admin token in place of a `loom_sk_` key. That's the point: **write against self-hosted Loom, move to hosted Loom later, change only the base URL and the key.**

---

## 3. New-GPU-server scenario (headless)

The founder's explicit case: *"I have a new server, deploy the entire stack for training/data/inference."* A fresh headless Ubuntu box with a GPU, no desktop, reached over SSH. The flow is the standalone flow with three headless adjustments.

**Install the driver first (the one thing Loom can't do for you).** Loom does not ship or manage the kernel-mode NVIDIA driver — the host owns it ([environments.md](../ml-lifecycle/environments.md) §3.1). On a fresh Ubuntu server:

```
$ sudo ubuntu-drivers install     # or the CUDA-repo .run for a specific version
$ nvidia-smi                      # confirm the driver + card are visible
```

`loom doctor` will tell you the exact floor if the installed driver is too old.

**Install Loom and init for remote access:**

```
$ curl -fsSL https://get.loom.dev/self-host | sh
$ loom init --standalone --listen 0.0.0.0
  … (as §2.3) …
  API:  http://0.0.0.0:8443   ← reachable from your laptop; token-authenticated
```

`--listen 0.0.0.0` binds the API to all interfaces so your laptop's `loom` CLI can drive the server remotely. **Auth is the local admin token** — every request carries it, so a bound-to-`0.0.0.0` API is not an open door. Point your CLI at it:

```
# on your laptop
$ loom config set server http://my-server.example:8443
$ loom config set token loom_admin_7f3a…
$ loom ps        # now talks to the remote server
```

**The installer provides systemd units** so the stack survives reboots and runs unattended — this is the headless whole-stack deployment the founder asked for:

- `loomd.service` — the coordinator + embedded gateway + queue.
- `loom-hostd.service` — the GPU agent.

Both are installed disabled-by-default and enabled by `loom init`; `systemctl status loomd loom-hostd` shows health, and both self-restart on failure with backoff (same hardened-unit posture as the host agent — [host-agent.md](../platform/host-agent.md) §9).

**Firewall: one port.** The only inbound port is the API (default `8443`). Open exactly that, ideally scoped to your own IP or VPN:

```
$ sudo ufw allow from <your-ip> to any port 8443 proto tcp
```

There is no relay, no NAT traversal, and no second port: standalone talks to nothing outbound except the curated-image registry and Hugging Face for the lazy pulls, and inbound only on the API.

**Notebooks and TensorBoard, over the API — no relay needed.** On the hosted product, interactive sessions tunnel through the outbound-only relay because the host is a NAT'd stranger ([deployment.md](./deployment.md) §4). Self-hosting has no such problem: your server is directly reachable, so `loom notebook` and `loom port-forward` are **direct forwards over the API port**, not relay-brokered:

```
$ loom notebook --gpu auto
  Jupyter on node local · forwarding → http://127.0.0.1:8888 (via API tunnel)
$ loom port-forward <job> 6006:6006     # TensorBoard, same direct path
```

No public port for Jupyter, no relay hop — the forward rides the already-open, already-authenticated API connection.

---

## 4. Private fleet — from one box to a cluster

The **private-fleet** profile keeps `loomd` on one machine (the coordinator, still embedded SQLite + queue + gateway) and adds GPU workers by running **only `loom-hostd`** on the other machines ([../architecture/profiles.md](../architecture/profiles.md)). This is a small team turning 3 spare rigs into a private training cluster in under 30 minutes: stand up standalone on the first box (§2), then enroll the others.

**On each new GPU machine**, install and enroll against the coordinator:

```
# on the second/third box
$ curl -fsSL https://get.loom.dev/self-host | sh     # gets loom-hostd + CLI
$ loom-hostd enroll --server https://loom.mytailnet:8443 --token loom_admin_7f3a…
  loom-hostd: probing this machine…
    NVIDIA GeForce RTX 3090 (24 GB) · driver 550.90 · Tier B eligible  ✓
  enrolled as node "rig-2" with coordinator loom.mytailnet:8443  ✓
```

Enrollment probes the card, confirms it meets the driver floor for the images it'll run, and registers it with the coordinator using the same admin token. Repeat on rig-3. Now the coordinator sees three GPU nodes, and the scheduler fans work across them:

```
$ loom run --gpu all -- python bench.py
  [prepare]  fanning to 3 nodes: local, rig-2, rig-3
  ✓ local   (rtx4090)  done  00:58
  ✓ rig-2   (rtx3090)  done  01:11
  ✓ rig-3   (rtx3090)  done  01:09
```

`--gpu all` runs across every enrolled node (the multi-node-CI shape from [deployment.md](./deployment.md) §3a, now on *your* machines); `--gpu auto` picks one; `--gpu rtx4090` targets a class. A `full-ft-small` recipe that needs a 2–4 GPU host ([recipes.md](../ml-lifecycle/recipes.md) §3) schedules onto whichever enrolled machine actually has multiple cards — single-node multi-GPU only, per the physics ([training.md](../ml-lifecycle/training.md) §1e); the fleet parallelizes *independent* jobs across machines, it does not shard one job's gradients over the LAN unless the GPUs are in one host.

**Network: LAN or a mesh VPN.** For machines in one room on one switch, a private LAN is fine. For machines split across sites, put them on a **WireGuard or Tailscale mesh** and enroll against the coordinator's mesh address (the `loom.mytailnet:8443` above) — the same start-relayed-then-direct model the platform uses ([../platform/networking.md](../platform/networking.md)), except you run the mesh, not us. Cross-site enrollment over a mesh keeps the "one open port" property: only the coordinator's API port, only on the mesh.

**Scaling ceiling, and the escape hatch.** Embedded SQLite comfortably coordinates a private fleet to **~dozens of nodes** — the coordinator's job is dispatch and bookkeeping, not a hot path. Beyond that, or if you want HA on the coordinator, migrate the source of truth to Postgres (per the backend design):

```
$ loomd migrate --to postgres --dsn postgres://…
  migrating SQLite → Postgres … 14 tables, 3.2k rows … done
  loomd now using Postgres as source of truth. Restart to apply.
```

This is a one-way door you take only when you outgrow SQLite; the default private fleet never needs it.

---

## 5. What you get vs. the hosted marketplace

Self-hosting is the **same ML product** with the marketplace machinery removed. The machinery only exists to make *untrusted strangers* transact safely; on your own hardware it's dead weight, so it's gone.

| Capability | Self-host (standalone / fleet) | Hosted marketplace |
|---|---|---|
| Curated images ([environments.md](../ml-lifecycle/environments.md)) | ✓ same catalog, same digests | ✓ |
| Recipes (`qlora-sft`, `dpo`, …) ([recipes.md](../ml-lifecycle/recipes.md)) | ✓ same, version-pinned | ✓ |
| `loom-ckpt` interruption tolerance ([training.md](../ml-lifecycle/training.md)) | ✓ (matters for owner-eject / desktop sharing, §6) | ✓ |
| OpenAI-compatible inference API | ✓ embedded gateway, `127.0.0.1`/LAN | ✓ operator gateway |
| Content-addressed data + lineage ([data.md](../ml-lifecycle/data.md)) | ✓ local artifact store | ✓ object store |
| Sandbox isolation (Tier B default) | ✓ **on by default** (see below) | ✓ |
| Tier A VFIO microVM | ✓ if you enable IOMMU | ✓ |
| **Billing / per-second metering** | ✗ none — you own the electricity | ✓ |
| **Marketplace / pricing / reputation** | ✗ no strangers to price or rank | ✓ |
| **Identity-stripping gateway** | ✗ unnecessary — no third party to hide from | ✓ (primary renter-from-host protection) |
| **Relay fabric / NAT traversal** | ✗ direct reachability on your network | ✓ |
| Auth model | single **local admin token** | scoped `loom_sk_` keys, accounts |

**Why isolation is still on by default — even for your own code.** The obvious question: if I'm the only user and it's my own job, why sandbox it at all? Because the threat on a single-tenant box isn't a malicious *tenant* — it's a **malicious dependency**. A `pip install` in a fine-tuning job pulls a transitive tree of packages any one of which could be compromised (typosquats, hijacked maintainer accounts, poisoned build steps — the standard supply-chain attack surface). Running that job in a hardened container (Tier B, the standalone default — gVisor `runsc` where the workload tolerates it, per [../platform/isolation.md](../platform/isolation.md)) means a bad wheel can't read your SSH keys, exfiltrate your other datasets, or pivot to the rest of your network. The curated-image + egress-allowlisted-mirror model ([environments.md](../ml-lifecycle/environments.md) §8) is defense-in-depth against *your own* dependencies, not against you. It costs you nothing and it's the difference between "a poisoned dep ran in a box" and "a poisoned dep ran as you." Leave it on.

---

## 6. Operations for self-hosters

You are the operator, so here's the operator runbook. All of it is boring on purpose.

**Upgrade.** Two moving parts, two mechanisms:

- **CLI:** `loom self-update` fetches, checksums, signature-verifies, and swaps the `loom` binary in place.
- **Coordinator + agents:** `loomd` and `loom-hostd` do a **staged upgrade with automatic rollback** — the updater stages the new binary, keeps the previous one, and if the new version crash-loops within a short probation window it reverts automatically and reports the failure (the same signed-release, keep-previous-binary, crash-loop-rollback logic the host agent uses — [host-agent.md](../platform/host-agent.md) §9). `loomd upgrade` triggers it; on a fleet, workers upgrade behind the coordinator.

```
$ loom self-update
  loom 2026.07 → 2026.08  ✓ sha256 ✓ signature  installed
$ loomd upgrade
  staging loomd 2026.08 … probation 90s … healthy ✓  (rollback armed but not needed)
```

**Backup.** The entire state of a standalone install is **one SQLite file plus the artifact directory**. Back both up and you can restore the whole platform:

```
$ rsync -a /var/lib/loom/  backup-host:/loom-backups/$(hostname)/
```

That's it — `loom.db` (datasets, jobs, lineage, deployments) and `artifacts/` (chunks, checkpoints, adapters). No object store to snapshot, no cluster state. On the fleet profile, back up the coordinator's `/var/lib/loom`; workers hold only cache and are disposable.

**Disk management.** Images and weights are the only large consumers (§7), and they're **capped and prunable**:

- **Cache caps** in `loom.toml`: `image_cache_gb` and `weight_cache_gb` bound how much the node keeps; the cache manager evicts LRU past the cap (same mechanism as the host agent's cache manager — [host-agent.md](../platform/host-agent.md) §2).
- **`loom cache prune`** reclaims on demand:

  ```
  $ loom cache prune --keep-recent 2
    evicting 3 stale images, 2 cold model weights … reclaimed 41 GB
  ```

- **Checkpoint retention** is keep-last-N per run (recipe default `keep_last_n: 3` — [recipes.md](../ml-lifecycle/recipes.md) §2); older checkpoints prune automatically so a long training run doesn't fill the disk.

**Sharing a GPU with your desktop — the idle policy works for *you*.** The host agent's owner-idle policy isn't just for renting to strangers; it's exactly what you want on a machine you also game or work on. Configure `loom-hostd` to only claim the GPU when *you're* not using it — the same scheduling-window + foreground-GPU-process detection the agent uses to protect a host ([host-agent.md](../platform/host-agent.md) §8):

```toml
# loom.toml (or the hostd config)
[idle_policy]
claim_when = "idle > 10min"      # only run Loom jobs when the desktop is quiet
foreground_gpu_yields = true     # a game/app grabbing the GPU ejects the job
```

And the **eject button** is yours too: `loom-hostd eject` (or *"Give me my GPU back"* in the tray) immediately checkpoints the running job via `loom-ckpt` and vacates the card, so you can launch a game without killing your fine-tune — it resumes from the exact step when the GPU frees up ([training.md](../ml-lifecycle/training.md) §3, [host-agent.md](../platform/host-agent.md) §8). This is why `loom-ckpt` is on by default even in self-host: *you* are the interrupting owner.

**Uninstall.** Clean and complete:

```
$ loomd stop && loom-hostd stop
$ loom uninstall            # removes systemd units, binaries; prompts before data
  remove /etc/loom, /var/lib/loom (loom.db + artifacts)?  [y/N]
```

`loom uninstall` disables and removes the systemd units and the three binaries, and asks before deleting `/etc/loom` and `/var/lib/loom` so a reinstall-in-place is possible. On workers, `loom-hostd unenroll` drops the node from the coordinator first.

---

## 7. Resource footprint

The headline: **the Loom control plane is small; only the ML payloads are large, and those are capped and prunable.** Idle budgets, from the backend design:

| Component | Idle footprint | Notes |
|---|---|---|
| `loomd` (coordinator + embedded SQLite + queue + gateway) | **< 100 MB RSS idle** | One Rust process; no JVM, no Postgres, no NATS. Grows only while serving/scheduling. |
| `loom-hostd` (GPU agent) | **< 30 MB RSS idle** | tokio binary holding a connection + sampling hardware ([host-agent.md](../platform/host-agent.md) §1). |
| Binaries on disk | **tens of MB** | `loomd` ~27 MB, `loom-hostd` ~19 MB, `loom` ~22 MB — statically linked. |
| Base disk before images | **< 200 MB** | binaries + `loom.db` + empty artifact store. |
| Curated runtime images | multi-GB **each**, **lazy + cached + capped** | Streamed on first use (Nydus, [environments.md](../ml-lifecycle/environments.md) §6); shared base layers dedupe; bounded by `image_cache_gb`. |
| Model weights | GB-scale **each**, **lazy + cached + capped** | Downloaded on first reference; bounded by `weight_cache_gb`; `loom cache prune` reclaims. |
| Checkpoints | adapter = tens of MB; full-model = GB | keep-last-N pruned ([recipes.md](../ml-lifecycle/recipes.md) §2). |

So a freshly installed, idle standalone Loom is **under ~130 MB of RAM and under 200 MB of disk**. The multi-GB numbers are all *ML content you asked for* — the CUDA image your job runs in, the base model your recipe fine-tunes — never the platform itself, and every one of them is lazy (arrives on first use), cached (arrives once), and capped (you set the ceiling, `loom cache prune` reclaims). That's the "shouldn't be huge files to set up" promise made concrete: **install is tens of MB; the heavy bytes are opt-in, on-demand, and bounded.**

---

## 8. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `loom doctor`: `driver 535.x < floor 550` | Host NVIDIA driver below the image's floor ([environments.md](../ml-lifecycle/environments.md) §3) | Upgrade the driver (`sudo ubuntu-drivers install` or the CUDA-repo `.run`), then re-run `loom doctor`. Loom can't manage the kernel driver — you own it. |
| `loom doctor`: `no Docker or Podman found` | No container runtime; jobs can't be isolated/launched | `sudo apt install podman` (rootless is fine) or install Docker Engine. The coordinator runs without it; **jobs** need it. |
| `Tier A unavailable — IOMMU not enabled` | No VFIO passthrough without IOMMU in BIOS/kernel | **Not a blocker.** Standalone defaults to **Tier B** (hardened container), which needs no IOMMU. Enable IOMMU only if you specifically want the microVM tier ([../platform/isolation.md](../platform/isolation.md)). |
| Recipe rejected at submit: `est. peak VRAM 26GB > 24GB` | Model/settings exceed the card; the recipe's VRAM estimator caught it pre-flight ([recipes.md](../ml-lifecycle/recipes.md) §2, [training.md](../ml-lifecycle/training.md) §8) | Follow the named knobs: `--grad-checkpointing`, smaller `--micro-batch`, `--seq-len` down, drop to QLoRA, or target a bigger card in the fleet. The estimator warns *before* you spend the run. |
| `loom init`/`enroll`: `address already in use :8443` | Port conflict (another service, or a second `loomd`) | `loom init --standalone --listen 0.0.0.0:9443` (or set `api_port` in `loom.toml`); update the CLI's `server` config to match. |
| Worker won't enroll: `cannot reach coordinator` | Firewall or wrong address across sites | Confirm the one API port is open to the worker (or that both are on the WireGuard/Tailscale mesh) and enroll against the coordinator's reachable address (§4). |
| Job evicted the moment you launch a game | `idle_policy` is doing its job | Expected — `loom-ckpt` checkpointed it; it resumes when the GPU frees. Loosen `claim_when` if you want Loom to hold the card longer (§6). |

---

## 9. Open questions

1. **Standalone auth beyond a single admin.** Standalone is one trusted admin token by design. A small team on a private fleet may want *several* named tokens (per-person, revocable) without pulling in the full hosted account/identity system. Where's the line — a lightweight local token table in SQLite, or is single-admin genuinely enough until you're big enough for Postgres + real identity? *(Bridge note: [renter-api.md](../platform/renter-api.md) §1.2 specifies only scoped `loom_sk_…` API keys and does not yet describe the standalone `loom_admin_…` local token; per [backend.md](../platform/backend.md) §8 the standalone `[auth] mode = "single_token"` issues one implicit full-scope token and skips the key-management routes, so the two are reconciled at the backend layer — renter-api.md should gain a one-line standalone-auth pointer when it is next revised.)*
2. **`loomd migrate --to postgres` reversibility and HA.** Migration is presented as a one-way door. Do we support migrating *back* (fleet shrinks again), and does Postgres-mode imply a documented HA topology for the coordinator, or is HA explicitly out of scope for self-host?
3. **Lazy-pull UX on slow home uplinks.** The first-job image pull and first-recipe weight download are multi-GB. On a slow residential downlink the "under 15 minutes" promise is download-bound, not Loom-bound. Do we pre-warn in `loom doctor` (measure downlink, estimate first-pull time) so the promise stays honest?
4. **Cross-site fleet without a user-run mesh.** We recommend WireGuard/Tailscale for multi-site fleets and explicitly *don't* run a relay in self-host. Is there demand for an optional self-hostable relay for teams who can't run a mesh, or does that reintroduce exactly the complexity self-host exists to avoid?
5. **Desktop GPU-sharing polish.** The idle-policy + eject path is inherited from the host agent, but the desktop-sharing UX (tray, "give me my GPU back", how aggressively to yield) may want self-host-specific defaults distinct from the rent-to-strangers tuning. What's the right out-of-box `idle_policy` for a personal machine?
6. **Standalone → hosted graduation.** A user who self-hosts and later wants to *also* rent out spare capacity, or burst onto the marketplace, currently reinstalls under a different profile. Is there a clean in-place graduation path, and does the local admin token / `loom_sk_` key split make that awkward?

---

*Related: [../architecture/profiles.md](../architecture/profiles.md) (deployment profiles) · [./deployment.md](./deployment.md) (hosted marketplace onboarding) · [../platform/host-agent.md](../platform/host-agent.md) (the GPU agent, idle policy, eject, self-update) · [../ml-lifecycle/environments.md](../ml-lifecycle/environments.md) (curated images, lazy pull, driver floors) · [../ml-lifecycle/recipes.md](../ml-lifecycle/recipes.md) · [../ml-lifecycle/training.md](../ml-lifecycle/training.md) · [../ml-lifecycle/serving.md](../ml-lifecycle/serving.md) · [../ml-lifecycle/data.md](../ml-lifecycle/data.md) · [../platform/isolation.md](../platform/isolation.md).*
