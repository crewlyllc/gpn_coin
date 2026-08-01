# GPU PoW Consensus Protocol — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches `pallet_gpu_pow` (Cycle 32 audit)
**Date:** 2026-08-01
**Author:** PM07

> The consensus layer: GPU-only Ethash mining that seals blocks and mints GPN block
> rewards. Source of truth = `gpn-node/pallets/gpu-pow/src/lib.rs`.

---

## 1. Design Goals

- **GPU-only**: the PoW is Ethash, which is memory-hard (DAG-dependent) and effectively
  unmineable on CPU / impractical on ASIC — it rewards GPU hashrate.
- **GPU reuse**: same Ethash kernels are a mature, well-understood GPU workload; the agent
  already ships an OpenCL Ethash miner (`gpu/ethash_kernel.cl`, `gpu/ethash_miner.rs`).
- **Solo-friendly**: prototype supports solo mining against the node's RPC.

## 2. Algorithm

- **Ethash** (Ethereum PoW algorithm, pre-merge).
- Memory-hard: requires a ~GB-scale DAG (epoch-dependent) generated in GPU VRAM.
- The agent builds the DAG once per epoch and searches nonces via OpenCL kernels
  (Keccak + hashimoto) against the current difficulty target.
- Node-verifiable: `submit_solution` lets the node independently verify the seal without
  trusting the miner.

## 3. Block Lifecycle & Reward Mint

```
miner finds nonce satisfying target
        │
        ▼
submit_solution(seal, nonce, beneficiary)   ──▶ node verifies Ethash seal
        │
        ▼ valid
gpu-pow records pending reward
        │
        ▼ on block finalize
gpn-token::mint_block_reward(beneficiary, CurrentBlockReward)   ──▶ GPN issued
```

- `beneficiary` is the miner's address; reward is minted (fair launch — no premine).
- Reward amount = `CurrentBlockReward` (50 GPN at genesis inflation) — see tokenomics spec.

## 4. Difficulty Adjustment

- `adjust_difficulty()` retargets `Difficulty` so average block time tracks the 15s target.
- Tuned live (Cycle 9) on the RTX 4060; `LastAdjustmentBlock` prevents over-frequent
  retargets. This keeps mining feasible on a single consumer GPU on the prototype testnet.

## 5. Epochs

- `CurrentEpoch` tracks the Ethash epoch (DAG regeneration boundary). Agents rebuild the
  DAG on epoch change.

## 6. Storage (source of truth)

| Item | Type | Purpose |
|------|------|---------|
| `Difficulty` | `U256` | Current mining target difficulty |
| `LastBlockTimestamp` | `u64` | For block-time measurement |
| `LastAdjustmentBlock` | `BlockNumber` | Throttle retargets |
| `CurrentEpoch` | `u64` | Ethash DAG epoch |
| `PendingBlockRewards` | `Balance` | Reward owed to beneficiary |
| `PendingRewardBeneficiary` | `AccountId?` | Who gets the pending reward |
| `LastHalvingBlock` | `BlockNumber` | (reserved / inflation-step marker) |
| `CurrentBlockReward` | `Balance` | Active per-block reward |
| `TotalMinted` | `Balance` | Cumulative issuance |

## 7. Agent Integration

- `gpn-agent` mines via `ExternalMiner` / `EthashGpuMiner` against `mining_rpc_url`
  (solo) or a pool URL.
- `get_mining_info()` RPC gives the miner difficulty / epoch / reward / beneficiary so it
  always searches against the current target.
- On switch to `Compute`, the agent **stops mining and releases the DAG** (freeing VRAM)
  so the GPU can serve AI workloads; on switch back it **rebuilds the DAG** and resumes.
  Verified live (Cycle 31: Mining→Compute→Mining on RTX 4060).

## 8. Related

- Tokenomics / rewards: `design/tokenomics/01-gpn-tokenomics.md`
- Agent mining module: `gpn-agent/src/mining/mod.rs`, `gpu/ethash_miner.rs`
- Implementation: `gpn-node/pallets/gpu-pow/src/lib.rs`
