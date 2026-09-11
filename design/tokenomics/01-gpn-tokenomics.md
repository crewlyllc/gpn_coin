# GPN Tokenomics Specification — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches the live `pallet-gpn-token` + `pallet-gpn-treasury` implementation
**Date:** 2026-08-01 · **reconciled 2026-09-11** (disclosed vested treasury, D-T1…D-T6)
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
| Hard supply cap | **1B GPN** (`MaxSupply`) |
| Launch model | **Fair launch via mining + disclosed vested treasury (20%)** — no private premine, no ICO, no liquid dev allocation |
| Treasury allocation | **200,000,000 GPN** (20% of the cap), vested, keyless `gpn/trsy` account, 13-of-25 governance |
| Max *mineable* | **800,000,000 GPN** (1B cap − 200M treasury) |

The prototype runs on `gpn-node --dev` with a dev funding faucet (accounts seeded with
100,000 GPN for testing — well below the cap and above the 1,000 GPN attestation stake).

Mainnet (`--chain=mainnet`) seeds **no user accounts** (fair launch) but **does** carry a single
disclosed genesis allocation: the **200,000,000 GPN vested treasury** (see §9). The remaining
**800M GPN** is issued via `mint_block_reward` through GPU PoW.

---

## 2. Supply & Emission Model

### 2.1 Emission source
- **All** GPN is minted by the `gpn-token` pallet via `mint_block_reward` (root-only,
  called by the `gpu-pow` consensus pallet when a block is sealed).
- `TotalMinted` (canonical, in `gpn-token`) tracks cumulative issuance and enforces the
  **1B GPN** `MaxSupply` cap via the `SupplyManager` trait used by `gpu-pow`.
- Mainnet seeds `initial_minted = treasury_allocation (200M)` so the disclosure counts against
  the cap; all *other* issuance is mined. Effective max mineable = **800M GPN**.

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
| Total supply | 1,000,000,000 | **1B hard cap** | ✅ Match |
| Mining alloc | 65% of initial | 80% of cap (800M), mined | Treasury carve-out disclosed |
| Staking rewards | 20% | Fee pool (40% of 5% fee) + attestation | Fee-driven |
| Treasury | 10% vested | **20% vested (200M), keyless + 13-of-25** | Adopted (D-T1…D-T6, §9) |
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

**Implemented this cycle (Cycle 34):**

1. ✅ **Burn mechanics** — `distribute_fees` now *actually* removes the burn share
   from circulation via `Currency::slash` on the pallet account (emits `Burned`
   event, reduces `total_issuance`). No longer just bookkeeping.
2. ✅ **Staking reward distribution** — `distribute_staking_rewards(stakers)` now
   pays each staker a share of the pool **weighted by their attested GPU tier
   multiplier** (`staking_multiplier_bps`: Bronze 1.0× / Silver 1.5× / Gold 2.5×;
   Unattested excluded). Proof-of-staked-GPU now directly boosts yield.
3. ✅ **Oracle operator rewards** — new `distribute_oracle_rewards(operators)`
   drains `OracleRewardPool` and splits it evenly among the listed operators.

**Still open for mainnet:**

4. ✅ **Supply cap decided (Cycle 35):** GPN is **hard-capped at 1B GPN** (`MaxSupply` in
   the runtime gpu-pow config = `1_000_000_000_000_000_000_000` raw = 1B GPN).
   `distribute_pending_rewards` refuses to mint past the cap, so emission is
   disinflationary AND absolutely bounded — supply can never exceed 1B GPN.
5. ✅ **Dev faucet scoped + mainnet launch (Cycle 35, treasury added later):** the `--dev`/`local`
   chain specs now seed a modest **100,000 GPN** test faucet (was erroneously 1
   trillion GPN, equal to the entire cap). The **`mainnet` chain spec**
   (`--chain=mainnet`) seeds **no user accounts and no oracle operators** — all mined GPN is
   issued via GPU PoW block rewards — plus the **disclosed 200M vested treasury** (§9). Also
   fixed a latent bug: the block reward constant was 50 **million** GPN, not 50 GPN
   (would have exhausted the 1B cap in ~20 blocks); now correctly **50 GPN**
   (matching the spec).

---

