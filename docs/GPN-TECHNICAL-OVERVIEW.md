# GPN — Technical Overview

*Last updated: 2026-10-22 · See `GPN-WHITEPAPER.md` for the full paper; `GPN-MASTER-REFERENCE.md` for the single-doc reference.*

## The model
A GPN node is **one entity** — one GPU, one identity/wallet — in **one of three modes at a time**:
**Mining** (hash blocks → minted GPN), **Compute** (serve AI jobs → escrow GPN), or *(later)* **Staking**.
It is designed to **auto-switch** on a USD/hr comparison of the modes (~5% hysteresis).

**What actually runs today:** a *yield*, not economic switching — the node mines by default, **fully stops** mining when it accepts an AI job (`~/.gpn/gpu-yield`), and resumes when the job completes. The oracle-driven `ModeManager` switch is designed but **not wired** to the node's internal miner.

**The chain is thin:** it coordinates jobs, holds escrow, enforces eligibility/capability gates, and settles rewards. **Compute runs off-chain.**

## Consensus
- **Ethash-derived GPU proof-of-work**; anyone can mine; no validator set.
- **15-second blocks**; self-tuning difficulty (millisecond resolution, runaway-proof).
- **Bitcoin-style rewards:** the block producer keeps the block reward (account bound via pre-runtime digest).
- **Per-node wallets:** each miner pays out to its own account.
- **Fork tolerance:** explicit network identity (chain-id + genesis hash + version); divergent chains warned + starved, not banned. No finality gadget → confirmation is probabilistic by depth.

## Eligibility (GPU-only)
- **Proof-of-Staked-GPU** attestation (GPU count + measured GFLOPS) → **tiers** (Bronze/Silver/Gold) → **reward multiplier** (staking raises your reward, optionally).
- **Stake-free path:** producing a **seal-verified block** *is* the proof of GPU work; the reward goes to the producer after a small number of authored blocks.
- **Capability gates:** `min_gflops`, `min_vram_mb`, `min_ram_mb` — the router won't send a job to a device that can't meet it.
- **Capacity challenge:** random compute challenges make throughput *proven*, not self-reported.
- **Liveness:** always-on, publicly-reachable full nodes earn credit and gate top tiers.

## AI marketplace
- **Job lifecycle:** Pending → Accepted → Running → AwaitingVerification → Completed (+ Cancelled/Failed).
- **Per-token escrow:** cap = rate × max tokens; charge = rate × actual; **surplus auto-refunds**; dollar-stable, GPN-settled.
- **Cost-derived pricing:** a roofline of the provider's true throughput × margin, clamped between the owner's cost and the market GPU-hour rate.
- **Routing:** capability filter → require idle → **lowest-latency (least-ms)** → lease. Providers **register + advertise endpoints on-chain**; transport is **pull** (`GET /assignments` long-poll + lease, results pushed to `POST /result/{id}`), so the access point never needs to dial a provider.
- **Off-chain escrow:** **payment channels** lock funds once and settle each job off-chain → **no per-job block delay**; either side can force-settle on-chain.
- **Safety:** permissionless expiry, per-job failure refunds, a bounded on-chain expiry sweep, and a per-requestor cap.

## Public AI API (LIVE)
- **`https://gpncoin.ai/api/v1/*`** — an OpenAI-compatible surface (chat completions, model list, providers, job lookup) over **HTTPS** via a **Cloudflare Worker** that proxies to the access point on a raw socket (the AP speaks plain HTTP on :8092, which a browser page cannot call). Bearer-token auth is injected by the Worker.
- **Rate-limited** at the edge: chat **12 requests / 10 min / IP**, reads 120/min, with a typed `gpn_rate_limited` error.
- **Testnet inference is free.** Latency depends on the payment mode: `channel` (the default) settles against an off-chain escrow channel and measures **p50 ≈ 70 ms** AP-local; `settle` pays the per-request on-chain cycle and is dominated by the **create-inclusion wait** (p50 ≈ 38 s, p95 ≈ 90 s at the current cadence), not by inference (~1 s). See `docs/offchain-escrow-channels.md`.

