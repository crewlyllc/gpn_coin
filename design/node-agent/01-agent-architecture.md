# GPU Node Agent Architecture — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches `gpn-agent` crate (Cycle 32 audit)
**Date:** 2026-08-01
**Author:** PM07

> The `gpn-agent` is the software every GPU node runs. It mines, attests, serves AI
> compute, and auto-switches between modes by profitability. Source of truth =
> `gpn-agent/` (Rust).

---

## 1. Crate Layout

```
gpn-agent/
├── src/
│   ├── main.rs                 # Entry point
│   ├── lib.rs                  # Library: Mode enum, AgentState, orchestration
│   ├── gpu/                    # GPU monitoring + OpenCL
│   │   ├── ethash_kernel.cl    # Keccak / DAG / hashimoto kernels
│   │   ├── ethash_miner.rs     # EthashGpuMiner (DAG build, nonce search)
│   │   ├── matmul_kernel.cl    # GPU GEMM kernel (compute workload)
│   │   └── matmul.rs           # Real GFLOPS measurement + GEMM executor
│   ├── modes/mod.rs            # ModeManager: Mining/Compute/StakeOnly transitions
│   ├── mining/mod.rs           # ExternalMiner / mining engine control
│   ├── compute/                # ComputeExecutor (Docker), job serving
│   ├── chain/                  # ChainClient (subxt) + signer (Sr25519)
│   ├── profitability/          # ProfitAnalysis, ProfitabilityCalculator, decide_switch
│   ├── config/mod.rs           # AgentConfig (default.toml)
│   └── utils/mod.rs
├── config/default.toml         # Runtime config
└── src/bin/                    # Standalone tooling (see §5)
```

## 2. Modes & Auto-Switch

`Mode = Mining | Compute | StakeOnly` (and `Idle`).

- **Mining**: run `EthashGpuMiner` against `mining_rpc_url` (solo) or pool; on switch to
  Compute it **stops mining and releases the DAG**; on switch back it **rebuilds the DAG**.
- **Compute**: run `ComputeExecutor` (Docker) to serve marketplace jobs; real GPU GEMM via
  `matmul`.
- **StakeOnly**: hold attestation stake, no active GPU work (revenue = staking APR).
- **Auto**: `ModeManager` switches based on `ProfitabilityCalculator::analyze()` +
  `decide_switch()` with 5% hysteresis and a `min_epoch_hours` floor to prevent flapping.

```
analyze() → ProfitAnalysis { mining_revenue, compute_revenue, staking_revenue } (USD/hr)
decide_switch(analysis, current) → recommended Mode
ModeManager::switch_to(Mode) → starts/stops engines, releases/rebuilds DAG
```

Revenue formulas:
- `mining = calc_mining_revenue(hashrate, difficulty, token_price)`
- `staking = calc_staking_revenue(staked_amount, apr, token_price)`
- `compute = calc_compute_revenue(gpu_price, utilization) = gpu_price × utilization × 0.85`
  (USD/hr, **no** token double-conversion — Cycle 31 fix)

## 3. Chain Client (`chain/`)

- `ChainClient` (subxt) talks to the Substrate node over WebSocket (`ws://localhost:9944`).
- `signer.rs` — `Sr25519Signer`; signed txs use an explicit head-block nonce
  (`get_account_nonce`) to avoid "Transaction is outdated" on back-to-back txs.
- Dynamic storage decoding recursively handles nested composites (AccountId32 display fix,
  Cycle 29) and U128/U256.
- Key calls: `submit_solution`, `deposit_stake`, `submit_attestation`,
  `register_provider`, `create_job`, `accept_job`, `start_job`, `submit_result`,
  `complete_job`, `list_claimable_jobs`, `update_feed` (seed), `get_account_info`.

## 4. Config (`config/default.toml`)

| Key | Default | Meaning |
|-----|---------|---------|
| `chain_endpoint` | `ws://localhost:9944` | Node RPC |
| `mode` | `auto` | `auto` / `mining` / `compute` / `stake_only` |
| `profitability_feed` | `chain` | `chain` (on-chain oracle) or `scripted` |
| `hysteresis_percent` | `5.0` | Switch threshold to avoid flapping |
| `min_epoch_hours` | `1.0` | Minimum time in a mode before switching |
| `attestation_interval_secs` | `3600` | Re-attest cadence |
| `staked_amount` | `10000.0` | GPN bonded for attestation |
| `mining_algorithm` | `ethash` | PoW algorithm |
| `solo_mining` | `true` | Solo vs pool |
| `max_concurrent_jobs` | `2` | Compute job concurrency |
| `docker_endpoint` | `unix:///var/run/docker.sock` | Compute executor runtime |

## 5. Standalone Tooling (`src/bin/`)

| Binary | Purpose |
|--------|---------|
| `gpn_gpu_mine` | Standalone GPU miner (node-verifiable seals) |
| `gpn_gpu_compute` | Standalone GPU GEMM / GFLOPS benchmark |
| `gpn_external_miner` | External-miner RPC harness |
| `gpn_marketplace_demo` | Single-account marketplace demo |
| `gpn_two_account_demo` | **Two-party** requestor(Bob)→provider(Alice) escrow demo (CPU, no GPU needed) |
| `gpn_create_job` | Requestor side: post a job (leaves Pending) |
| `gpn_seed_feed` | Seed an on-chain profitability feed (test) |
| `gpn_simulate_autoswitch` | Scripted auto-switch simulation |
| `gpn_check_state` | Read live on-chain nonce/balance/staked/attested + attestation record |

## 6. Verified Live (RTX 4060, Cycles 27–31)

- Attest: `staked=true attested=true`, Bronze @ 188–294 GFLOPS.
- Marketplace: Bob escrows 50 GPN → Alice earns 47.5 (two-account, on-chain).
- Auto-switch: chain feed `GPU=$12/hr util=95%` → `Mining→Compute` (DAG released) → claim
  & serve real 2048³ GEMM job #1 → paid → demand fades → `Compute→Mining`.

## 7. Open Items (Phase 4)

- Docker compute executor hardening (image pinning, sandboxing, result verification).
- Pool-mode mining config on mainnet.
- Malicious-result slashing in the marketplace.
- Feed staleness handling.

## 8. Related

- Protocols: `design/protocols/01..04-*.md`
- Pallet overview: `design/pallets/00-pallet-overview.md`
- Implementation: `gpn-agent/`
