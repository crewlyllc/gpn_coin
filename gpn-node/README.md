# GPN — GPU-Native Network

A GPU-native blockchain combining **GPU proof-of-work mining**, **GPU-attested staking**, a **decentralized AI compute marketplace**, and native tokenomics — built on the **Polkadot SDK (Substrate)**.

> **Status:** Working local testnet. All 5 pallets + runtime + node compile; the node mines blocks, pays genesis balances, and exposes RPC. PoW is currently a CPU placeholder — the real GPU Ethash algorithm is the next major task.

---

## What it is

GPN is a single chain that merges four ideas most crypto projects treat as separate:

1. **GPU PoW mining** — `pallet-gpu-pow`: Ethash-compatible mining with difficulty adjustment, epoch/DAG tracking, block rewards, and halving. ASIC-resistant by design.
2. **GPU-attested staking** — `pallet-gpu-attestation`: Proof-of-Staked-GPU (PoSG). Miners stake GPN and prove they hold GPUs (challenge-response attestation) to earn a staking weight multiplier (Bronze 1× / Silver 1.5× / Gold 2.5×).
3. **AI compute marketplace** — `pallet-compute-marketplace`: Decentralized GPU rental. Providers register GPU capability specs; requesters create jobs; escrow-based payment with a 5% marketplace fee. 7-state job lifecycle.
4. **Tokenomics** — `pallet-gpn-token`: Inflation schedule (10% → 1% terminal, −0.5%/yr), fee distribution (staking/treasury/burn/oracle), supply cap, reward pools.

Plus a **profitability oracle** (`pallet-profitability-oracle`) that feeds GPU prices, utilization, and staking APR on-chain — the foundation for auto-switching between mining and compute (the novel differentiator: no existing project merges GPU mining + compute marketplace in one network).

---

## Architecture

- **Framework:** Substrate / Polkadot SDK (pinned to commit `da5c966029ce352120d5928d789ee3df0f97db46`)
- **Consensus:** `sc-consensus-pow` with a custom `PowAlgorithm` (currently CPU placeholder; GPU Ethash planned)
- **Runtime APIs:** `Core`, `Metadata`, `BlockBuilder`, `TaggedTransactionQueue`, `OffchainWorkerApi`, `AccountNonceApi`, `SessionKeys`, `GenesisBuilder`, and a custom `GPUMiningApi`
- **Token decimals:** 12 (GPN). Initial supply: pre-funded dev accounts.

### Workspace layout

```
gpn-node/
├── node/            # Substrate node binary (chain spec, CLI, service, PoW)
├── runtime/         # FRAME runtime wiring all pallets
├── pallets/
│   ├── gpu-pow/             # GPU PoW mining + block rewards
│   ├── gpu-attestation/     # Proof-of-Staked-GPU
│   ├── compute-marketplace/ # AI compute rental + escrow
│   ├── profitability-oracle/# GPU price/utilization/APR feed
│   └── gpn-token/           # Inflation, fees, treasury, pools
└── (gpn-agent/      # standalone Rust miner binary — compiles, GPU integration pending)
```

---

## Build & run

### Prerequisites
- Rust toolchain (`rustup`) with `wasm32-unknown-unknown` target and `rust-src`:
  ```bash
  rustup target add wasm32-unknown-unknown
  rustup component add rust-src
  ```
- System deps:
  ```bash
  sudo apt install -y libclang-dev llvm-dev clang protobuf-compiler pkg-config libssl-dev cmake
  ```

### Build
```bash
cd gpn-node
cargo build --release -p gpn-node
```

### Run a dev node (mines blocks)
```bash
./target/release/gpn-node --dev --tmp --alice --force-authoring
```
- RPC on `127.0.0.1:9944`, metrics on `9615`.
- Pre-funded accounts: Alice, Bob, Charlie, Dave, Eve, Ferdie (each 1e12 GPN).
- The dev chain mines blocks via CPU PoW (`--force-authoring`).

### Other commands
```bash
# Export the chain spec
./target/release/gpn-node build-spec --dev
./target/release/gpn-node export-chain-spec --dev
# Purge dev data
./target/release/gpn-node purge-chain --dev
```

---

## Tests

```bash
cd gpn-node
cargo test --release -p pallet-gpu-pow
cargo test --release -p pallet-gpu-attestation
cargo test --release -p pallet-compute-marketplace
cargo test --release -p pallet-profitability-oracle
cargo test --release -p pallet-gpn-token
```
**75 unit tests** across all 5 pallets (mining, staking/attestation, marketplace lifecycle, oracle, tokenomics).

---

## Known issues / next work

1. **Block rewards not auto-wired to block production.** The reward mechanism works and is unit-tested (`submit_solution` dispatchable → `distribute_pending_rewards` in `on_initialize`), but `sc-consensus-pow` produces sealed blocks without calling `submit_solution`, so rewards don't auto-accrue to miners. **Fix:** wire a custom inherent (or read the block's pre-runtime digest via a public frame_system accessor) so each block's producer is rewarded automatically.
2. **GPU mining is a placeholder.** `GPNPowAlgorithm` in `node/src/service.rs` is CPU (blake2-based). The real Ethash-derived algorithm lives in `pallet-gpu-pow` + `gpn-agent` but isn't integrated into the node's consensus yet.
3. **Warnings:** deprecated hard-coded `#[pallet::weight]` values and unused imports remain (cosmetic).
4. **RPC:** no custom RPC modules for the GPN pallets yet (99 default methods only).

---

## Design note

The original architecture memo recommended **Cosmos SDK**, but the entire build is on **Substrate/Polkadot SDK**. Given a working Substrate codebase now exists (pallets, PoW consensus, runtime), Substrate is the pragmatic choice — FRAME's pallet model fits the multi-pallet design well, and `sc-consensus-pow` gives us the GPU PoW hook directly. See `shared/artifacts/project7/architecture-decisions.md` for the original analysis.