## Model catalogue (LIVE, VRAM-gated)
Three independent gates, one shared implementation (`gpn_agent::models`):
1. **VRAM fit** — a weight-size table with a 1.15× headroom factor (KV cache + runtime). A model that cannot fit is never advertised (`/status models[]`), never claimed, and never routed. Example: `deepseek-r1:14b` needs ~10.3 GB and is dropped on an 8 GB card.
2. **Public allowlist** — `/v1/models` lists assistant-grade chat models only (16 families, prefix-matched so quant tags work). Hard-excluded even with `GPN_PUBLIC_MODELS=all`: `tuned-*` fine-tune artifacts, sub-0.3B toys (`stories260K`), embedding/reranker models.
3. **Usable ≠ advertised** — a named model outside the catalogue is still served; the catalogue is a shop window, not a permission system.

On the current 8 GB node this yields four served models: `llama3.2:1b`, `qwen2.5:0.5b`, `qwen2.5:7b`, `deepseek-r1:7b`. Overrides: `GPN_PUBLIC_MODELS`, `GPN_MODEL_VRAM_HEADROOM`, `GPN_MODEL_VRAM_OVERRIDE`.

## Economics (derived, with honest limits)
GPN does not price tokens off a competitor's price list; it prices them off the **GPU-hour rental market**:

```
market  = oracle avg_gpu_hour_price         # USD/hr per GPU class
target  = clamp(0.50 × market, owner_cost, market)
$/token = target / (tok_s × 3600 × 0.65) × 1.30
gpn/tok = $/token ÷ token_price_usd
```

`0.65` is the assumed card duty cycle, `1.30` the margin. Throughput is a roofline: decode on a 1B model is **memory-bandwidth-bound** (~95 tok/s on a 30 TFLOP / 272 GB/s card), not FLOP-bound.

| GPU-hour **market** clears at | 1B model, $/1M output tokens |
|---|---|
| **$0.30** (idle consumer hardware) | **≈ $0.88** — below hosted small models |
| **$1.50** | ≈ $4.38 |
| **$3.00** (today's feed fallback) | **≈ $8.75** — ~22× `gpt-5-nano`'s $0.40 |

Break-even against `gpt-5-nano` is **≈ $0.137/GPU-hr**. Two real gaps: the **on-chain price oracle feed is absent** (the scheduler runs on hardcoded `$0.15/GPN`, `$3.00/GPU-hr`, `0.65`), and **input tokens are not metered** — only output is billed, so long prompts are underpriced. GPN is therefore a **GPU-rental market whose token price is downstream of the GPU-hour clearing price** — not a blanket "cheaper than OpenAI".

## Node operations: dashboard, Idle, updates
- **Local dashboard** — `gpn_node --gui` / `GPN_GUI=1` serves `http://127.0.0.1:8099` from the supervisor: mode, peers, height, GPU/VRAM, current job, child processes, and the idle panel. **Loopback only, by design** (it can stop the miner); any non-`127.0.0.1` bind is refused at startup. No auth because there is no network exposure.
- **Idle mode** — pause mining **and** AI job claims for up to **6 h** while the node keeps syncing, peering and serving the chain. Auto-resume at the deadline (survives restart/crash via `~/.gpn/idle.json`), manual resume (`gpn_node resume`, `POST /api/resume`), and a running job always finishes. A safety valve **refuses (409)** to idle a node with **zero peers**, since that node may be the only block producer (`--force-idle` overrides for a dev chain).
- **Auto-update — ON by default (opt out).** `https://gpncoin.ai/releases.json` is an **ed25519-signed** manifest (the signature covers a canonical text blob, not the JSON; the public key is compiled into the node). Installs are **bundle-aware**: versioned trees under `<base>/update/versions/<v>/`, an atomic `current` symlink/junction flip, `current.previous` for one-step rollback, and a **build-stamp identity check** (not just a hash) so a mis-uploaded bundle is refused. The schedule is per-node randomized — a stable phase over the interval plus ±5–10% jitter — and downloads are **staggered** (`GPN_UPDATE_SPREAD_SECS`, default 1200 s) so the fleet does not stampede the release host. Opt out with `GPN_AUTO_UPDATE=0` / `auto_update = false`. A node below `min_version` gets **`UPGRADE REQUIRED`**: it keeps syncing but stops mining and announcing.

## Provider address publication
Publishing a dialable address is best-effort, never a gate on joining. The heartbeat normalizes the node RPC's listen addresses (current litep2p builds emit a **doubled** `/p2p/<id>/p2p/<id>`; identity is stripped and `/p2p/<peer_id>` appended exactly once, shared with the AP's gate). It then walks an evidence ladder: `GPN_PUBLIC_ADDR` → the node-written `~/.gpn/dialability.json` → a public listen address → a **STUN mapping on the listen port**. Loopback, RFC1918, link-local, CGNAT and ULA ranges are never published; when every rung fails, `multiaddrs` is `[]` **and the log says which rungs were rejected and how to fix it**. Empty publication is not an error — the pull transport still works.

