# GPN Network — GPU-Native Crypto & AI Compute Network

A decentralized blockchain where every GPU-connected node can mine, stake, and serve AI compute — auto-switching between them based on real-time profitability.

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                   GPN Network                        │
├─────────────────────────────────────────────────────┤
│  Consensus:   Hybrid PoW (Ethash) + PoSG (GPU Staking)│
│  SDK:         Substrate (Polkadot SDK)              │
│  Token:       GPN (GPU-Native Network)              │
│  Mining:      Ethash-compatible (GPU-only)          │
│  Marketplace: AI compute (inference/training)       │
│  Staking:     Proof of Staked GPU (3 tiers)         │
├─────────────────────────────────────────────────────┤
│  gpn-node       → Substrate-based blockchain node    │
│  gpn-agent      → GPU node agent (mining/compute)   │
│  pallet-gpu-pow → Ethash PoW consensus pallet       │
│  pallet-gpu-attestation → PoSG attestation pallet   │
│  pallet-compute-marketplace → GPU compute jobs       │
└─────────────────────────────────────────────────────┘
```

## Project Structure

```
projectmanager07/
├── gpn-node/          # Substrate blockchain node
│   ├── node/          # Node binary
│   ├── runtime/       # Runtime with custom pallets
│   └── pallets/       # Custom Substrate pallets
│       ├── gpu-pow/             # Ethash mining pallet
│       ├── gpu-attestation/     # Proof of Staked GPU
│       ├── compute-marketplace/ # GPU compute marketplace
│       ├── profitability-oracle/# On-chain profitability feed
│       └── gpn-token/           # Token economics (mint/inflation/fees)
├── gpn-agent/         # GPU node agent (Rust)
│   ├── src/
│   │   ├── main.rs              # Entry point
│   │   ├── lib.rs               # Library (modules + Mode/AgentState)
│   │   ├── gpu/                 # GPU monitoring + OpenCL Ethash miner
│   │   │   ├── ethash_kernel.cl # OpenCL Keccak/DAG/hashimoto kernels
│   │   │   └── ethash_miner.rs  # EthashGpuMiner (DAG build, nonce search)
│   │   ├── bin/gpn_gpu_mine.rs  # Standalone GPU miner demo (node-verifiable seals)
│   │   ├── modes/               # Mode switching
│   │   ├── chain/               # Chain RPC client
│   │   ├── compute/             # Docker executor
│   │   ├── profitability/       # Revenue calculation
│   │   └── config/              # Agent configuration
│   └── config/
│       └── default.toml
├── shared/artifacts/project7/   # Research docs (Phase 1-2)
│   ├── 01-gpu-coins-research.md
│   ├── 02-gpu-compute-networks-research.md
│   ├── 03-hybrid-consensus-research.md
│   └── 04-design-decisions-synthesis.md
├── design/           # Source-of-truth design (aligned to implemented code)
│   ├── pallets/00-pallet-overview.md
│   ├── protocols/01-gpu-pow-consensus.md
│   ├── protocols/02-gpu-attestation.md
│   ├── protocols/03-compute-marketplace.md
│   ├── protocols/04-profitability-oracle.md
│   ├── node-agent/01-agent-architecture.md
│   └── tokenomics/01-gpn-tokenomics.md
└── HEARTBEAT.md
```

## Key Design Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Chain SDK | Substrate | Rust, custom consensus, max flexibility |
| Mining algorithm | Ethash-compatible | GPU reuse, mature ecosystem |
| Staking | PoSG (Proof of Staked GPU) | GPU attestation + weight multipliers |
| Marketplace | Hybrid auction + queue | Covers all use cases |
| Auto-switch | Agent-based | Fast decisions, no on-chain overhead |
| Tokenomics | Fair launch, disinflationary | Market trust, no ICO |

## Getting Started

### Prerequisites
- Rust toolchain (nightly)
- Substrate development dependencies

### Build
```bash
cd gpn-node
cargo build --release
```

### Run a local testnet
```bash
# Terminal 1: Start node (Alice)
cargo run --release -- --chain dev --alice

