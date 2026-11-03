# GPN — Technical Overview

*Last updated: 2026-11-02 · See `GPN-WHITEPAPER.md` for the full paper; `GPN-MASTER-REFERENCE.md` for the single-doc reference. Verified against chain spec 45 / release 0.3.11.*

## The model
A GPN node is **one entity** — one GPU, one identity/wallet — and it is **always miner + AI provider**:
it mines by default and serves an AI job when the access point assigns one. *(An optional **Staking**
multiplier exists; it is not a mode and not a prerequisite.)* There is **no profitability switch to
configure**; a USD/hr auto-switch is a deferred roadmap item.

**What actually runs today:** the node mines by default, **fully stops** mining when it accepts an AI job
(`~/.gpn/gpu-yield`), and resumes when the job completes. The oracle-driven `ModeManager` switch is
designed but **not wired** to the node's internal miner.

**The chain is thin:** it coordinates jobs, holds escrow, enforces eligibility/capability gates, and settles rewards. **Compute runs off-chain.**

## Consensus
- **Ethash-derived GPU proof-of-work**; anyone can mine; no validator set.
- **15-second blocks**; self-tuning difficulty (millisecond resolution, runaway-proof).
- **Bitcoin-style rewards:** the block producer keeps the block reward (account bound via pre-runtime digest).
- **Per-node wallets:** each miner pays out to its own account.
- **Fork tolerance:** explicit network identity (chain-id + genesis hash + version); divergent chains warned + starved, not banned. No finality gadget → confirmation is probabilistic by depth.

## Eligibility (GPU-only)
- **Proof-of-Staked-GPU** attestation (GPU count + measured GFLOPS) → **tiers** (Bronze/Silver/Gold) → optional **reward multiplier**. **There is no staking or delegation prerequisite to attest, list, or serve.**
- **Eligibility, verified by work:** producing a **seal-verified block** *is* the proof of GPU work; the reward goes to the producer after a small number of authored blocks.
- **Capability gates:** `min_gflops`, `min_vram_mb`, `min_ram_mb` — the router won't send a job to a device that can't meet it.
- **Capacity challenge:** random compute challenges make throughput *proven*, not self-reported.
- **Liveness:** always-on, publicly-reachable full nodes earn credit and gate top tiers.
- **Not verified: output.** Nothing checks that a returned completion is correct, spot-checked, or redundant. See the truth table.

## AI marketplace
- **Job lifecycle:** Pending → Accepted → Running → AwaitingVerification → Completed (+ Cancelled/Failed). `AwaitingVerification` records submission of the result hash; it does **not** run a correctness check.
- **Per-token escrow:** cap = rate × max tokens; charge = rate × actual; **surplus auto-refunds**; dollar-stable, GPN-settled.
- **Throughput-anchored pricing:** an anchor constant (what a 4060-class card earns per hour) divided by the token rate that card reaches, plus a margin, converted at the token price.
- **Routing:** capability filter → require idle → **lowest-latency (least-ms)** → lease. Providers **register + advertise endpoints on-chain**; transport is **pull** (`GET /assignments` long-poll + lease, results pushed to `POST /result/{id}`), so the access point never needs to dial a provider.
- **Off-chain escrow:** **payment channels** lock funds once and settle each job off-chain → **no per-job block delay**; either side can force-settle on-chain.
- **Safety:** permissionless expiry, per-job failure refunds, a bounded on-chain expiry sweep, and a per-requestor cap.

## Public AI API (LIVE)
- **`https://gpncoin.ai/api/v1/*`** — an OpenAI-compatible surface over **HTTPS** via a **Cloudflare Worker** that proxies to the access point on a raw socket (the AP speaks plain HTTP on :8092, which a browser page cannot call). Bearer-token auth is injected by the Worker.
- **Implemented today (exact public paths):** under `/api/v1` — `GET /models`, `POST /chat/completions`, `POST /images/generations`, `GET /images/generations/{id}` (32-hex job id), `GET /images/{id}` (PNG), `POST /finetunes`, `GET /finetunes/{id}`, `GET /finetunes/{id}/artifact`. Outside that prefix — `GET /api/health`, `/api/mode`, `/api/providers` (live provider registry), `/api/jobs/{id}` (on-chain job state) and `/api/network` (node directory). Note the split: provider and job lookup are served at **`/api/providers`** and **`/api/jobs/{id}`**, *not* at `/api/v1/providers` or `/api/v1/jobs/{id}` (those 404). There is **no `/api/pricing`**; the rate card is the access point's internal `GET /pricing` on :8092 and is not proxied publicly.
- **Rate-limited** at the edge: chat **12 requests / 10 min / IP**, reads 120/min, with a typed `gpn_rate_limited` error.
- **Testnet inference is free.** Latency depends on the payment mode: `channel` (the default) settles against an off-chain escrow channel and measures **p50 ≈ 70 ms** AP-local; `settle` pays the per-request on-chain cycle and is dominated by the **create-inclusion wait** (p50 ≈ 38 s, p95 ≈ 90 s at the current cadence), not by inference (~1 s). See `docs/offchain-escrow-channels.md`.

