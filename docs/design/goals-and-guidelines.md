# GPN — Goals & Guidelines

_Living document. Add to it as questions come up. Last updated 2026-10-02._

## What GPN is
A GPU-native network where each GPU either **mines** (PoW) or **serves AI compute**,
auto-switching between them. The chain coordinates, escrows payment, and settles
rewards; the heavy compute runs **off-chain**.

## Goals
1. **Turn idle GPUs into income.** Any GPU owner can earn by mining or serving AI,
   without a big setup.
2. **Cheap, accessible AI compute.** Buyers get AI over one simple interface
   (REST / OpenAI-compatible), paying per token, with the price tied to real cost.
3. **Trustless coordination.** The chain holds escrow, enforces capability gates,
   and settles rewards; no party can run off with funds.
4. **Privacy by default.** Questions and answers are **never** recorded on-chain —
   only opaque/salted commitments.
5. **Decentralization.** Committee-based governance so no single key can act alone.
6. **Sustainability for providers.** Price covers compute cost + margin; a GPU
   always earns at least its cost.
7. **A usable, fork-tolerant network.** Mainnet is unambiguously identifiable
   (genesis + chain-id); forks are tolerated but starved of value.

## Guidelines
### Architecture
- **Chain = coordination + escrow. Compute = off-chain.** Never move AI compute on-chain.
- **One GPU does one job at a time.** An AI job yields mining (the network as a whole
  keeps mining); a machine never mines *and* infers at once.
- **1 GPU = 1 reward** — rewards gated on hardware attestation (anti-spoof).
- **Privacy**: Q&A stay off-chain; on-chain values are salted/opaque (verifiable by
  the holder, meaningless to observers).

### Money & economics
- **Per-token pricing + per-job escrow.** Escrow = rate × max_output_tokens (a cap);
  the actual charge = rate × tokens used; the surplus auto-refunds.
- **Priced in dollars, settled in GPN.** `gpn_per_token = usd_per_token / token_price_usd`
  so the *dollar* cost is stable while the GPN amount floats.
- **Cost-derived rate** = `min(TFLOPs·eff/(2·params), BW·eff/(2·bytes))` (the roofline),
  ÷ `(tok/s × 3600 × utilization)` × (1+margin).
- **Policy**: target ~**50% of the market GPU-rental** (clamped ≥ the owner's cost).
- **The reward and the block time are coupled**: `emission_rate = reward / target_block_time`.
  Change one, adjust the other. (Standing: 12 GPN per 4s = 3 GPN/s.)
- **Token**: genesis supply fixed; the halving is the sole emission; treasury funded
  by genesis unlock; predictable, non-inflationary-by-surprise.

### Governance & safety
- **Genesis is IMMUTABLE.** Never change the genesis hash, genesis WASM, chain-spec
  difficulty, or supply. Runtime changes go via **forkless `set_code`** only.
- **Committee consensus**: the 2-of-3 committee proposes/executes Root calls after a
  timelock (no single key acts alone). Rewards, fees, and runtime params are
  adjustable this way — **no genesis change**.
- **Review before change**: every change is logged (WHAT/WHY/RISK/ROLLBACK) and
  reviewed by the main agent against these guidelines; escalate to Jeremy only for
  genuinely undecidable, high-blast-radius issues.
- **Fork tolerance**: never ban isolated mining (breaks dev/private nets). Rely on
  genesis+chain-id identity, a version handshake, loud warnings, a canonical registry,
  and market value to separate the canonical chain from forks.

### Operations
- **Everything runs as services** (systemd user units), auto-restart, survive reboot.
- **Testable increments**: build clean, test after every change, revert on failure.
- **Small, reversible changes** over big rewrites.

## Open questions / to decide
- Mainnet genesis: final difficulty + reward + supply, chosen together.
- Mainnet oracle: bonded operators + a real GPN market for `token_price_usd`.
- Fee split (provider / access point / protocol) exact bps.
- Image/video + fine-tuning model catalog + pricing tiers.