# Terminal 2: Start node (Bob)
cargo run --release -- --chain dev --bob --bootnodes /ip4/127.0.0.1/tcp/30333/p2p/ALICE_PEER_ID
```

### Two-node GPU p2p testnet (integration test)

`gpn-node/scripts/gpn_two_node_testnet.sh` boots **two** `gpn-node` processes on the
`local` chain spec (Alice + Bob authorities) over libp2p and verifies, end-to-end:

1. **Both** nodes mine with the **real GPU (OpenCL) Ethash miner** — not the CPU fallback.
2. Both nodes connect as peers over `--bootnodes`.
3. Both nodes converge on the **same longest chain** (identical best-block hash) —
   blocks mined by either peer are imported by the other.

This is the longest-chain + GPU-mining-across-peers integration test for the
Phase-3 prototype. Requires a GPU (NVIDIA OpenCL ICD, e.g. an RTX 4060).

```bash
cd gpn-node
# Release binary must be a cycle-11+ build (includes the GPU miner).
# Use target/debug/gpn-node if unsure — a stale release predates the GPU miner
# and will silently fall back to CPU mining (the script will report a failure).
cargo build --release
bash scripts/gpn_two_node_testnet.sh            # uses target/release/gpn-node
bash scripts/gpn_two_node_testnet.sh --binary target/debug/gpn-node
bash scripts/gpn_two_node_testnet.sh --keep    # leave nodes running to poke at them
```

Expected success output (exit code 0):

```
✅ node A GPU miner online
✅ node A produced blocks
✅ node B GPU miner online
✅ node B connected as a peer
✅ node B imported blocks (synced)
✅ nodes converged on same mined block: best: #7 (...)
==== RESULT: 6 passed, 0 failed ====
```

> **Note:** a node should run **either** the internal self-miner **or** serve external
> miners, not both — the self-miner consumes every template within ms, so external
> miners see `None`. This script uses the internal GPU self-miner on both nodes.

### Run GPU node agent
```bash
cd gpn-agent
cargo run -- config/default.toml
```

## Phase Status

- ✅ **Phase 1 (Research):** Complete
- ✅ **Phase 2 (Design):** Complete
- 🏗️ **Phase 3 (Prototype):** Dev testnet live — real Ethash PoW consensus + self-tuning difficulty + **real GPU Ethash miner on the RTX 4060** (OpenCL, produces node-verifiable seals); GPU miner wired into live node; **seal-format + extrinsic tests committed**; **TransactionPayment fees live**; **external miner RPC live** (`gpn_getTemplate`/`gpn_submitWork` — node & miner separable, verified end-to-end). Next: wire gpn-agent GPU miner to the RPC + profitability auto-switch prototype
- ⏳ **Phase 4 (Launch):** Not started

## GPU Mining (gpn-agent)

`gpn_gpu_mine` runs a real OpenCL Ethash miner on the GPU (no CUDA toolkit / C headers needed — kernels compile at runtime via `clBuildProgram`, using the NVIDIA OpenCL ICD):

```bash
cd gpn-agent
cargo run --release --bin gpn_gpu_mine   # builds ~1 GiB epoch-0 DAG, mines below 2^245, self-verifies vs node verify_seal
```

Heavy GPU correctness tests (build the 1 GiB DAG, ~34 s each):

```bash
cargo test -- --ignored --nocapture gpu_hashimoto_matches_reference
```

## External Mining RPC

The node exposes two JSON-RPC methods so a miner can run separately from the node (the production mining architecture — and the substrate for GPU auto-switch):

- `gpn_getTemplate` → `{ best_hash, pre_hash, difficulty, block_number }`
- `gpn_submitWork(pre_hash, seal)` → `bool` (seal = 8-byte LE nonce ++ 32-byte mix hash)

Run an authority node **without** the internal self-miner to leave templates available for external miners:

```bash
gpn-node --alice --chain local --tmp --rpc-port 9944   # no --force-authoring
```

Then, from another process (or a reference CPU miner):

```bash
# fetch template, mine a seal, submit it
PRE=$(curl -s -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"gpn_getTemplate","params":[]}' http://127.0.0.1:9944/ | jq -r '.result.pre_hash, .result.difficulty')
PREHASH=$(echo $PRE | cut -d' ' -f1); TGT=$(echo $PRE | cut -d' ' -f2)
SEAL=$(gpn-node/target/release/gpn_mine_seal "$PREHASH" "$TGT")
curl -s -H 'Content-Type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"gpn_submitWork\",\"params\":[\"$PREHASH\",\"$SEAL\"]}" http://127.0.0.1:9944/
```

**Note:** a node runs either the internal self-miner (`--force-authoring` / `--dev`) **or** serves external miners — not both. The self-miner consumes every build within milliseconds, leaving no template for external miners.

### GPU external miner (`gpn-agent`)

The full production path — mine on your own GPU from a machine that does **not** run a full node:

```bash
# 1. node (authority, no internal miner — leaves templates available)
gpn-node --alice --chain local --tmp --rpc-port 9944 --node-key <KEY_A>

