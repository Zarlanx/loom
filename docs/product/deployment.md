# Deployment: onboarding & everyday UX

This document owns the developer experience end-to-end — how a **host** goes from a bare machine to earning, and how a **renter** goes from signup to running compute, training, evaluating, and serving. It is the product surface that sits on top of the [host agent](../platform/host-agent.md), the [marketplace](./marketplace.md) mechanics, the [training stack](../ml-lifecycle/training.md), and the [serving stack](../ml-lifecycle/serving.md).

The founder's hard requirement governs every decision here: **setup must be quick.** Not "quick for infrastructure software" — quick like signing up for a SaaS. If onboarding takes an afternoon, we've already lost. So this doc leads with time budgets and treats them as commitments, not aspirations. Everything else is in service of those numbers.

---

## 1. Time-to-value budgets (commitments)

These are the numbers we design against, instrument, and alert on. If a p50 regresses past budget, it's a release blocker.

| Journey | Budget (p50) | Definition of "done" |
|---|---|---|
| **Host: install → first eligible listing** | **< 10 min** | `curl`-install completes, agent enrolls, probe finishes, at least one tier is marked eligible and the machine appears as listable capacity. **Zero BIOS work for Tier B.** |
| **Renter: signup → first job running** | **< 5 min** | Account created, CLI authed, `loom run` accepted, container pulled, process executing on a real GPU with logs streaming back. |
| **Renter: signup → first inference API call** | **< 2 min** | Account created, API key minted, an OpenAI-compatible request against a curated model returns a 200 with tokens. |

Three design consequences fall directly out of these budgets:

- **No account-review gate on the happy path.** Hosts and renters self-serve. Fraud/abuse controls run asynchronously and in the background (see [marketplace.md](./marketplace.md) reputation), never as a synchronous blocker before first value.
- **The probe must be fast and non-interactive.** Bandwidth measurement and NAT classification are the long poles; both are bounded (§2) so the 10-minute host budget holds even on a slow uplink.
- **Inference is a warm path.** The 2-minute number assumes a curated model is already resident in the [weight cache](../ml-lifecycle/serving.md) somewhere in the fleet. Cold-loading a model the renter chose is a job (the 5-minute path), not an API call.

We publish these budgets on the marketing site and show a live "typical setup time" counter derived from real p50s. Under-promising here is a competitive weapon: the incumbent experience for renting consumer GPUs is a multi-hour yak-shave.

---

## 2. Host onboarding walkthrough

The full agent lifecycle, owner controls, and self-update live in [host-agent.md](../platform/host-agent.md). This section is the **UX narrative**: what a host actually types and sees, with honest discussion of the trust surface.

### Step 0 — the install command, and the `curl | sh` question

```
curl -fsSL https://get.loom.dev | sh
```

We are not going to pretend `curl | sh` is above reproach — piping a remote script straight into a shell is asking a stranger to trust us with root-adjacent privileges on their machine. Here is exactly what we do to earn that trust, stated plainly so a skeptical host can verify it:

