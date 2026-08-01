# Pallet Architecture Overview — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches the live runtime (`gpn-node/runtime/src/lib.rs`, Cycle 32 audit)
**Date:** 2026-08-01
**Author:** PM07

> Source-of-truth reference for the five custom pallets that make up the GPU-Native
> Network runtime. Where this doc and the code disagree, **the code wins**.

---

## 1. Runtime Wiring

`construct_runtime!` order (all custom pallets):

```rust
GPUPoW: pallet_gpu_pow,
GPUAttestation: pallet_gpu_attestation,
ComputeMarketplace: pallet_compute_marketplace,
ProfitabilityOracle: pallet_profitability_oracle,
GPNToken: pallet_gpn_token,
```

Dependency graph (who calls whom):

```
gpu-pow ──mints block reward──▶ gpn-token (mint_block_reward)
gpu-pow ──(PoW seal)────────────────────────────────────────┐
gpu-attestation ──provides GPUAttestationProvider trait────▶ compute-marketplace
                                                             (min_gflops gate at accept_job)
compute-marketplace ──charges 5% fee──▶ gpn-token (distribute_fees)
profitability-oracle ──feed──▶ gpn-agent (ProfitabilityCalculator)
```

---

## 2. Pallet Inventory

| Pallet | Crate | Role | Key storage |
|--------|-------|------|-------------|
| `gpu-pow` | `pallet_gpu_pow` | Ethash PoW consensus + block reward mint | `Difficulty`, `CurrentBlockReward`, `TotalMinted`, `PendingBlockRewards` |
| `gpu-attestation` | `pallet_gpu_attestation` | Proof-of-Staked-GPU (deposit + GFLOPS proof) | `Attestations`, `ProviderStake`, `AttestationChallenge`, `TotalStaked` |
| `compute-marketplace` | `pallet_compute_marketplace` | GPU job marketplace (register/create/accept/start/submit/complete) | `Jobs`, `JobEscrow`, `RegisteredProviders`, `JobProvider`, `ProviderJobs` |
| `profitability-oracle` | `pallet_profitability_oracle` | On-chain profitability feed (token price / GPU $/hr / utilization / APR) | `CurrentFeed`, `OracleOperators`, `LastUpdateBlock` |
| `gpn-token` | `pallet_gpn_token` | Token economics: mint, inflation decay, fee split, staking pool | `CurrentInflation`, `TotalMinted`, `FeeDistributionConfig`, `StakingRewardPool`, `OracleRewardPool` |

Full tokenomics spec: `design/tokenomics/01-gpn-tokenomics.md`.

---

## 3. Per-Pallet Public API (extrinsics)

### 3.1 `gpu-pow` — `pallet_gpu_pow`
- `submit_solution(seal, nonce, beneficiary)` — submit an Ethash PoW solution; on valid
  seal the pallet mints the block reward to `beneficiary` via `gpn-token::mint_block_reward`.
- `adjust_difficulty()` — retarget difficulty to keep block time near target (15s).
- (read-only) `get_mining_info() -> MiningInfo` — current difficulty, epoch, reward,
  beneficiary, target — used by the agent's miner to know what to mine.

Storage: `Difficulty`, `LastBlockTimestamp`, `LastAdjustmentBlock`, `CurrentEpoch`,
`PendingBlockRewards`, `PendingRewardBeneficiary`, `LastHalvingBlock`, `CurrentBlockReward`,
`TotalMinted`.

### 3.2 `gpu-attestation` — `pallet_gpu_attestation`
- `deposit_stake(amount)` — lock ≥ `MinAttestationStake` (1,000 GPN) as provider bond.
- `submit_attestation(record)` — submit a measured `AttestationRecord` (gpu_count,
  compute_gflops, tier) proving real GPU compute; requires prior stake.
- `withdraw_stake()` — release stake after `check_expired` clears attestation.
- `check_expired()` — re-evaluate / expire stale attestations.
- (read-only) `effective_stake(who)`, `account_id()`.

Cross-pallet: exposes `GPUAttestationProvider<AccountId>` trait (`attested_gflops`,
`attested_tier`) consumed by `compute-marketplace` for the `min_gflops` gate.

