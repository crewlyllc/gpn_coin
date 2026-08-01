# GPN Tokenomics Specification — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches the live `pallet-gpn-token` implementation (Cycle 32 audit)
**Date:** 2026-08-01
**Author:** PM07

> This spec is the **source-of-truth** document for GPN tokenomics as *implemented*.
> It supersedes the "Preliminary" tokenomics table in
> `shared/artifacts/project7/04-design-decisions-synthesis.md`, which described a
> *target* design (1B supply, 65/20/10/5 initial split) that was **not** what got built.
> Where this doc and the code disagree, **the code wins** — this doc tracks the code.

---

## 1. Token Identity

| Field | Value |
|-------|-------|
| Ticker | **GPN** (GPU-Native Network) |
| Decimals | **12** (raw balance = amount × 10¹²) |
| Base unit | 1 GPN = 1_000_000_000_000 raw |
| Genesis initial inflation | **10%** |
| Terminal inflation | **1%** |
| Initial block reward | **50 GPN/block** |
| Launch model | **Fair launch via mining** — no premine, no ICO, no dev allocation |

The prototype runs on `gpn-node --dev` with a dev funding faucet (accounts seeded with
1,000,000,000,000 GPN for testing). On mainnet the genesis supply should be 0 and all GPN
issued via `mint_block_reward` through GPU PoW.

---

## 2. Supply & Emission Model

### 2.1 Emission source
- **All** GPN is minted by the `gpn-token` pallet via `mint_block_reward` (root-only,
  called by the `gpu-pow` consensus pallet when a block is sealed).
- `TotalMinted` storage tracks cumulative issuance (enforces `SupplyCapExceeded` guard).
- No genesis supply (fair launch).

### 2.2 Inflation schedule (implemented)
- `InitialInflation = 10%`, `TerminalInflation = 1%`.
- `on_initialize` checks every `BlocksPerYear` blocks; when due, `update_inflation()`
  reduces the rate by **0.5% per year** (`Perbill::from_parts(5_000_000)`) down to a floor
  of the 1% terminal rate.
- Disinflationary: emission rate monotonically declines toward the 1% floor.

### 2.3 Block reward / emission rate
- `InitialBlockReward = 50 GPN` per block at genesis inflation.
- Effective per-block issuance scales with the current inflation rate relative to genesis
  (the reward is the inflation-driven mint, not a fixed halving schedule — no hardcoded
  halving epochs in the token pallet; the decay is the 0.5%/yr inflation step).
- With 15s block time: ~5,760 blocks/day → ~288,000 GPN/day at launch (matches the
  preliminary design's headline number).

---

## 3. Fee Distribution (Implemented)

Marketplace / transaction fees are funneled through `distribute_fees(total_fee)` and split
by a configurable `FeeDistribution` (default, **must sum to 500 bps = 5%**):

| Destination | Basis points | Share of 5% fee |
|-------------|-------------|-----------------|
| Staking reward pool | 200 | 40% |
| Treasury | 150 | 30% |
| Burn | 100 | 20% |
| Oracle operators | 50 | 10% |

- `is_valid()` enforces the four fields sum to **exactly 500 bps**.
- `set_fee_distribution` (root-only) can rebalance; invalid configs rejected.
- `StakingRewardPool` / `OracleRewardPool` accumulate; `distribute_staking_rewards`
  drains the staking pool (paid out to stakers).
- Treasury share transfers to the configured `TreasuryAccount`; burn share is accounted
  (removed from circulation) in the event.

---

## 4. Stake Parameters (Attestation / Marketplace)

From `gpu-attestation` + `compute-marketplace` pallets:

| Parameter | Value | Notes |
|-----------|-------|-------|
| `MinAttestationStake` | **1,000 GPN** | Required before a provider can attest |
| `ProviderStake` | enforced at attest | Agent self-deposits before attesting |
| `MarketplaceFeeBps` | **500 bps = 5%** | Charged on job payout (provider receives price − fee) |

> **Units note (Cycle 31 fix):** compute revenue is `gpu_price(USD/hr) × utilization × 0.85`
> — GPU-hour price is already USD; do **not** multiply by token price again (that bug
> deflated compute revenue ~13× and hid the chain-feed path).

---

## 5. Incentive Flow (How the three modes earn)

```
GPU node (gpn-agent)
   │
   ├─ MINING  → seal Ethash PoW block → gpu-pow mints block reward (50 GPN) → miner
   │
   ├─ STAKING → deposit ≥1,000 GPN (attestation stake) → attest GPU (GFLOPS proof)
   │            → staking weight × GPU tier multiplier → paid from staking pool
   │
   └─ COMPUTE → register provider → accept marketplace job (min_gflops gate)
                → run real GPU GEMM → submit result → complete → escrow payout
                (provider gets price − 5% fee; fee split per Section 3)
```

The `profitability-oracle` feed supplies token price / GPU-hour price / utilization / APR;
the agent's `ProfitabilityCalculator` converts all three modes to USD/hr and
`decide_switch()` (with 5% hysteresis) picks the highest — verified live (Cycle 31) with a
real on-chain feed driving Mining→Compute and serving a paid job.

---

## 6. Tokenomics vs. Preliminary Design — Reconciliation

| Parameter | Preliminary (synthesis) | Implemented (this spec) | Note |
|-----------|-------------------------|--------------------------|------|
| Total supply | 1,000,000,000 | **0 genesis; fair-launch mint** | No premine |
| Mining alloc | 65% of initial | 100% of issuance via PoW | Cleaner |
| Staking rewards | 20% | Fee pool (40% of 5% fee) + attestation | Fee-driven |
| Treasury/dev | 10% vested | 30% of 5% fee | Fee-driven |
| Community | 5% | — | Dropped for fair launch |
| Premine/ICO | 0% | 0% | ✅ Match |
| Emission | halve /2yr | **−0.5%/yr inflation step** | Different mechanism |
| Block reward | 50 GPN | 50 GPN | ✅ Match |
| Block time | 15s | 15s | ✅ Match |

**Decision:** the fee-driven + inflation-decay model (implemented) is preferred over the
fixed-allocation model (paper). It is simpler, fair-launch clean, and self-sustaining via
marketplace fees rather than pre-minted allocations.

---

## 7. Open Questions (for Phase 4 / mainnet prep)

1. **Bonding curve / supply cap:** `SupplyCapExceeded` exists in errors but no hard cap is
   configured — decide whether GPN is capped (e.g., 28.7B like Kaspa, or 1B) or uncapped
   disinflationary.
2. **Burn mechanics:** burn share (20% of fees) is accounted in the event but not yet
   actually removed from issuance — wire an explicit burn (transfer to a dead account /
   `T::Currency::slash`).
3. **Staking reward distribution:** `distribute_staking_rewards` currently just drains the
   pool and emits an event — needs a real per-staker payout keyed by GPU tier multiplier.
4. **Oracle operator rewards:** `OracleRewardPool` accumulates but no distribution call
   exists yet — add one.
5. **Dev faucet removal:** mainnet genesis must not seed 1T GPN test balances.

---

## 8. Related Docs

- Implementation: `gpn-node/pallets/gpn-token/src/lib.rs`
- Consensus: `gpn-node/pallets/gpu-pow/src/lib.rs`
- Attestation: `gpn-node/pallets/gpu-attestation/src/lib.rs`
- Marketplace: `gpn-node/pallets/compute-marketplace/src/lib.rs`
- Oracle: `gpn-node/pallets/profitability-oracle/src/lib.rs`
- Design synthesis: `shared/artifacts/project7/04-design-decisions-synthesis.md`