# 2. external GPU miner (RTX 4060, OpenCL) — fetches template, mines, submits over HTTP
cargo run -p gpn-agent --release --bin gpn_external_miner -- http://127.0.0.1:9944 0 1048576
```

Mining is now a single HTTP call, which is the substrate for GPU auto-switch (the GPU can alternate between serving AI compute and mining by starting/stopping this loop).

## GPU Compute Marketplace

The marketplace (`pallet-compute-marketplace`) is where providers offer GPU time and
requestors post paid AI jobs (LLM inference, fine-tuning, etc.). Payment uses **escrow**:
on `create_job` the requestor's tokens move to the marketplace account; on `complete_job`
the provider is paid from escrow minus a `MarketplaceFeeBps` (5%) marketplace fee.

### Lifecycle
```
create_job (Pending, escrow debited from requestor)
  → accept_job  (Pending → Accepted; gated by attested GFLOPS vs min_gflops)
  → start_job   (Accepted → Running)
  → submit_result (Running → AwaitingVerification)
  → complete_job  (→ Completed; payout to provider from escrow)
```

### GFLOPS units convention
`min_gflops` in `GPURequirements` is **true FP32 GFLOPS** (billions of float ops/sec).
The agent measures this with a dense-GEMM benchmark (`gpu::measure_gflops`). The attestation
pallet tiers are gated on it (Bronze ≥ 100 / Silver ≥ 1000 / Gold ≥ 5000 GFLOPS), and a
provider must be **attested** to at least a job's `min_gflops` to accept it
(`ProviderComputeInsufficient` otherwise). `min_gflops = 0` = open market (no gate).

### Single-account live demo (needs GPU + prior attestation)
`gpn_marketplace_demo` drives the full lifecycle on the GPU with **one account** for both
sides — best for exercising the `min_gflops` attestation gate on the RTX 4060.
```bash
cargo run -p gpn-agent --release --bin gpn_marketplace_demo -- ws://127.0.0.1:9944 //Alice 150
```

### Two-account requestor→provider demo (no GPU needed)
`gpn_two_account_demo` proves the **real multi-party escrow/payment path**: a distinct
paying customer (requestor) and a distinct provider. Runs against a plain CPU dev node.
```bash
# 1. boot dev node (funds Alice & Bob — the dev chain-spec endows 1,000,000 GPN each)
gpn-node/target/release/gpn-node --dev --rpc-port 9944 --tmp