Storage: `AttestationChallenge`, `Attestations`, `AttestedProviderCount`, `TotalStaked`,
`ProviderStake`.

### 3.3 `compute-marketplace` — `pallet_compute_marketplace`
Job lifecycle (requestor ↔ provider, real two-party escrow):
1. `register_provider(requirements)` — provider declares `GPURequirements` (idempotent).
2. `unregister_provider()` — leave the market.
3. `create_job(...)` — requestor posts a job with `gpu_requirements`, price, escrow deposit
   (debited immediately, held in `JobEscrow`).
4. `accept_job(job_id)` — provider claims; **rejected with `ProviderComputeInsufficient`**
   if `attested_gflops < min_gflops` (gate).
5. `start_job(job_id)` — provider begins work (bounds state machine).
6. `submit_result(job_id, result_hash)` — deterministic result hash committed.
7. `complete_job(job_id)` — payout: provider receives `price − 5% MarketplaceFeeBps`,
   fee routed to `gpn-token::distribute_fees`.
8. `cancel_job(job_id)` — requestor aborts (escrow refund path).

Job states: `Pending → Accepted → Running → Completed` (plus `Cancelled`).

Storage: `NextJobId`, `Jobs`, `JobRequestor`, `JobProvider`, `ProviderJobs`,
`RequestorJobs`, `JobEscrow`, `RegisteredProviders`, `ActiveJobCount`.

### 3.4 `profitability-oracle` — `pallet_profitability_oracle`
- `update_feed(feed)` — submit a `ProfitabilityFeed` (token price USD, gpu_price USD/hr,
  utilization, staking APR, timestamps) — operators only.
- `add_operator(who)` / `remove_operator(who)` — root-managed operator set.

Storage: `CurrentFeed`, `LastUpdateBlock`, `OracleOperators`, `OperatorCount`.

### 3.5 `gpn-token` — `pallet_gpn_token`
- `mint_block_reward(beneficiary, amount)` — root-only, called by `gpu-pow`; issues GPN
  (guarded by `SupplyCapExceeded`).
- `distribute_fees(total_fee)` — split by `FeeDistributionConfig` (default
  200/150/100/50 bps = staking pool / treasury / burn / oracle).
- `set_fee_distribution(config)` — root-only rebalance; `is_valid()` enforces sum = 500 bps.
- `distribute_staking_rewards()` — drains `StakingRewardPool` and emits payout event
  (per-staker keyed-by-tier distribution is an open Phase-4 item).
- (read-only) `account_id()`, `total_supply()`.

Storage: `CurrentInflation`, `TotalMinted`, `FeeDistributionConfig`,
`LastInflationUpdate`, `StakingRewardPool`, `OracleRewardPool`.

---

## 4. Cross-Pallet Contracts

| Consumer | Provider | Trait / call |
|----------|----------|--------------|
| `gpu-pow` → `gpn-token` | mint | `mint_block_reward` (root) |
| `compute-marketplace` → `gpu-attestation` | read | `GPUAttestationProvider::attested_gflops / attested_tier` |
| `compute-marketplace` → `gpn-token` | fee | `distribute_fees` |
| `gpu-pow` → `gpn-token` | reward pool | staking pool accumulation |

---

## 5. Verification

- `cargo check --workspace` clean (Cycle 26+).
- Pallet test counts at last full run: gpu-attestation **20 pass**,
  compute-marketplace **21 pass** (incl. 4 `min_gflops` gate tests), agent lib
  **22 pass / 7 GPU-ignored**.
- Live on RTX 4060: PoW seal, attest (Bronze @ 188–294 GFLOPS), marketplace two-party
  escrow payout (Bob escrows 50 → Alice earns 47.5), chain-feed-driven auto-switch
  (Mining→Compute→Mining) all verified (Cycles 27–31).

---

## 6. Related Docs

- Tokenomics: `design/tokenomics/01-gpn-tokenomics.md`
- Protocols: `design/protocols/01-gpu-pow-consensus.md`, `02-gpu-attestation.md`,
  `03-compute-marketplace.md`, `04-profitability-oracle.md`
- Agent: `design/node-agent/01-agent-architecture.md`
- Implementation: `gpn-node/pallets/*/src/lib.rs`, `gpn-node/runtime/src/lib.rs`