- **The installer script is tiny and auditable.** `curl -fsSL https://get.loom.dev` with no `| sh` prints the script to your terminal. It does three things: detect OS/arch, download the matching `loom-host` binary, and verify it. Read it before you run it — we link to the exact script in the docs and it's the same file served at that URL.
- **The binary is checksummed and signed.** The installer verifies a SHA-256 against a checksum served over a separate TLS-pinned path, then verifies a [minisign](https://jedisct1.github.io/minisign/)/cosign signature against a public key baked into the installer. A tampered CDN can't hand you a malicious binary without also breaking the signature.
- **Distro packages are coming.** `apt`/`dnf` repos and a Homebrew tap (for the renter CLI) land shortly after GA for hosts who won't run `curl | sh` on principle — same signed artifacts, distributed through package-manager trust. We say "later" honestly: it's not day-one.
- **Outbound-only, unprivileged main process.** The agent never opens an inbound port and runs its main process unprivileged, with a small root helper for exactly the operations that need it. That posture is specified in [host-agent.md](../platform/host-agent.md) §2 — worth linking a nervous host to directly.

The installer drops the `loom-host` CLI, installs a `systemd` unit (`loom-host.service`), and hands off to enrollment. Total wall time so far: seconds.

### Step 1 — enroll

```
loom-host enroll --token LOOM-8F3A-...-QK29
```

The enrollment token comes from the web dashboard ("Add a machine") or is generated headless via the CLI on an already-enrolled account. It binds this machine to the host's account and establishes the agent's identity (mTLS) with the [control plane](../platform/control-plane.md). No password, no inbound anything.

### Step 2 — the probe report

Enrollment immediately runs a hardware probe. This is the part the host watches, so it streams live and finishes in **under two minutes** on a normal connection:

```
loom-host: probing this machine…
  GPU          NVIDIA GeForce RTX 4090  (24 GB VRAM, 1 device)
  Driver       565.77   ✓ (min 550)
  CUDA         12.6 runtime available
  Disk         412 GB free on /var/lib/loom  ✓ (min 100 GB)
  Bandwidth    ↓ 611 Mbps   ↑ 92 Mbps        ✓ (min ↑ 50 Mbps)
  NAT          Full-cone (relay-friendly)    ✓
  IOMMU        not enabled in BIOS
```

Each line is a check with a plain-language verdict, not a raw dump. Bandwidth is a bounded active measurement against the nearest relay PoP (a few seconds, capped); NAT type is classified so the host knows whether they'll need to lean on the [relay fabric](../platform/networking.md) (they will — outbound-only is the whole point, and the relay handles it).

### Step 3 — eligibility verdict per tier

```
Eligibility
  Tier B (containers)   ✓ ELIGIBLE — ready to earn now
  Tier A (VFIO passthrough)   ⚠ AVAILABLE IF you enable IOMMU
      Tier A gives renters a dedicated GPU in a microVM (higher trust,
      higher pay). It needs IOMMU turned on in your BIOS.
      Run:  loom-host setup-tier-a   (guided, ~10 min + one reboot)
      This is optional. You can earn on Tier B today and add Tier A later.
```

The verdict is explicit and non-punitive. **Tier B never requires BIOS work** — that's the promise that keeps the 10-minute budget real for a daily-driver gaming rig. Tier A's extra setup (IOMMU toggle, VFIO binding, headless assumption) is surfaced as an *optional upgrade* with a guided walkthrough (`loom-host setup-tier-a` explains the BIOS steps for the host's detected motherboard vendor where we can, verifies afterward, and is fully reversible). The tier internals are in [isolation.md](../platform/isolation.md); the host never needs to read that to host.

### What the probe rejects — and how to fix it

The probe fails closed and always pairs a rejection with a fix:

| Rejection | Message the host sees | Fix-it guidance |
|---|---|---|
| **Driver too old** | `Driver 535.x < min 550. Renters' CUDA 12.x images won't run.` | One-liner to the vendor driver upgrade for their distro; re-probe with `loom-host probe`. |
| **VRAM too small** | `6 GB VRAM < min 8 GB for Tier B listings.` | Not fixable on this card. We explain that sub-8GB GPUs can't host current curated images and suggest CPU-side roles are out of scope at launch. |
| **Bandwidth floor** | `Upload 22 Mbps < min 50 Mbps. Weight/checkpoint transfer would stall jobs.` | Explain the floor exists to protect renters from slow nodes; suggest wired Ethernet; offer to re-test. Persistent low-bandwidth machines can still list but are down-ranked and capped to smaller jobs. |
| **No supported GPU** | `No NVIDIA GPU detected. Loom hosts Linux + NVIDIA first; ROCm is a fast-follow.` | Point to the support matrix (§9) and the roadmap. |

Rejections are the most common reason a host bounces, so the copy is written to be encouraging and specific — never "ineligible," always "ineligible *because X*, fixable by *Y*."

### Step 4 — pricing suggestion & go live

```
Suggested price   $0.34 / GPU-hr  (RTX 4090, your region, current demand)
                  You keep ~$0.27/hr after platform fee. Edit anytime.
Accept & list?  [Y/n]
```

We suggest a market-informed price (mechanics in [marketplace.md](./marketplace.md)) so the host isn't forced to guess. Accepting flips the machine to listable. Out of the box it still won't touch the GPU until the owner sets an **availability window** or toggles "available now" — the agent is safe-by-default (see [host-agent.md](../platform/host-agent.md) §8). Owner controls — availability windows, caps, and the **eject button** that reclaims the GPU immediately and evacuates any running tenant — are the agent's domain and covered there; we link, we don't duplicate.

From `curl` to a live, eligible listing: comfortably inside 10 minutes, with no reboot on the Tier B path.

---

## 3. Renter quickstarts — three narratives

The renter installs the CLI (`brew install loom` / `curl` installer / `pip install loom` shim) and runs `loom auth login`, which opens a browser once and caches a token. That's the shared preamble for all three. Now the concrete stories.

### (a) The "test across consumer hardware" engineer

An ML engineer maintains a Triton kernel / a PyTorch extension / an inference tool. Before a release they need to know it works across the messy reality of *consumer* GPUs — not the A100/H100 monoculture a hyperscaler rents. This is a genuine differentiator: **Loom's supply is exactly the long tail of consumer hardware that's otherwise impossible to test on in CI.** You cannot rent an RX 9070 XT or an RTX 5090 from a hyperscaler; on Loom they're the median node.

```
loom run \
  --gpu rtx4090,rtx5090,rx9070xt \
  --image loom/torch:2026.07-cu126-torch2.12 \
  --repo . \
  -- pytest tests/gpu/ -q
```

This fans one command out to three GPU models in parallel, syncs the working tree, runs the suite on each, and streams merged logs tagged by GPU. You get a matrix result:

```
✓ rtx4090   42 passed in 71s     $0.011
✓ rtx5090   42 passed in 58s     $0.014
✗ rx9070xt   2 failed in 63s     $0.008   (fp8 path — see log)
Total: $0.033
```

The `rx9070xt` failure is the entire point — you found a ROCm fp8 divergence before your users did, for three cents. This story is why the [GitHub Action](#7-ecosystem-integrations) matters so much (§7): the same command drops straight into CI.

### (b) The fine-tune → eval → deploy loop

The managed-lifecycle flow. Each step is one command; each prints a cost estimate first and streams progress.

```
# 1. Push a dataset as a versioned manifest (dedup'd, content-addressed)
loom data push ./sft_data.jsonl --name my-sft
#   → created my-sft@v1

# 2. Fine-tune with a curated recipe (knobs are recipe flags)
loom train --recipe qlora-sft \
  --base meta-llama/Llama-3.1-8B \
  --data my-sft@v1 \
  --epochs 3 --lora-r 16 \
  --gpu rtx4090
#   → estimate: ~1.8 GPU-hr, ~$0.61. Proceed? [Y/n]
#   → streams loss curve; checkpoints to loom storage; resumable

# 3. Evaluate the resulting adapter against a suite
loom eval --suite instruction-following \
  --model adapter:my-sft-run-3f2a \
  --base meta-llama/Llama-3.1-8B
#   → produces an eval report (also visible in the dashboard)

# 4. Deploy the adapter behind an OpenAI-compatible endpoint
loom deploy adapter:my-sft-run-3f2a --name my-model
#   → https://inference.loom.dev/v1  (model = "my-model")
```

Then it's a normal OpenAI call:

```
curl https://inference.loom.dev/v1/chat/completions \
  -H "Authorization: Bearer $LOOM_API_KEY" \
  -d '{"model":"my-model","messages":[{"role":"user","content":"hi"}]}'
```

Recipe internals (QLoRA, FSDP, TRL) live in [training.md](../ml-lifecycle/training.md); the endpoint, adapter hot-loading, and weight cache live in [serving.md](../ml-lifecycle/serving.md). The renter never touches either to complete the loop.

### (c) The pure-inference user

The 2-minute path, and the lowest-friction way onto the platform:

```
loom auth login
loom keys create --name prod        # prints: loom_sk_...
```

Then in application code, the only change from OpenAI is the `base_url`:

```python
from openai import OpenAI
client = OpenAI(base_url="https://inference.loom.dev/v1", api_key="loom_sk_...")
resp = client.chat.completions.create(
    model="llama-3.1-8b-instruct",
    messages=[{"role": "user", "content": "hello"}],
)
```

No `loom run`, no repo, no GPU flag. Change one line, done. This is the funnel's top: an inference user who likes the price and latency is a warm lead for the training loop.

---

## 4. CLI design

The CLI is the product for most renters. It's `loom`, single static binary, with a shallow, guessable command tree.

```
loom
├── auth        login / logout / whoami / status
├── keys        create / list / revoke              (scoped API keys: inference + control plane)
├── run         <flags> -- <cmd>   one-shot job on rented GPUs
├── train       --recipe ...       managed fine-tune  → training.md
├── eval        --suite ...        managed evaluation  → evaluation.md
├── deploy      model|adapter:...  stand up an endpoint → serving.md
├── data        push / pull / ls / rm    versioned manifests → data.md
├── ps          list running/recent jobs
├── logs        <job>  stream or tail logs
├── top         <job>  live GPU util / mem / power (nvtop-style)
├── ssh         <job>  interactive shell into the sandbox
├── exec        <job> -- <cmd>     run a command in a live job
├── port-forward <job> <remote:local>   tunnel a port through the relay
├── notebook    launch Jupyter on a GPU, port-forwarded to localhost
├── deployments list / logs / scale / rm   manage endpoints
└── config      set / get   (default GPU, region, output=json, …)
```

### UX principles (non-negotiable)

- **Every long op streams progress and is resumable.** Pulls, uploads, training, evals show live progress bars; if your laptop closes, the job keeps running server-side and `loom logs <job>` / `loom ps` re-attach. Jobs are cattle: a node dying mid-run resumes from checkpoint automatically (§6).
- **Every job prints a cost estimate before it starts and running cost while it runs.** `loom run`/`train` show `estimate: ~$X, proceed? [Y/n]`; the live log footer shows accrued spend (`$0.021 · 00:47 elapsed`). Per-second billing means these numbers are honest.
- **`--dry-run` everywhere.** On any command that spends money or mutates state, `--dry-run` prints the plan and the estimate and exits `0` without doing anything. Essential for scripting and for nervous first runs.
- **JSON output mode for scripting.** `--output json` (or `loom config set output json`) makes every command emit machine-readable JSON to stdout with human text on stderr — so `loom ps --output json | jq` and CI usage are first-class, not afterthoughts.
- **`--yes` / non-interactive.** Every confirmation prompt is skippable with `--yes` for automation.

### Interactive sessions

Interactive work has to feel local despite the GPU being on a stranger's machine behind NAT. Both mechanisms tunnel through the outbound-only [relay](../platform/networking.md) — there is never an inbound port on the host.

- **`loom notebook`** provisions a GPU, starts Jupyter inside the sandbox, and port-forwards it to `http://localhost:8888` on the renter's laptop through the relay. One command → a browser tab on a rented 4090. `loom port-forward <job> 6006:6006` does the same for TensorBoard.
- **`loom ssh <job>`** opens an interactive shell **inside the sandbox** (the container or microVM), not on the host OS — the renter sees only their tenant environment, and the host's machine is never exposed. `loom exec <job> -- nvidia-smi` runs a single command the same way. Semantics match SSH's mental model (a TTY, agent forwarding off by default) but the transport is a relay-brokered stream, so it works through any NAT.

---

## 5. Web dashboard scope (v1)

The dashboard complements the CLI; it is not a second full product. **Scope statement, not a spec:**

- **Renter view:** jobs (status, logs, cost), spend (per-job and rollup, billing), endpoints/deployments (URL, model, keys, live QPS), and eval reports (the rendered output of `loom eval`). Plus API-key management and a one-screen "run inference now" playground.
- **Host view:** earnings (paid/pending, payout history), utilization (hours listed vs. hours rented, per machine), health (agent online, GPU temp/util, recent job outcomes), and controls (availability windows, price, eject, add/remove machines).

Anything beyond read-mostly monitoring + the handful of controls above (team management, org billing, fine-grained RBAC, custom dashboards) is explicitly post-v1.

---

## 6. Failure UX

The part everyone skips. On a marketplace of consumer machines behind residential NAT, **failure is the normal case**, not the exception — so the failure experience is the product. The renter should feel the platform absorbing chaos on their behalf; the host should feel treated fairly.

**A node dies mid-job.** Nodes are cattle. The renter sees a single event in the same log stream they're already watching — no crash, no lost work:

```
[14:22:07] ⚠ node lost (host went offline)
[14:22:09] ↻ resuming from checkpoint step 4200 on new node (rtx4090, us-east)
[14:22:41]   resumed. ETA +6 min. no action needed.
```

Automatic checkpoint/resume (spec'd in [training.md](../ml-lifecycle/training.md)) makes this a footnote instead of a disaster. The renter is not billed for the dead node's re-run of already-checkpointed work; the marketplace handles the accounting ([marketplace.md](./marketplace.md)).

**No capacity matches the request.** Instead of a hard error, the renter gets a queue position and an actionable price lever:

```
No rx9070xt available at ≤ $0.30/GPU-hr right now.
  Queue position: 3   (est. wait 4 min at current price)
  Or raise your max to $0.38/GPU-hr to match a listed node now:  --max-price 0.38
  Or run now on rtx4090 (available):  --gpu rtx4090
```

**The job OOMs.** The pre-flight estimator (§1 warm-path, detailed in [training.md](../ml-lifecycle/training.md)) tries to catch this *before* spending money — `loom train` warns `est. peak VRAM 26GB > 24GB on rtx4090` at submit time. If it OOMs anyway, the message is actionable and names the knobs:

```
✗ CUDA out of memory at step 12 (used 24.0/24.0 GB).
  Try:  --grad-checkpointing   (trades compute for memory)
        --micro-batch 1        (currently 4)
        --gpu rtx5090          (32 GB)
  Your checkpoint at step 0 is saved; fixing and re-running resumes cheaply.
```

**The host's machine fails a job.** Transparency runs both directions. When a host's machine drops a job (offline, GPU fault, evicted late), the host sees it plainly, with the reputation consequence shown — not hidden:

```
Job job-9f2a ended early: host went offline mid-run.
  Reliability: 98.1% → 97.4%   (rolling 30-day completion)
  This affects your ranking and Tier A eligibility.
  Frequent drops during your availability window? Narrow the window so
  you only list when the machine is reliably free.
```

Reputation mechanics are owned by [marketplace.md](./marketplace.md); the UX obligation here is that a host is never surprised by a score change — the cause and the fix are always on screen.

---

## 7. Ecosystem integrations

Adoption comes from meeting developers where they already are. Priority order:

- **OpenAI SDK compatibility (inference).** Covered in §3(c): change `base_url`, keep your code. This is table stakes and the single highest-leverage integration — every existing OpenAI-SDK app is a candidate migration with a one-line diff.
- **Model-router listing (LiteLLM / OpenRouter).** Because our inference API is OpenAI-compatible, **LiteLLM already works today with zero code on their side** — a user sets `model="openai/<ours>"` with our `base_url`, and LiteLLM's OpenAI-compatible provider path handles it; LiteLLM also documents a lightweight registration route for adding an OpenAI-compatible provider by editing a single provider file.[^litellm] Getting listed as a *first-class* provider in OpenRouter's catalog is a business-development conversation, not a technical one, and gates on our supply being reliable enough to meet their SLA expectations — so we treat OpenRouter listing as a **fast-follow once reliability is proven**, while LiteLLM compatibility is available on day one. *(Provider-catalog acceptance criteria for OpenRouter are not publicly documented as a self-serve flow — flagged as an assumption to verify with BD.)*
- **Hugging Face hub throughout.** `--base`, `--data`, and model references accept HF repo IDs directly (pull); `loom deploy --push-to hf://user/model` and training outputs can push back (push). The [serving](../ml-lifecycle/serving.md) and [training](../ml-lifecycle/training.md) docs own the mechanics; the UX principle is that an HF ID works anywhere a model or dataset is expected.
- **GitHub Action for GPU CI.** The strongest fit with the §3(a) tool-testing story. A published `loom-labs/run-action` lets a workflow do:

  ```yaml
  - uses: loom-labs/run-action@v1
    with:
      gpu: rtx4090,rtx5090,rx9070xt
      run: pytest tests/gpu/
  ```

  This is a genuine complement to, not a clone of, existing GPU-runner options: GitHub's own first-party GPU runners are a single datacenter tier (a Tesla T4 larger-runner, org/enterprise plans), and beyond that teams either wire up their own self-hosted runners or pay for services like RunsOn (which rents cloud NVIDIA/AMD instances such as T4/A10G/L4/L40S/A100/H100 as Actions runners).[^ghgpu] All of those give you *datacenter* GPUs; Loom gives you the *consumer* long tail (RTX 5090, RX 9070 XT) that CI genuinely cannot get elsewhere, billed per-second, with no runner to maintain.
- **VS Code remote into interactive sessions.** Feasible and honestly bounded. VS Code Remote-SSH needs an SSH endpoint; `loom ssh` is a relay-brokered stream, not a listening `sshd` on a public host. The realistic path is a thin local SSH `ProxyCommand` that shells out to `loom ssh <job>` as the transport, letting VS Code attach over the relay tunnel. This is plausible but unproven at design time — Remote-SSH is fussy about its transport, and the server-side VS Code component must install into the sandbox image. **Flagged: needs a spike; we ship `loom notebook`/`loom port-forward` first and treat VS Code Remote as best-effort v1.1.**

---

## 8. Docs & templates strategy

Copy-paste is the fastest teacher. **Every recipe ships with a runnable quickstart** — `loom train --recipe qlora-sft --help` prints a complete, real command you can paste and run, and each recipe has a matching page with the same block. `loom init [--template qlora-sft|inference|gpu-ci]` scaffolds a project directory (a `loom.toml`, a sample dataset or test, a ready `loom run`/`train` invocation, and for the CI template a working GitHub Actions workflow) so a new user's first act is *editing a working thing*, not authoring from a blank file. We maintain example repos (`loom-labs/examples`) covering each of the §3 narratives end-to-end, and every doc command block is CI-tested against the real CLI so the docs can't rot.

---

## 9. Support matrix (v1) & non-goals

**Supported at launch:**

| Side | Dimension | v1 floor |
|---|---|---|
| **Host** | OS | Linux (Ubuntu 22.04+/Debian 12+/Fedora 39+, x86-64) |
| **Host** | GPU | NVIDIA, ≥ 8 GB VRAM (Tier B); headless + IOMMU for Tier A |
| **Host** | Driver | ≥ 550 (CUDA 12.x class) |
| **Host** | Uplink | ≥ 50 Mbps sustained upload |
| **Renter** | CLI OS | Linux (x86-64/arm64), macOS (Apple Silicon + Intel); Windows via WSL2 |
| **Renter** | Inference | any OpenAI-SDK-capable language/runtime — no OS constraint |

**Explicit non-goals at launch** (stated so nobody's surprised):

- **Windows hosts.** Linux + NVIDIA only for hosting at GA; Windows hosting is "much later," gated on isolation-tier work.
- **macOS GPU hosting.** No Metal/Apple-Silicon hosting. macOS is a *renter CLI* platform only.
- **Multi-node / distributed training.** Single-GPU and single-node multi-GPU only in v1; multi-node FSDP across rented machines behind residential NAT is a research problem we're deferring (see [training.md](../ml-lifecycle/training.md) open questions).
- **ROCm hosting is fast-follow, not GA.** AMD renters can *target* ROCm images when supply exists, but broad AMD host onboarding trails NVIDIA.

---

## 10. Open questions

1. **Installer trust vs. friction.** Do we make signature verification mandatory-by-default in the installer (fail-closed if `minisign`/`cosign` isn't present) or best-effort with a loud warning? Fail-closed is safer but adds a dependency that can blow the 10-minute budget on minimal distros.
2. **Bandwidth floor as hard gate vs. soft down-rank.** §2 currently soft-caps low-bandwidth hosts rather than rejecting them. Does letting marginal uplinks list dilute renter trust more than the added supply is worth?
3. **Cost-estimate accuracy SLA.** We show an estimate before every job. How far can actuals drift from the estimate before it erodes trust — and do we cap the renter's liability at, say, estimate × 1.25 and eat overruns?
4. **`loom run --repo .` sync semantics.** Full working-tree sync is simplest to explain but leaks build artifacts and secrets. Do we default to `.gitignore`-aware sync, and how do we surface what got uploaded before it's uploaded?
5. **OpenRouter provider listing.** Is first-class OpenRouter catalog inclusion actually reachable for a marketplace whose supply reliability is inherently noisier than a datacenter provider's, and what completion-rate bar do we need to hit first? *(BD-dependent; unverified — see §7.)*
6. **VS Code Remote feasibility.** Does the `ProxyCommand`-over-`loom ssh` path actually satisfy Remote-SSH, or do we need a purpose-built VS Code extension? Needs a spike before we promise it (§7).
7. **Interactive-session eviction UX.** A `loom notebook` on a host that reclaims its GPU (eject) is a worse experience than a resumable batch job — there's live in-memory state. What do we promise: warn-and-checkpoint, migrate, or just "your kernel died, here's your saved notebook"?

---

[^litellm]: LiteLLM routes to any OpenAI-compatible endpoint via its OpenAI-compatible provider path, and documents adding a provider by editing a single provider registration file. See LiteLLM docs — [Providers](https://docs.litellm.ai/docs/providers), [OpenRouter](https://docs.litellm.ai/docs/providers/openrouter), and [Integrate as a Model Provider](https://docs.litellm.ai/docs/provider_registration/). OpenRouter is described as a proxy over 400+ models / 60+ providers ([TrueFoundry comparison](https://www.truefoundry.com/blog/litellm-vs-openrouter)); its self-serve provider-listing criteria are not publicly documented (unverified).

[^ghgpu]: GitHub's only first-party GPU-hosted runner is a single datacenter tier — a Tesla T4 "larger runner" (`gpu-t4-4-core`), GA since 2024-07-08 and available on larger-runner/organization plans ([GitHub changelog](https://github.blog/changelog/2024-07-08-github-actions-gpu-hosted-runners-are-now-generally-available/), [larger-runners reference](https://docs.github.com/en/actions/reference/runners/larger-runners)). For anything beyond a T4, GPU CI requires self-hosted runners or third-party runner services. RunsOn documents GPU runner instances spanning NVIDIA (T4, A10G, L4, L40S, V100, A100, H100, H200) and AMD (Radeon Pro V520): [RunsOn GPU runners](https://runs-on.com/runners/gpu/). General self-hosted GPU runner guidance: [devactivity](https://devactivity.com/insights/testing-gpu-code-on-github-actions-overcoming-performance-hurdles-with-self-hosted-runners/), [GitHub Docs](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/use-in-a-workflow). All such options offer *datacenter* GPUs, not the consumer long tail.