## Model catalogue (LIVE, VRAM-gated)
Three independent gates, one shared implementation (`gpn_agent::models`):
1. **VRAM fit** — a weight-size table with a 1.15× headroom factor (KV cache + runtime). A model that cannot fit is never advertised (`/status models[]`), never claimed, and never routed. Example: `deepseek-r1:14b` needs ~10.3 GB and is dropped on an 8 GB card.
2. **Public allowlist** — `/v1/models` lists assistant-grade chat models only (16 families, prefix-matched so quant tags work). Hard-excluded even with `GPN_PUBLIC_MODELS=all`: `tuned-*` fine-tune artifacts, sub-0.3B toys (`stories260K`), embedding/reranker models.
3. **Usable ≠ advertised** — a named model outside the catalogue is still served; the catalogue is a shop window, not a permission system.

On the current 8 GB node the public catalogue is exactly **three** models: `llama3.2:1b`, `qwen2.5:0.5b`, `qwen2.5:7b`. `GET /v1/models` is authoritative — do not promise a model that is not listed. Overrides: `GPN_PUBLIC_MODELS`, `GPN_MODEL_VRAM_HEADROOM`, `GPN_MODEL_VRAM_OVERRIDE`.

## Economics (derived, with honest limits)
GPN does not price tokens off a competitor's price list, nor off a GPU-rental market. It prices them off
**throughput**: one anchor constant (what a 4060-class card earns per hour) divided by the token rate that
card reaches on a 1B model.

```
anchor_tok_s = roofline(30,000 GFLOP/s, 272 GB/s, 1B) = 95.2 tok/s
anchor_rate  = $0.10/hr                    # GPN_GPU_ANCHOR_USD_HR
k            = anchor_rate / (anchor_tok_s × 3600) = $2.918e-7 / output token
price_out    = k × (1 + GPN_MARGIN_BPS/10000)        # 3000 bps → $0.379/1M
price_in     = price_out × GPN_INPUT_RATE_FRAC       # 0.15   → $0.057/1M
provider_hr  = roofline(gflops, bw, params_b) × 3600 × k
```

**The buyer price is GPU-independent by design**: the same $0.379/1M output is charged whichever card
serves the job, so a spot market of heterogeneous hardware can exist at all. Faster hardware earns more
per hour instead of costing the buyer more. Throughput (not FP32 GFLOPs) is the metric because decode of
a small model is memory-bandwidth-bound — a FLOP-based price would rank a 4090 above an H100.

| GPU (1B stream) | Memory bandwidth | Throughput | Provider earns |
|---|---|---|---|
| RTX 4060 8GB (anchor) | 272 GB/s | 95.2 tok/s | **$0.100/hr** |
| RTX 4090 24GB | 1008 GB/s | 352.8 tok/s | **$0.371/hr** |
| A100 80GB | 2039 GB/s | 713.7 tok/s | **$0.750/hr** |
| H100 80GB | 3350 GB/s | 1172.5 tok/s | **$1.232/hr** |

At $0.379/1M output the 1B tier sits next to `gpt-5-nano`'s $0.40 rather than a multiple of it. The live
access point publishes these numbers at `GET /pricing`; the current idle-hardware feed reports ≈$0.30/GPU-hr.

Guardrails: a derived price outside **[$0.05, $5.00] per 1M** refuses to publish unless
`GPN_PRICING_UNSAFE=1`; the effective anchor and price are logged at startup; a provider whose modelled
earning is below its own declared `owner_cost_usd_hr` is logged as losing money instead of being hidden.

**One limit we do not paper over: input metering is channel-first.** The off-chain channel bills
input at 15% of the output rate and escrows `prompt_tokens/4 + max_tokens`. The on-chain pallet stores a
single `(price_per_token, max_tokens, actual_tokens)`, so an on-chain job publishes a *blended* rate that
is exact when the completion runs to `max_tokens` and slightly under-bills short completions; exact
two-rate settlement on-chain requires a pallet change. The **oracle feed is live** (since 2026-10-22): a
bonded operator posts token `$0.15`, GPU-hour `$0.30`, utilization `65%` and APR `0%`, and the USD→GPN
conversion reads that bond-weighted median; only an unreadable chain falls back to `$0.15/GPN`.

## Node operations: dashboard, Idle, updates
- **Local dashboard** — `gpn_node --gui` / `GPN_GUI=1` serves `http://127.0.0.1:8099` from the supervisor: mode, peers, height, GPU/VRAM, current job, child processes, and the idle panel. **Loopback only, by design** (it can stop the miner); any non-`127.0.0.1` bind is refused at startup. No auth because there is no network exposure.
- **Idle mode** — pause mining **and** AI job claims for up to **6 h** while the node keeps syncing, peering and serving the chain. Auto-resume at the deadline (survives restart/crash via `~/.gpn/idle.json`), manual resume (`gpn_node resume`, `POST /api/resume`), and a running job always finishes. A safety valve **refuses (409)** to idle a node with **zero peers**, since that node may be the only block producer (`--force-idle` overrides for a dev chain).
- **Auto-update — ON by default (opt out).** `https://gpncoin.ai/releases.json` is an **ed25519-signed** manifest (the signature covers a canonical text blob, not the JSON; the public key is compiled into the node). Installs are **bundle-aware**: versioned trees under `<base>/update/versions/<v>/`, an atomic `current` symlink/junction flip, `current.previous` for one-step rollback, and a **build-stamp identity check** (not just a hash) so a mis-uploaded bundle is refused. The schedule is per-node randomized — a stable phase over the interval plus ±5–10% jitter — and downloads are **staggered** (`GPN_UPDATE_SPREAD_SECS`, default 1200 s) so the fleet does not stampede the release host. Opt out with `GPN_AUTO_UPDATE=0` / `auto_update = false`. A node below `min_version` gets **`UPGRADE REQUIRED`**: it keeps syncing but stops mining and announcing.

