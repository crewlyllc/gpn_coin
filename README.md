# GPN Network — GPU-Native Crypto & AI Compute Network

A decentralized blockchain where every GPU-connected node can mine, stake, and serve AI compute — auto-switching between them based on real-time profitability.

> **Official public home: <https://github.com/crewlyllc/gpn_coin>**
> Browse the code and download builds (no account or token needed) from
> <https://github.com/crewlyllc/gpn_coin/releases>.
> That public repository is an automatic one-way mirror (code + release assets) of this
> private development repository, which stays the origin for development, CI and the
> gpncoin.ai website deployment.

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
├── shared/artifacts/project7/   # Research + cycle logs + ops docs
│   ├── 01-gpu-coins-research.md
│   ├── 02-gpu-compute-networks-research.md
│   ├── 03-hybrid-consensus-research.md
│   ├── 04-design-decisions-synthesis.md
│   └── testnet-public-runbook.md  # Public testnet spec/runbook (Phase 4 prep)
├── design/           # Source-of-truth design (aligned to implemented code)
│   ├── pallets/00-pallet-overview.md
│   ├── protocols/01-gpu-pow-consensus.md
│   ├── protocols/02-gpu-attestation.md
│   ├── protocols/03-compute-marketplace.md
│   ├── protocols/04-profitability-oracle.md
│   ├── protocols/05-oracle-governance.md  # Oracle operator governance spec (§9 #5)
│   ├── node-agent/01-agent-architecture.md
│   └── tokenomics/01-gpn-tokenomics.md
├── docs/            # Ops runbooks
│   ├── mainnet-launch-runbook.md
│   ├── upgrades-governance.md   # Mainnet runtime-upgrade pipeline (committee)
│   └── incident-response.md
├── SECURITY.md      # Vulnerability disclosure policy
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
| Tokenomics | Mining + disclosed vested treasury (100 M of ≈ 520.5 M cap), halving-only | Market trust, no ICO |

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

### Chain specs
`--chain` selects the genesis spec (all share the same WASM runtime):
- `dev` — single-node dev, pre-funded accounts + Alice sudo (Cycle 7+)
- `local` — 2–4 node GPU p2p testnet, pre-funded (Cycle 13)
- `testnet` — **public testnet (Cycle 43)**: config-only fork of mainnet, `ChainType::Local`,
  NO endowed accounts (fair-launch economics, operators get stake via off-chain drip faucet),
  bootstrap oracle operators Alice+Bob seeded at genesis, ops sudo key Alice; identical
  difficulty/reward/inflation/cap to mainnet.
- `mainnet` — mining launch (Cycle 35) with a **disclosed vested treasury**
  (100M GPN: 70M genesis-unlocked, 30M over 10 years, 13-of-25 governance — see `pallet-gpn-treasury`);
  zero oracle operators at genesis; inert sudo. Max mineable = 420.48M GPN (hard cap ≈ 520.5M with treasury).

### Set-aside reserves (funding a specific account at genesis)

On `dev` / `local` (and optionally `testnet`) you can **set GPN aside in a
separate account** at genesis — useful for a treasury, grant pool, ops reserve,
or a cold account — without touching another account's funds. Because GPN is a
**fair launch** (no runtime premine), reserve balances are **genesis-only** and
never apply to `mainnet`.

Configure at chain-spec build time with `GPN_GENESIS_RESERVE` (whitespace-trimmed,
comma-separated entries of `name:ss58address:whole_gpn`):

```bash
# dev chain with a 1,000,000 GPN treasury reserve + a 250,000 GPN cold reserve
export GPN_GENESIS_RESERVE="treasury:5E6nohVV...:1000000,cold:5GrwvaEF5...:250000"
gpn-node build-spec --chain dev --disable-default-bootnode > dev-custom.json
gpn-node --chain dev-custom.json
```

Notes:
- Amounts are in **whole GPN** (converted to 12-decimal base units).
- Reserves are **not** applied if `endowed_accounts` is empty — i.e. `testnet`
  and `mainnet` stay fair-launch unless you explicitly add reserves to the
  testnet spec (see `chain_spec.rs::testnet_chain_spec`).
- Listing the same address twice sums the balances.
- To view the resulting allocation:
  `gpn-node build-spec --chain dev --disable-default-bootnode | python3 -c "import json,sys; print(json.load(sys.stdin)['genesis']['runtimeGenesis']['config']['balances']['balances'])"`
- Env var is read when the spec is built (`build-spec`) or when the embedded spec
  is built by the node at startup. For a baked-in default, edit
  `default_reserves()` in `gpn-node/node/src/chain_spec.rs`.

