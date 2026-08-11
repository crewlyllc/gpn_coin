# GPN — Annotated File Map

> One-line description of every source file in the GPU-Native Network project.
> Generated 2026-08-10. 49 Rust files + 1 HTML dapp + design docs.

---

## 🚀 Quickstart — run the whole stack

These assume a built node + agent (see `gpn-node/README.md`). Run each in its own terminal.

### 1 · Start the blockchain node (mines blocks)
```bash
cd gpn-node
export PATH="$HOME/.cargo/bin:$PATH"
./target/release/gpn-node --dev --tmp --force-authoring
# RPC:  ws://127.0.0.1:9944   ·   metrics: :9615
```

### 2 · Start the faucet (gives out test GPN)
```bash
cd gpn-agent
./target/release/gpn_faucet --endpoint ws://127.0.0.1:9944 \
  --listen 127.0.0.1:8081 --signer-uri //Alice
#  http://127.0.0.1:8081
```

### 3 · Serve the customer dapp
```bash
cd ..   # to projectmanager07
python3 -m http.server 8080 --directory dapp
#  open  http://127.0.0.1:8080/gpn-rent.html
```

### 4 · Try it
- Browser → **http://127.0.0.1:8080/gpn-rent.html** (needs the Polkadot.js wallet extension)
- **Connect Wallet** → **Get test GPN from faucet** → balance updates → pick a GPU → **Create Job**.
- Watch the chain advance: `curl -s -X POST http://127.0.0.1:9944 -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","id":1,"method":"chain_getHeader","params":[]}'`

### 5 · Mine with the GPU (optional)
The dev node already mines automatically in step 1 (`--force-authoring`).
To mine with a real GPU instead (e.g. on a separate machine), run the external miner:
```bash
cd gpn-agent
./target/release/gpn_external_miner http://127.0.0.1:9944 0
# usage: gpn_external_miner [rpc_url] [device_index] [batch] [poll_ms]
```

### Notes
- First build needs `rustup target add wasm32-unknown-unknown` + `rustup component add rust-src`.
- The `--dev --tmp` node resets its data each restart (throwaway testnet).
- To launch the **public testnet** instead, see `testnet-public-runbook.md` (§7) — needs your go/no-go + a ~$10–20/mo VPS.

---

## 🧱 gpn-node/ — the blockchain itself (Substrate)

### Workspace root
| File | What it does |
|---|---|
| `Cargo.toml` | Workspace manifest; pins the Polkadot SDK to a fixed commit for reproducible builds |
| `README.md` | Build / run / test instructions |

### node/ — the network node binary
| File | What it does |
|---|---|
| `src/main.rs` | Program entry point — launches the CLI |
| `src/cli.rs` | Command-line argument parsing (dev/local flags, subcommands) |
| `src/command.rs` | Routes commands (build-spec, run node, key management, etc.) |
| `src/chain_spec.rs` | Chain definition — genesis state, pre-funded dev accounts, token settings |
| `src/service.rs` | Boots the node: networking, PoW consensus, mining worker, RPC server |
| `src/rpc.rs` | Extra RPC endpoints exposed by the node |
| `src/gpu_miner/mod.rs` | GPU mining driver hooked into the node's PoW |
| `src/bin/gpn_mine_seal.rs` | Standalone helper to mine a PoW seal for a block |

### runtime/ — wires the pallets into one chain
| File | What it does |
|---|---|
| `build.rs` | Compiles the chain logic into WebAssembly (so the runtime is portable) |
| `src/lib.rs` | Glues all 5 pallets + core system pallets into a single runtime |

### pallets/ — the 5 business modules
| File | What it does |
|---|---|
| `gpu-pow/src/lib.rs` | GPU PoW mining: difficulty, block rewards, halving, solution verification |
| `gpu-attestation/src/lib.rs` | Proof-of-Staked-GPU: stake GPN + prove real GFLOPS → tier multiplier |
| `compute-marketplace/src/lib.rs` | GPU rental: job lifecycle, escrow, 5% fee, proof-of-compute gate |
| `profitability-oracle/src/lib.rs` | Price/APR/utilization feed; drives mining↔compute auto-switching |
| `gpn-token/src/lib.rs` | The GPN coin: inflation schedule, fee distribution, treasury, pools |