## Provider address publication
Publishing a dialable address is best-effort, never a gate on joining. The heartbeat normalizes the node RPC's listen addresses (current litep2p builds emit a **doubled** `/p2p/<id>/p2p/<id>`; identity is stripped and `/p2p/<peer_id>` appended exactly once, shared with the AP's gate). It then walks an evidence ladder: `GPN_PUBLIC_ADDR` → the node-written `~/.gpn/dialability.json` → a public listen address → a **STUN mapping on the listen port**. Loopback, RFC1918, link-local, CGNAT and ULA ranges are never published; when every rung fails, `multiaddrs` is `[]` **and the log says which rungs were rejected and how to fix it**. Empty publication is not an error — the pull transport still works, and a NAT'd node serves jobs normally. A node needs **no inbound port** to earn.

## Tokenomics
Fair launch via mining; **halving every ~2 years**; 50 GPN/block @ 15 s; **hard supply cap ≈ 520.5 M GPN** (≈ 420.48 M emitted + 100 M treasury). **At mainnet launch** the 100 M treasury sits in a keyless pallet account (70 M genesis-unlocked, 30 M vested over 10 yr) and is spendable only by a **13-of-25 treasury spend roster**. The testnet genesis mints **0** into that account today. Fee split: 40% staking rewards / 30% treasury / 20% burn / 10% oracle operators.

## Privacy
A **shielded pool** (Sapling-style commitments + nullifiers): **deposit** hides a note; **spend** proves ownership in zero knowledge, publishing only an unlinkable nullifier; supports shielded↔shielded and shielded→transparent with value conservation. Production security relies on a **multi-party trusted-setup ceremony** (trapdoor destroyed by independent contributors).

## Governance & upgrades
- **The design:** a **5-member council with 3-of-5 approvals** governs root calls after a timelock (**3 days on mainnet**). Treasury *spending* is a **different body** — the 13-of-25 treasury spend roster.
- **LIVE on testnet (spec 45; verified 2026-11-02, block 236144):** 5 members, **3 approvals required** (max 8), **10-block (~2 min) timelock** from the `testnet-fast-timelock` build. Governance hardening (spec 42) is **applied**. There is **no cancel/veto extrinsic** and no referendum / fast-track / preimage pallet track — an approved proposal cannot be recalled in the current runtime. The mainnet sudo key is inert; genesis is immutable; runtime upgrades go forklessly via `set_code`.
- **Testnet governance is a bootstrap, not a security guarantee** — it runs under operator control, and the testnet imposes **no member-count requirement**. Present 5/3-of-5 as the design, not as a rule the chain enforces.

## Network
Permissionless, redundant **access points**; **Bitcoin-style peer discovery** (persistent peer store + DNS/HTTP seeds + on-chain registry + a compiled-in bootnode fallback).

## Capability truth table

| Capability | Status | Detail |
|---|---|---|
| **Text / LLM inference** | **LIVE** | Public HTTPS `/v1/chat/completions`, OpenAI-compatible, free on testnet |
| **Image generation** | **LIVE (public)** | `POST /v1/images/generations`; sd-turbo runs on chain-assigned jobs |
| **Video generation (LTX)** | **BLOCKED** | Implemented, never completed: the T5-XXL encoder exceeds 8 GB VRAM; no public route |
| **Fine-tune / ML providers** | **LIVE (public) / internal** | `POST /v1/finetunes` is public; ML inference remains internal |
| **On-chain GPU-hour price oracle** | **LIVE** | Bonded operator posts token $0.15 / GPU-hour $0.30 / util 65% / APR 0% (since 2026-10-22); `gpn_effective_feed` returns the bond-weighted median |
| **Result verification** | **NOT IMPLEMENTED** | No output is verified, spot-checked, or made redundant; settlement follows the submitted result hash. Roadmap. |
| **Governance hardening (spec 42)** | **APPLIED (testnet)** | 5-member council / 3-of-5 / 10-block testnet timelock; no cancel or referendum track; bootstrap, not a security guarantee |

---
*Status (2026-11-02, spec 45 / 0.3.11):* live on a public testnet — chain, LLM/image/fine-tune marketplace, payment channels, shielded spends, on-chain discovery, public HTTPS API, node GUI + Idle, signed auto-update. *Next:* result verification, public CRS ceremony, mainnet (3-day governance timelock), bridge.