### Vested treasury (`pallet-gpn-treasury`)

Mainnet ships with a **disclosed, vested treasury**: **100,000,000 GPN** funded to the
keyless `gpn/trsy` pallet account at genesis. **70 M is unlocked at genesis** and the
remaining **30 M vests over 10 years** (20 × 1.5M six-month tranches). Spending requires
a **13-of-25** approval from the governance roster (`GPN_TREASURY_ROSTER` env, else
placeholders). The allocation is fixed and counts toward the **≈ 520.5 M GPN hard cap**
(≈ 420.48 M mined + 100 M treasury). See `design-treasury-vesting.md`.

**Operating the treasury** — the `gpn_treasury` CLI:
```bash
# read-only
gpn_treasury status    # balance, unlocked, spendable, threshold, roster size
gpn_treasury schedule  # vesting tranches (unlock block, amount, unlocked?)
gpn_treasury roster    # roster size + threshold
gpn_treasury proposals # pending spend proposals

# signed (each signer must be a roster member)
gpn_treasury propose <dest-ss58> <amount-gpn> --signer //member1
gpn_treasury approve <proposal-id>            --signer //member2   # repeat to 13
gpn_treasury execute <proposal-id>            --signer //member1
```
The treasury account has **no private key** — funds move only through this 13-of-25
flow, bounded by the unlocked tranche total.

### Running on Windows / Linux / macOS

GPN targets **Linux** first-class and is **cross-platform friendly**:

- **Miner / agent / node** — pure Rust; builds on Linux and Windows. The GPU
  miner uses OpenCL via `opencl3` (loaded **at runtime**, so it works on any OS
  with a vendor OpenCL driver — NVIDIA/AMD/Intel drivers ship one). No CUDA
  toolkit required.
- **Windows build deps:** install the **MSVC build tools**, LLVM/Clang, and the
  Rust targets `wasm32-unknown-unknown` + component `rust-src`. The node is a
  Substrate binary — build tooling on Windows is heavier than Linux; the
  **miner/agent/CLI** are the lightweight cross-platform pieces.
- **Temp paths** are platform-aware (`std::env::temp_dir()`), not hard-coded `/tmp`.
- **Docker endpoint** defaults per-OS (`npipe:////./pipe/docker_engine` on
  Windows, `unix:///var/run/docker.sock` elsewhere). Docker is optional for the
  prototype.
- **NVIDIA drivers** must be installed on the host for GPU mining/compute.


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
- ✅ **Phase 3 (Prototype):** Complete — a full dev testnet is live on the RTX 4060 with **real GPU-only Ethash PoW** (OpenCL miner, self-tuning difficulty, node-verifiable seals), **Proof-of-Staked-GPU** (stake 1,000 GPN + attest real GFLOPS → 3-tier weighted rewards), a **compute marketplace** (two-party escrow, jobs, SLA, `min_gflops` gate, 5% fee), a **fully governed profitability oracle** (Phase A→B→C: committee admission, per-operator feeds, staking-backed slashing, on-chain bond-weighted median — all 5 governance code gaps closed), and **auto-switch** (one GPU alternates Mining ↔ Compute by live chain feed, no human). Tooling: external miner RPC (`gpn_getTemplate`/`gpn_submitWork`), transaction fees, drip faucet, block explorer, two-node GPU p2p testnet, CI. All live-verified (cycles 8–58).
- ⏳ **Phase 4 (Launch):** Ready to execute — **blocked only on Jeremy's go/no-go for business decisions D1–D5 + a ~$10–20/mo VPS** (see `shared/artifacts/project7/cycle63-decision-card.md`). The public-testnet toolchain (two-node GPU p2p, explorer, faucet) was dry-run-verified end-to-end in cycle 61; the launch is turnkey via `shared/artifacts/project7/testnet-public-runbook.md §7`.

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

**✅ Funding gap CLOSED (cycle 37):** `distribute_fees` draws from the **`gpn/tokn`**
pallet account, and marketplace fees (`MarketplaceFeeBps` 5%) now **flow into that same
account** — `pallet-compute-marketplace` routes the fee to a `FeeSink` (wired to the GPN
token pallet) at `complete_job`, so the fee economy is **self-funding** in normal
operation. The `seed_gpn` arg is now only needed to top up the token pallet for a manual
exercise (real fees accumulate there automatically). Verified live on `gpn-node --dev`:
a 50 GPN job's 2.5 GPN fee landed in `gpn/tokn` (0 → 2.5 GPN), then `distribute_fees`
drained it into the pools/treasury/burn. **Mainnet:** the sudo key is a non-endowed
placeholder in the `mainnet` spec (Alice on dev/local) and MUST be removed/rotated before
real launch.