## Tokenomics
Fair launch via mining; **halving every ~2 years**; 50 GPN/block @ 15 s; **hard supply cap ≈ 520.5 M GPN** (≈ 420.48 M emitted + 100 M treasury); the 100 M treasury is held by a keyless pallet account (70 M genesis-unlocked, 30 M vested over 10 yr) and spendable only by a 13-of-25 governance roster; 5% fee split (40% staking / 30% treasury / 20% burn / 10% oracle).

## Privacy
A **shielded pool** (Sapling-style commitments + nullifiers): **deposit** hides a note; **spend** proves ownership in zero knowledge, publishing only an unlinkable nullifier; supports shielded↔shielded and shielded→transparent with value conservation. Production security relies on a **multi-party trusted-setup ceremony** (trapdoor destroyed by independent contributors).

## Governance & upgrades
- **LIVE on chain (spec 41):** committee-root governance after a timelock — **3 members, 2-of-3 approvals, 3-day timelock**, no cancel, no expiry, no preimage proposals, **zero weights** (`WeightInfo = ()`). The mainnet sudo key is inert; genesis is immutable; runtime upgrades go forklessly via `set_code`.
- **STAGED, NOT APPLIED (spec 42 artifact, built + tested):** committee **5 members, 3-of-5** approvals, `set_members`, **14-day proposal expiry** + pruning, **`cancel()`** at the same threshold, **preimage proposals** (approve a code hash instead of a 512 KB inline WASM), hand-set weights, and a v0→v1 storage migration. 512,888 bytes, sha256 `7694f7f6…d30f1`; canonical genesis WASM untouched; no `set_code` submitted. See `staged/gov-hardening-v1.md`.
- The differences above are the whole point: **do not read the staged design as the live chain.**

## Network
Permissionless, redundant **access points**; **Bitcoin-style peer discovery** (persistent peer store + DNS/HTTP seeds + on-chain registry + a compiled-in bootnode fallback).

## Capability truth table

| Capability | Status | Detail |
|---|---|---|
| **Text / LLM inference** | **LIVE** | Public HTTPS `/v1/chat/completions`, OpenAI-compatible, free on testnet |
| **Image generation** | **WORKS INTERNALLY** | Completes via on-chain `POST /jobs` (`job_type=ImageGeneration`, sd-turbo), artifact hash verified historically; **no public `/v1` route** |
| **Video generation (LTX)** | **BLOCKED** | Implemented, never completed: the T5-XXL encoder exceeds 8 GB VRAM |
| **Fine-tune / ML providers** | **WORKS INTERNALLY** | Shipped and claimable on-chain, not wired to the public API |
| **On-chain GPU-hour price oracle** | **GAP** | Feed absent; scheduler uses hardcoded fallbacks |
| **Input-token metering** | **GAP** | Only output tokens are billed |
| **Governance hardening (spec 42)** | **STAGED** | Not applied; chain is still 3-member / 2-of-3 / no expiry / zero weights |

---
*Status (2026-10-22):* live on a public testnet — chain, LLM marketplace, payment channels, shielded spends, on-chain discovery, public HTTPS API, node GUI + Idle, signed auto-update. *Next:* governance hardening apply, public CRS ceremony, mainnet, bridge.