---

## 🛠 gpn-agent/ — tools & libraries that run against the chain

### Core libraries
| File | What it does |
|---|---|
| `src/lib.rs` | Library root — exposes all modules |
| `src/main.rs` | Agent CLI entry point |
| `src/config/mod.rs` | Config loading (endpoints, keys, GPU settings) |
| `src/chain/signer.rs` | Wallet: builds + signs transactions with a private key |
| `src/chain/mod.rs` | RPC plumbing — talks to the node |
| `src/gpu/ethash_miner.rs` | Real GPU Ethash mining implementation (OpenCL) |
| `src/gpu/matmul.rs` | GPU matrix-multiply (a real compute job the GPU can run) |
| `src/gpu/mod.rs` | GPU monitoring + dispatch (mine vs compute) |
| `src/compute/executor.rs` | Runs AI/ML compute jobs on the GPU |
| `src/compute/mod.rs` | Compute job orchestration |
| `src/mining/mod.rs` | Mining loop / block sealing |
| `src/modes/mod.rs` | Node operating modes (miner / compute / auto-switch) |
| `src/profitability/scripted.rs` | Calculates which option pays better |
| `src/profitability/mod.rs` | Profitability decision logic |
| `src/utils/mod.rs` | Shared helpers |

### Executables (`src/bin/`) — 19 tools
| File | What it does |
|---|---|
| `gpn_gpu_mine.rs` | Mine GPN with the GPU |
| `gpn_external_miner.rs` | External GPU miner (separate machine) |
| `gpn_faucet.rs` | HTTP faucet that drips test GPN (used by the dapp) |
| `gpn_create_job.rs` | Post a GPU job to the marketplace from CLI |
| `gpn_marketplace_demo.rs` | End-to-end marketplace demo |
| `gpn_two_account_demo.rs` | Two-account escrow/payout demo |
| `gpn_simulate_autoswitch.rs` | Simulate mining↔compute auto-switching |
| `gpn_explorer.rs` | Block explorer web view |
| `gpn_operate_oracle.rs` | Run an oracle operator |
| `gpn_seed_feed.rs` | Seed initial oracle price data |
| `gpn_aggregate_feed.rs` | Aggregate oracle feeds |
| `gpn_effective_feed.rs` | Read the effective on-chain feed |
| `gpn_check_feed.rs` | Check oracle feed health |
| `gpn_committee_probe.rs` | Probe committee voting |
| `gpn_phase_c_probe.rs` | Probe Phase-C oracle features |
| `gpn_distribute_rewards.rs` | Distribute staking rewards |
| `gpn_verify_dispatch.rs` | Verify a transaction actually dispatched |
| `gpn_check_state.rs` | Inspect chain state |
| `gpn_gpu_compute.rs` | Run a GPU compute job directly |

---

## 🌐 dapp/ — the customer web interface
| File | What it does |
|---|---|
| `gpn-rent.html` | Browser app: connect wallet, get GPN from faucet, pick GPU (27 models), create a rental job |

---

## 📚 Design docs & cycle logs
| File | What it does |
|---|---|
| `design/node-agent` | Node ↔ agent architecture |
| `design/pallets` | Pallet design notes |
| `design/protocols` | Protocol specs (mining, oracle) |
| `design/tokenomics` | Tokenomics design |
| `shared/artifacts/project7/design-tokenomics-spec.md` | Full tokenomics spec |
| `shared/artifacts/project7/testnet-public-runbook.md` | Public testnet launch playbook |
| `shared/artifacts/project7/cycle63-decision-card.md` | The 5 launch decisions + VPS ask |
| `shared/artifacts/project7/operator-onboarding.md` | How to become a network operator |
| `shared/artifacts/project7/marketplace-sla-reputation-proposal.md` | SLA/reputation design |
| `shared/artifacts/project7/cycle##-…-log.md` | 19 milestone logs (every build step) |
