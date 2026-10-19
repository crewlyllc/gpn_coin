# GPN — Technical Overview

*A short, site-friendly technical summary. See `GPN-WHITEPAPER.md` for the full paper.*

## The model
A GPN node is **one entity** — one GPU, one identity/wallet — that is **always both a miner and an AI
provider**. It mines whenever the GPU is free; when a job arrives it **pauses mining, frees VRAM, runs the
job to completion, then resumes mining**. There is no provider-only or mine-only mode, and no profitability
switch. *(A separate **Staking** role is planned.)*

> **"One GPU, one job" note:** "one job at a time" is **mutual exclusion** — the GPU never runs two
> jobs concurrently — and is **NOT** a restriction on job *types*. Every node is equipped by design to
> serve **every** job type (LLM · image · video · ML inference · fine-tune) and advertises all of them.

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
- **Cost-derived pricing:** a roofline of the provider's true throughput × margin, clamped ≥ cost.
- **Routing:** capability filter → require idle → **lowest-latency (least-ms)** → dispatch the assignment. Providers **register + advertise endpoints on-chain** for discovery; models can be pulled on demand.
- **Off-chain escrow:** **payment channels** lock funds once and settle each job off-chain → **no per-job block delay**; either side can force-settle on-chain.
- **Safety:** permissionless expiry, per-job failure refunds, a bounded on-chain expiry sweep, and a per-requestor cap.

## Tokenomics
Fair launch via mining; **halving every ~2 years**; 50 GPN/block @ 15 s; **effective max supply ≈ 520 M**; vested 100 M treasury (13-of-25); 5% fee split (40% staking / 30% treasury / 20% burn / 10% oracle).

## Privacy
A **shielded pool** (Sapling-style commitments + nullifiers): **deposit** hides a note; **spend** proves ownership in zero knowledge, publishing only an unlinkable nullifier; supports shielded↔shielded and shielded→transparent with value conservation. Production security relies on a **multi-party trusted-setup ceremony** (trapdoor destroyed by independent contributors).

## Governance & upgrades
Committee-root governance after a timelock (no single key acts alone); **immutable genesis**; runtime upgrades **forklessly** via `set_code`; review-first.

## Network
Permissionless, redundant **access points**; **Bitcoin-style peer discovery** (persistent peer store + DNS/HTTP seeds + on-chain registry + a compiled-in bootnode fallback).

---
*Status:* live on a public testnet (chain, marketplace, channels, shielded spends, discovery, governance). *Next:* public CRS ceremony, mainnet, bridge.