## 9. Vested treasury allocation (mainnet) — implemented

The project owner's locked decisions **D-T1…D-T6 (2026-09-10)** moved mainnet from a strict
zero-genesis fair launch to a **disclosed, vested treasury**. This section is the authoritative
description; the code wins.

| # | Decision | Value |
|---|---|---|
| D-T1 | Allocation | **20% of the 1B cap = 200,000,000 GPN** |
| D-T2 | Address | **Keyless `gpn/trsy` pallet account** (`PalletId(*b"gpn/trsy")`); no private key exists |
| D-T3 | Vesting | **Reading A**: Phase 1 = 100M over 48 mo (8 × 12.5M / 6 mo); Phase 2 = 100M over 120 mo (20 × 5M / 6 mo) |
| D-T4 | Counts against 1B cap | **Yes** → `initial_minted = 200M`, max mineable **800M GPN** |
| D-T5 | Governance | **Fixed 25-address roster at genesis; 13-of-25 approvals** to authorize a spend |
| D-T6 | Default | **Vesting is the default on mainnet** (not opt-in) |

### 9.1 Vesting schedule (exact)

Total **200,000,000 GPN**; **0 GPN unlocked at launch**. Unlock blocks are `i × 1,036,800`
blocks (≈ 6 months at 15 s/block).

| Phase | Amount | Cadence | Tranches | Per tranche | First unlock |
|---|---|---|---|---|---|
| 1 | 100,000,000 | every 6 mo | 8 | 12,500,000 | month 6 (block 1,036,800) |
| 2 | 100,000,000 | every 6 mo | 20 | 5,000,000 | month 54 |
| **Total** | **200,000,000** | | **28** | | full unlock ≈ month 168 |

### 9.2 Implementation

- **Genesis:** `mainnet_chain_spec()` passes `gpn(TREASURY_ALLOC_GPN)` = 200M to
  `genesis_config_json`, funding the `gpn/trsy` pallet account; `gpn_token.initial_minted` is
  seeded with the same amount so it counts against `MaxSupply`. `dev`/`local`/`testnet` pass `0`
  (clean fair-launch fork for testing).
- **Pallet:** `pallet-gpn-treasury` stores `Roster` (25), `Threshold` (13), `Schedule`
  (28 tranches), `TotalReleased`, and spend proposals (`ProposalDest/Amount/Approvals/Executed`).
  The treasury can only ever spend its **unlocked** balance.
- **Roster source:** `GPN_TREASURY_ROSTER` env (comma-separated ss58) at spec-build time, else
  placeholder keys. **Mainnet must be launched with the real 25-address roster.**
- **Ops CLI:** `gpn_treasury status|schedule|roster|proposals|propose|approve|execute`.
- **Enforced errors:** `NotInRoster`, `NotEnoughApprovals`, `InsufficientUnlocked`,
  `AlreadyExecuted`, `RosterEmpty`.

### 9.3 Disclosure

The amount, the address, the vesting schedule, and the governance rule are **public and
on-chain verifiable** — the point of a disclosed treasury is that anyone can independently check
it. See `README.md` (Vested treasury section), `design-treasury-vesting.md`, and
`docs/mainnet-launch-runbook.md` §7.

> **Honest note:** a vested treasury is still a genesis allocation, and it *does* depart from the
> earlier "no premine" line. That departure is disclosed here and in the README rather than
> hidden, per D-T1…D-T6.

---

## 10. Related Docs

- Implementation: `gpn-node/pallets/gpn-token/src/lib.rs`
- Treasury: `gpn-node/pallets/gpn-treasury/src/lib.rs` · `design-treasury-vesting.md`
- Consensus: `gpn-node/pallets/gpu-pow/src/lib.rs`
- Attestation: `gpn-node/pallets/gpu-attestation/src/lib.rs`
- Marketplace: `gpn-node/pallets/compute-marketplace/src/lib.rs`
- Oracle: `gpn-node/pallets/profitability-oracle/src/lib.rs`
- Design synthesis: `shared/artifacts/project7/04-design-decisions-synthesis.md`
- Launch runbook: `docs/mainnet-launch-runbook.md` · Incident response: `docs/incident-response.md`
- Security policy: `SECURITY.md`