### Live chain-feed-driven auto-switch (cycle 31)
In `auto` mode the agent's profitability feed drives mode selection. `profitability_feed =
"chain"` (default) reads the **real on-chain oracle** (`ProfitabilityOracle::CurrentFeed`:
token price, GPU-hour price, utilization, APR) + `GPUPoW::Difficulty`; `"scripted"` uses a
deterministic demand-spike timeline (for demos/CI). When the live feed says compute is more
profitable, the agent switches Mining→Compute and **claims & serves marketplace jobs
on-chain** (accept→start→real GPU GEMM→submit→complete+payout).

```bash
# seed a compute-favorable feed (token=$0.15, GPU=$12/hr, util=95%, APR=15%)
gpn-agent/target/release/gpn_seed_feed ws://127.0.0.1:9944 150000 12000000 9500 1500 //Alice

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

**Staleness guard (cycle 48):** if the on-chain feed's `updated_at` block is older than
`feed_staleness_ttl_blocks` (default `1440` ≈ 6h at 15s blocks), `analyze()` **forces a
conservative Mining default** (compute + staking revenue = 0, so mining wins the switch)
instead of acting on a stale feed. Set `feed_staleness_ttl_blocks = 0` or omit it to
disable the guard. This is oracle-governance §6 gap #1 (see `design/protocols/05-oracle-governance.md`).

**Reasonableness bounds (cycle 49):** if `calc_compute_revenue(gpu_price, utilization)`
exceeds `compute_revenue_max_usd_per_hour` (default `50` USD/hr), `analyze()` treats the
feed as **implausible** (unit misconversion / garbage / outlier) and **forces Mining**
(compute = 0) so one bad feed can't force a mass switch to Compute. Oracle-governance §6
gap #2. Diagnostic: `gpn_check_feed` (exit 0 = plausible, 1 = implausible).

**Median aggregation across operators (cycle 50):** the pallet now stores each operator's
**own** latest feed (`ProfitabilityOracle::OperatorFeeds`, populated on every
`update_feed`). With `feed_aggregation = "median"` (default `"latest"`), `analyze()`
aggregates ALL operators' feeds via a per-field median, so **one outlier operator cannot
move the effective feed** that drives the auto-switch. Oracle-governance §6 gap #3.
Diagnostic: `gpn_aggregate_feed` prints every operator's feed + the median.

**Per-operator update throttle (cycle 58):** `update_feed` is rate-limited **per operator**
(not network-global), so independent committee/bonded operators can each post their own feed
without blocking each other; same-operator rate limiting (every `OracleUpdateInterval` blocks) is
preserved. This unblocks the live multi-operator median aggregation (cycle 50 flagged the old
global throttle as a Phase-C design flaw).

**On-chain effective feed (cycle 56):** the pallet's authoritative Phase-C
**bond-weighted median** (`ProfitabilityOracle::effective_feed()`) is now exposed via
a runtime API + `gpn_effectiveFeed` RPC + the `gpn_effective_feed` read tool. This is
the single source of truth for the aggregate — a client reads exactly what the chain
computes instead of reimplementing the bond-weighted median client-side (as
`gpn_aggregate_feed` does). Read-only; `gpn_effective_feed [endpoint]` (exit 0 = on-chain
effective feed available, 1 = none / unreachable).

### Oracle operator service: `gpn_operate_oracle` (cycle 41)
`gpn_seed_feed` is a one-shot — fine for bootstrapping, but a **real oracle is a
maintained service**. `gpn_operate_oracle` is a long-running loop that keeps
`ProfitabilityOracle.update_feed` fresh:

```bash
# values file (written as a default template on first run): oracle_values.toml
gpn-agent/target/release/gpn_operate_oracle ws://127.0.0.1:9944 oracle_values.toml 60 //Alice
#   → ✅ update_feed dispatched successfully   (first iteration sets the feed)
#   → ⏳ skipped: pallet interval not yet elapsed (UpdateTooSoon)   (back-off, every 600 blocks)
```

- **Values reloaded every iteration** from the TOML file (micro-USD + basis points, same
  units as `gpn_seed_feed`), so an external price feed can update prices without a restart.
- **Respects the pallet guard** (`OracleUpdateInterval` = 600 blocks ≈ 2.5h): never spams,
  treats `UpdateTooSoon` as the expected back-off, not an error.
- **Fatal vs. retry:** `NotAuthorized` (operator not in `OracleOperators`, added via root
  `add_operator`) exits with a clear message; transient errors log and retry.
- Uses the shared `signer::submit_signed` (dispatch-outcome verified, cycle 39) + head-block
  nonce (cycle 27).