# 2. Bob (requestor) posts a 50 GPN job; Alice (provider) serves it; asserts payout
cd gpn-agent
cargo run --release --bin gpn_two_account_demo -- ws://127.0.0.1:9944 //Bob //Alice 50 0
#   → ✅ job Completed; requestor=Bob, provider=Alice
#   → ✅ Bob lost 50 GPN (escrow); Alice earned 47.5 GPN (50 − 5% fee)
```
Args: `[endpoint] [requestor_uri] [provider_uri] [price_gpn] [min_gflops]`. Set
`min_gflops > 0` to exercise the attestation gate (provider must be attested first).

### Fee economy: `gpn_distribute_rewards` (cycle 36)

The `gpn-token` pallet splits marketplace/transaction fees four ways — staking pool
200 / treasury 150 / burn 100 / oracle pool 50 bps — via `GPNToken::distribute_fees`,
which is **root-only**. The GPN runtime now includes the **Sudo pallet** (dev/ops
tooling) so root calls are dispatchable by a signed account; `gpn_distribute_rewards`
exercises the whole fee economy end-to-end on-chain:

1. (optional) sudo-seed the `gpn/tokn` pallet account (simulates collected fees — see
   the funding gap note below);
2. `Sudo.sudo(GPNToken.distribute_fees(total_fee))` — fills pools, funds treasury, burns;
3. `GPNToken.distribute_staking_rewards(stakers)` — **tier-weighted** payout
   (proof-of-staked-GPU: attested tier multiplies share; unattested excluded);
4. `GPNToken.distribute_oracle_rewards(operators)` — even split.

```bash
# 1. boot dev node (Alice is the sudo key on dev/local)
gpn-node/target/release/gpn-node --dev --rpc-port 9944 --tmp

# 2. seed 3000 GPN into the token pallet, distribute 1000 GPN fee, pay Alice (staker+operator)
cd gpn-agent
cargo run --release --bin gpn_distribute_rewards -- ws://127.0.0.1:9944 1000 //Alice //Alice //Alice 3000
#   → StakingRewardPool 0→400, OracleRewardPool 0→100   (after distribute_fees)
#   → OracleRewardPool drained to 0 (Alice paid 100)
#   → StakingRewardPool stays 400 when Alice is UNATTESTED (tier multiplier 0 → excluded)
```
Args: `[endpoint] [total_fee_gpn] [sudo_uri] [stakers_csv] [operators_csv] [seed_gpn]`.

**⚠ Funding gap (found cycle 36):** `distribute_fees` draws from the **`gpn/tokn`** pallet
account, but marketplace fees (`MarketplaceFeeBps` 5%) currently land in the **`gpn/cmkt`**
marketplace account — the two are not connected, so the pools stay empty in normal
operation. Until that's wired (route marketplace fees → `gpn/tokn`, or have the marketplace
call `distribute_fees`), the `seed_gpn` arg simulates collected fees so the economy is
observable. **Mainnet:** the sudo key is a non-endowed placeholder in the `mainnet` spec
(Alice on dev/local) and MUST be removed/rotated before real launch.

### Live chain-feed-driven auto-switch (cycle 31)
In `auto` mode the agent's profitability feed drives mode selection. `profitability_feed =
"chain"` (default) reads the **real on-chain oracle** (`ProfitabilityOracle::CurrentFeed`:
token price, GPU-hour price, utilization, APR) + `GPUPoW::Difficulty`; `"scripted"` uses a
deterministic demand-spike timeline (for demos/CI). When the live feed says compute is more
profitable, the agent switches Mining→Compute and **claims & serves marketplace jobs
on-chain** (accept→start→real GPU GEMM→submit→complete+payout).

```bash
# seed a compute-favorable feed (token=$0.075, GPU=$12/hr, util=95%, APR=15%)
gpn-agent/target/release/gpn_seed_feed ws://127.0.0.1:9944 75000 12000000 9500 1500 //Alice

# create a Pending job for the agent to claim
gpn-agent/target/release/gpn_create_job ws://127.0.0.1:9944 //Bob 0

# run the agent in auto + chain-feed mode (config/chain_autoclaim.toml)
gpn-agent/target/release/gpn-agent gpn-agent/config/chain_autoclaim.toml
#   → Profitability: Mining=$0.00/hr, Compute=$9.69/hr  (from live feed)
#   → 🔄 Mining → Compute  → 1 claimable job(s) found → ✅ job #1 completed & paid out
```

**Units note (fix, cycle 31):** `calc_compute_revenue` returns **USD/hr** = GPU-hour price
× utilization × 0.85. The oracle's GPU-hour price is already USD (micro-USD), so it must
NOT be multiplied by token price again — doing so produced USD² and capped compute revenue
~13× too low, which kept the chain feed from ever recommending Compute.
