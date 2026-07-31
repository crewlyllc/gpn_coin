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
│       └── ...
├── gpn-agent/         # GPU node agent (Rust)
│   ├── src/
│   │   ├── main.rs              # Entry point
│   │   ├── gpu/                 # GPU monitoring
│   │   ├── modes/               # Mode switching
│   │   ├── chain/               # Chain RPC client
│   │   ├── compute/             # Docker executor
│   │   ├── profitability/       # Revenue calculation
│   │   └── config/              # Agent configuration
│   └── config/
│       └── default.toml
├── shared/artifacts/project7/   # Research & design docs
│   ├── 01-gpu-coins-research.md
│   ├── 02-gpu-compute-networks-research.md
│   ├── 03-hybrid-consensus-research.md
│   ├── 04-design-decisions-synthesis.md
│   └── design/
│       ├── 01-architecture-overview.md
│       ├── 02-gpu-attestation-protocol.md
│       ├── 03-compute-marketplace-detailed.md
│       └── 04-profitability-oracle-auto-switch.md
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

### Run GPU node agent
```bash
cd gpn-agent
cargo run -- config/default.toml
```

## Phase Status

- ✅ **Phase 1 (Research):** Complete
- 🏗️ **Phase 2 (Design):** In progress — architecture docs written, pallets scaffolded
- ⏳ **Phase 3 (Prototype):** Not started
- ⏳ **Phase 4 (Launch):** Not started