CLI: `gpn_operate_oracle [endpoint] [values_file] [interval_secs] [signer_uri]`
(details: `shared/artifacts/project7/cycle41-oracle-operator-live-log.md`)

### Testnet drip faucet: `gpn_faucet` (cycle 44)
An off-chain drip faucet for the public testnet (runbook §5 option 3 — **zero
runtime change**, keeps the fair-launch mainnet runtime pristine). It's a small
HTTP service backed by a funded testnet key that drips a fixed amount of GPN to a
requesting address via a rate-limited signed `Balances.transfer_allow_death`.

```bash
gpn-agent/target/release/gpn_faucet --endpoint ws://127.0.0.1:9944 \
  --listen 127.0.0.1:8081 --signer-uri //Alice --drip-gpn 5000 --reserve-gpn 1000
# operator gets a drip (ss58 or 0x-hex address):
curl "http://127.0.0.1:8081/drip?address=<ss58-or-0xhex>&amount=5000"
# inspect faucet balance + rate-limit ledger:
curl http://127.0.0.1:8081/status
```

- **Rate limiting (per address):** `--cooldown-secs` (default 3600), `--daily-cap-gpn`
  (100k), `--total-cap-gpn` (1M), `--reserve-gpn` (1000 — faucet refuses to drain
  itself below the reserve; the operator must first mine GPN into the faucet key).
- **Reuses the shared signer plumbing** (`signer::submit_signed`, dispatch-outcome
  verified) so a drip that fails dispatch is reported as an error, not a false success.
- **Verified live on `gpn-node --dev`:** 5000 GPN drip to Bob landed on-chain (Alice
  100000→95000, dispatch ok); cooldown → HTTP 429; hex-address drip ok; invalid /
  missing address → HTTP 400; reserve guard → HTTP 500.

CLI: `gpn_faucet [--endpoint …] [--listen …] [--signer-uri …] [--drip-gpn …] [--reserve-gpn …]`
(details: `shared/artifacts/project7/cycle44-faucet-live-log.md`; runbook `§5`)

### Block explorer: `gpn_explorer` (cycle 45)
A **read-only** web block explorer + JSON API over any node's JSON-RPC — the
Phase-4 public-testnet explorer (runbook §7). Safe to point at a public node
because it never signs or writes. Serves a dark-mode HTML dashboard (head block,
live difficulty, profitability feed, account balances + attestation, recent
blocks, marketplace jobs) plus JSON endpoints for scripting:

```
gpn-agent/target/release/gpn_explorer --listen 0.0.0.0:8082 --account //Alice --account //Bob
```

- `GET /` — HTML dashboard · `/api/chain` — head + difficulty + feed + **effective feed**
- `/api/blocks?count=12` — recent blocks (number + hash)
- `/api/account?address=<ss58|//uri|0x-hex>` — balance/nonce/staked/attested/tier
- `/api/jobs` — all marketplace jobs (state, price, min-GFLOPS, requestor, provider)
- `/api/effective-feed` — **on-chain Phase-C bond-weighted median** (cycle 56 `gpn_effectiveFeed` RPC; `{"error":…}` when no bonded operator has posted)

The dashboard now also renders an "On-chain effective feed (Phase-C bond-weighted
median)" section showing the **authoritative** aggregation the pallet computes
(`ProfitabilityOracle::effective_feed()`, cycle 57 wiring) — the same value a
client gets from `gpn_effective_feed`, surfaced alongside the latest posted feed.

CLI: `gpn_explorer [--endpoint ws://127.0.0.1:9944] [--listen 127.0.0.1:8082] [--account …]…`
(details: `shared/artifacts/project7/cycle45-explorer-live-log.md`; runbook `§7`)

## Documentation & ops

- **Mainnet launch runbook** — `docs/mainnet-launch-runbook.md` (genesis build + verification,
  bootnodes, post-launch governance: block beneficiary + oracle operators, treasury ops,
  monitoring, abort criteria).
- **Mainnet runtime upgrades** — `docs/upgrades-governance.md` (the governance pipeline that
  replaces sudo on mainnet: `gpn_gov_upgrade stage/propose/approve/status/execute/verify/rollback`,
  the 2-of-3 committee + 3-day timelock, the verification and rollback rules, and what the
  runtime still lacks).
- **Incident response plan** — `docs/incident-response.md` (severity levels, roles,
  detection, playbooks: chain halt/fork, supply anomaly, treasury/key compromise, oracle feed,
  marketplace escrow, infra).
- **Security policy** — `SECURITY.md` (private disclosure, scope, critical invariants, safe
  harbor, coordinated disclosure given GPN's governed-only emergency path).
