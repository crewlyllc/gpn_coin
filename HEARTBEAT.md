# HEARTBEAT.md — Project 7: GPU-Native Crypto & AI Compute Network

**Status: Phase 3 (Prototype) — LIVE END-TO-END GPU ATTESTATION ON RTX 4060 (Cycle 27): the agent now self-deposits `ProviderStake` and attests on-chain with a real measured GFLOPS. Verified live: Alice staked 1,000 GPN → measured 188.7 GFLOPS on the 4060 → on-chain `tier=Bronze` (gpus=1, compute_gflops=188667299223). Also fixed two signed-tx plumbing bugs that blocked all live attestations: (1) subxt read the account nonce from the stuck-finalized #0 block → second+ tx rejected as `Transaction is outdated` (now reads nonce from best-head block); (2) `wait_for_finalized_success` hangs forever on a PoW chain with no finality gadget (now waits for `InBestBlock` instead). Added `gpn_check_state` helper to read live stake/attestation state.**
**Last Updated:** 2026-07-31 ~18:40 PT
**Caretaker:** PM07

---

## 🚀 Current Milestone (Cycle 27)

**`GPUAttestation::deposit_stake` is wired into the agent and the full GPU-attestation leg is proven LIVE on the RTX 4060.**

1. **`ChainClient::deposit_stake(amount)`** — signed dynamic extrinsic (reuses shared `signer::Sr25519Signer` + `GpnConfig`); `perform_attestation` deposits `config.min_attestation_stake` (default = pallet `MinAttestationStake` = 1,000 GPN) before attesting. Non-fatal no-op if already staked.
2. **Signed-tx nonce fix** — `submit_signed` now takes an explicit head-block nonce (`ChainClient::get_account_nonce` / `signer::current_nonce` read `System::Account` nonce at best-head via `chain_getBlockHash` + `storage().at(hash)`). Fixes `Transaction is outdated` on second+ tx from the same account.
3. **Finality hang fix** — replaced `wait_for_finalized_success` (never fires on PoW, `finalized` stuck at #0) with an `InBestBlock` wait loop; stopped decoding block events (spurious `Serde json error` after inclusion).
4. **`gpn_check_state` binary** — reads live on-chain nonce/balance/staked/attested + full attestation record (gpu_count, compute_gflops, tier enum name).

**Verified live (RTX 4060, `gpn-node --dev`):**
```
account=5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY nonce=6 balance=996999999999.9994 GPN staked=true attested=true
attestation: gpus=1 gflops=188667299223 tier=Bronze
```

Commit `761c65f`. Detail: `shared/artifacts/project7/cycle27-attest-deposit-live-log.md`

---

## 🚀 Previous Milestone (Cycle 26)

**`min_gflops` is now enforced at `accept_job` via a clean cross-pallet trait — the marketplace can no longer match a job to a provider that can't actually deliver the required GPU throughput.**

1. **`gpu-attestation` exposes `GPUAttestationProvider<AccountId>`** — public read-only trait (`attested_gflops` / `attested_tier`), implemented for `Pallet<T>` by reading `Attestations` storage. Downstream pallets get a stable interface with no storage coupling.
2. **`compute-marketplace` consumes it** — new `Config::GPUAttestation: GPUAttestationProvider<AccountId>`; `accept_job` now rejects with new `Error::ProviderComputeInsufficient` when `provider.attested_gflops < job.gpu_requirements.min_gflops`. 0 GFLOPS = never attested → blocked from gated jobs. `min_gflops=0` → gate skipped (open market preserved).
3. **Runtime wired** — `type GPUAttestation = GPUAttestation` in the marketplace Config impl.
4. **4 new tests** (mock attestation via `thread_local` for race-free parallel isolation): below-min rejected / un-attested rejected / meets-requirement passes / zero-gate skipped.

**Verified:** compute-marketplace **21 passed / 0 failed** (was 17; +4); gpu-attestation **20 passed / 0 failed**; `cargo check --workspace` clean. Commit `8fd687a`. Detail: `shared/artifacts/project7/cycle26-gflops-accept-gate-log.md`

---

## 🚀 Previous Milestone (Cycle 25)

**The attestation RPC stub is gone — the agent now submits the real `GPUAttestation::submit_attestation` extrinsic on-chain.** Four pieces landed:

1. **New shared module `chain::signer`** — extracted the `gpn_seed_feed` plumbing (custom `GpnConfig` for the bare `AccountId32` address + `sp_core::sr25519::Pair` `Signer`) into a reusable library module: `connect()`, `Sr25519Signer::from_uri()`, `submit_signed()` (immortal tx, waits for finalized success).
2. **`gpn_seed_feed` refactored** to use the shared module (duplication removed).
3. **`ChainClient` can now submit signed extrinsics** — `.with_signer(uri)`; `get_attestation_challenge()` reads the real `GPUAttestation::AttestationChallenge` storage (new `composite_to_bytes` helper); `submit_attestation(...)` builds & submits the signed `GPUAttestation::submit_attestation` dynamic extrinsic.
4. **Config + agent wiring** — `AgentConfig.signer_uri` (default `//Alice`); agent `ChainClient` is built with `.with_signer(...)`, so `perform_attestation` pushes the attestation on-chain.

**Verified:** gpn-agent release build clean (all 5 bins); lib tests **15 passed / 0 failed / 7 GPU-gated ignored** (+2 new `composite_to_bytes` tests); pallets unchanged & green (attestation 20 / marketplace 17). Detail: `shared/artifacts/project7/cycle25-attestation-extrinsic-log.md`

**Caveat:** the extrinsic requires `ProviderStake` (pallet enforces `InsufficientStake` otherwise) — a provider must `deposit_stake` before attesting. Agent currently assumes stake exists; wiring deposit + a live RTX 4060 end-to-end attest is the next step.

---

## 🚀 Previous Milestone (Cycle 24)

**Measured compute throughput (GFLOPS) is now an on-chain attestable metric — the AI-compute leg is tied to the attestation + marketplace pallets.** Three pieces landed:

1. **`gpu-attestation` pallet** — `AttestationRecord` gains `compute_gflops: u64`; tiers are now gated by **proof of real compute** via `AttestationTier::min_gflops()` (Bronze≥100 / Silver≥1000 / Gold≥5000). `submit_attestation` takes `compute_gflops`; a GPU count without the throughput to back it is demoted and rejected with new `Error::InsufficientCompute`. GPU count is necessary but no longer sufficient.
2. **`compute-marketplace` pallet** — `GPURequirements` gains `min_gflops: u64` (Default 0), so requestors can require a minimum FLOP rate and match against attested providers.
3. **`gpn-agent`** — `gpu::measure_gflops()` runs a short dense-GEMM benchmark and `perform_attestation` passes the real number through `submit_attestation(result, gpu_info_hash, gpu_count, compute_gflops)`.

**Verified:** pallet tests **20 (attestation) + 17 (marketplace) passed, 0 failed** (3 new GFLOPS tests); runtime + agent compile clean; agent lib tests 13 passed / 7 GPU-gated ignored (baseline unchanged). Detail: `shared/artifacts/project7/cycle24-gflops-attestation-log.md`

---

## 🚀 Previous Milestone (Cycle 23)

**Two-node GPU p2p testnet documented + wired into CI.** Re-verified the priority-1 integration
live on the RTX 4060: `gpn_two_node_testnet.sh` → **6/6 passed** (both nodes GPU-mining real Ethash,
connected as peers, converged on best #7). Added a full README section (flags, stale-binary gotcha,
expected output) and `.github/workflows/ci.yml` — CPU build+test jobs on ubuntu-latest (GPU tests
`#[ignore]`d) + a `workflow_dispatch`-gated `two-node-gpu-testnet` job on a `[self-hosted, linux, gpu]`
runner (path to run the RTX 4060 box as a self-hosted runner). Toolchain pinned to **stable**
(workspace builds on rustc 1.97.x).

Detail: `shared/artifacts/project7/cycle23-two-node-testnet-ci-log.md`

---

## 🚀 Previous Milestone (Cycle 22)

**The last feed gap is closed — the `chain` profitability feed is now fully live.** Two pieces landed:

1. **`gpn_seed_feed` tool** — submits a *signed* `ProfitabilityOracle.update_feed` extrinsic as Alice (the genesis oracle operator in dev/local specs). Uses a custom `Signer` on `sp_core v28` (avoids the `substrate-compat`/`sp-core v31` version conflict) and a custom `GpnConfig` (bare `AccountId32` address — the GPN runtime uses `Address = AccountId`, not `MultiAddress`). Immortal tx → robust on fast-block dev chains.
2. **ChainClient head-block read fix** — subxt's `storage().at_latest()` reads the latest **finalized** block, which on a dev/single-node PoW chain (no finality gadget) stays stuck at #0 while the feed lives far ahead on the best fork. Added `head_block_hash()` (raw `chain_getBlockHash` via reqwest) + `storage_at_head()`, now used by `fetch_feed`, `fetch_difficulty`, and `get_account_info`.

**Verified live on the RTX 4060 host (`gpn-node --dev`):** seeded feed on-chain (`CurrentFeed` storage confirmed: token=75000 micro-USD / gpu=3000000 / util=7200 bps / apr=1500 bps). The live test now prints the **real** values:
```
[live] difficulty = 138793332.36        (real, self-tuned)
[live] token=$0.075 gpu=$3/hr util=0.72 apr=15%
```
Previously it printed fallbacks ($0.05 / $2.5 / 0.65 / 12%). Full suite 13 passed / 7 GPU-gated ignored; release build clean.

Detail: `shared/artifacts/project7/cycle22-feed-seed-live-log.md`

---

## 🚀 Previous Milestone (Cycle 21)

**The `ChainClient` is now real — the `chain` profitability feed reads actual on-chain data, not stubs.** Replaced the hardcoded placeholder client (100M difficulty, $0.05 token, etc.) with a subxt `OnlineClient<SubstrateConfig>` using **dynamic storage queries** (no codegen). It reads:
- `GPUPoW::Difficulty` — the pallet's little-endian `[u64;4]` PoW **target**, converted to a classic difficulty metric `2^256/target`.
- `ProfitabilityOracle::CurrentFeed` — real `ProfitabilityFeed` (token price / GPU-hour price / utilization / APR) from micro-USD + basis points.
- `System::Account` balance (best-effort).

**Verified live against a running `gpn-node --dev` (RTX 4060, blocks #0–#10):** `[live] difficulty = 2048` — real, derived from the chain's self-tuning target (2^245 → 2^11), NOT the 100M placeholder. Feed values fall back gracefully until an oracle operator seeds them. 3 new unit tests; suite 13 passed / 7 GPU-gated ignored; release build clean.

Detail: `shared/artifacts/project7/cycle21-subxt-chain-client-log.md`

---

## Previous Milestone (Cycle 20)

**The headline feature is now proven on real hardware end-to-end.** Ran `gpn-agent` in `auto` mode with `profitability_feed = "scripted"` against a live `gpn-node` (Alice, no internal miner, external-mining RPC on :9944). The scripted `demand_spike_scenario` drove exactly two switches, each genuinely starting/stopping the real GPU engines:

1. Boot → **Mining** (DAG built, real Ethash engine started) → mined & submitted blocks **#1–#8** (node imported all).
2. Compute demand spike → **Mining → Compute**: mining engine **stopped** (DAG released), real 2048³ GEMM AI workload **started** (~**915 GFLOPS** on RTX 4060).
3. Compute demand fades → **Compute → Mining**: compute stopped, mining engine **restarted** (DAG rebuilt) → mined & submitted blocks **#9–#17** (node imported all).

**Verified:** node imported 17 blocks (best #17), exactly 2 switches, mining & compute engines never overlapped. This is the product thesis (one GPU alternates between securing the network and serving AI compute based on real-time profitability, no human in loop) proven on hardware.

Detail: `shared/artifacts/project7/cycle20-autoswitch-live-verified-log.md` (config: `shared/artifacts/project7/autoswitch-demo.toml`)

---

## Previous Milestone (Cycle 19)

**`Compute` mode now actually runs a real AI workload on the GPU — the compute leg of the auto-switch is live.** `ModeManager` holds a real `ComputeExecutor` engine (symmetric to the `ExternalMiner` mining engine) that runs a dense float32 GEMM — the core primitive of neural inference — on the same GPU that mines Ethash. Switching to `Compute` **starts** the real workload; switching to `Mining` **stops** it and releases the GPU. This mirrors the mining integration (Cycle 17) exactly. (Docker isn't on this host and the old `DockerExecutor` was a dead stub — replaced with the working OpenCL engine; container orchestration is documented as the production layer on top.)

**Verified (cycle 19) on the RTX 4060:** GPU GEMM matches CPU reference; `mode_switch_starts_and_stops_compute` passes (Mining→Compute starts the real AI job, Compute→Mining stops it); mining leg regression passes; standalone `gpn_gpu_compute` delivers **830.63 GFLOPS** (2048³ matmul, 20.68 ms/iter). Full suite 10 passed / 6 GPU-gated ignored; release build clean.

Detail: `shared/artifacts/project7/cycle19-compute-engine-mode-manager-log.md`

---

## 🚀 Previous Milestone (Cycle 18)

**The auto-switch decision logic is now pure, unit-tested, and demonstrable — and `auto` mode can actually be driven by a profitability feed.** Extracted `decide_switch()` as a free function (testable without chain/GPU, with hysteresis anti-flapping), replaced the stub tests with 5 real ones, added a `ScriptedFeed` demand-spike timeline + `config.profitability_feed` ("chain"/"scripted") switch in the live `ProfitabilityCalculator`, and a headless `gpn_simulate_autoswitch` binary that prints the GPU mode timeline (Mining → Compute on spike → Mining on fade).

**Verified (cycle 18):** `decide_switch` prefers-highest / hysteresis no-op / flip-on-exceed / anti-flapping / all-mode-pairs all pass; scripted feed flips Mining→Compute→Mining (exactly 2 switches) at 5% hysteresis and stays put on small ±4% oscillation at 10%; live agent `analyze()` returns the scripted timeline when `profitability_feed = "scripted"`. Agent tests 9 passed, 4 GPU-gated ignored; release build clean.

Detail: `shared/artifacts/project7/cycle18-profitability-autoswitch-log.md` (commit dcfc7e7)

---

## 🚀 Previous Milestone (Cycle 17)

**The headline auto-switch feature now has a real substrate.** The agent's `ModeManager` previously only *logged* mode transitions. Now switching to `Mining` mode **actually starts the GPU mining loop** (spawns the `ExternalMiner` engine — builds DAG, polls `gpn_getTemplate`, mines, submits via `gpn_submitWork`), and switching to `Compute`/`StakeOnly` **stops it and releases the GPU**. The mining loop was extracted from the `gpn_external_miner` binary into a reusable, stoppable `ExternalMiner` engine (`gpn-agent/src/mining/mod.rs`) running on a dedicated OS thread.

**Verified live on the RTX 4060:** agent in `mine` mode → `ModeManager.boot()` started the engine → mined blocks #1–#12 over RPC, node imported each. New GPU-gated test `mode_switch_starts_and_stops_miner` proves the auto-switch core: Mining→Compute **stops** the engine, Compute→Mining **restarts** it. Also fixed two real bugs: (1) boot never launched mining (same-mode short-circuit — added `boot()`); (2) `switch_to` never persisted `current_mode` to state (would break repeated auto-switches).

**Verified (cycle 17):**
- `ExternalMiner::start`/`stop` — engine spawns & cleanly cancels, releasing the DAG from VRAM.
- `ModeManager` switches Mining→Compute→Mining, `is_mining()` toggles correctly (GPU-gated test, ✅).
- Agent `mine` mode mined blocks #1–#12 through `ModeManager` (RPC path), node imported all.
- Config: `mining_rpc_url`/`mining_device_index` added; `#[serde(default)]` on all fields (fixed flat-struct vs `[node]`-section TOML crash); `default.toml` rewritten flat.
- `cargo test` → 4 passed, GPU tests `#[ignore]`d. Release build clean.
- Detail: `shared/artifacts/project7/cycle17-autoswitch-mode-manager-log.md`

---

## 🚀 Current Milestone (Cycle 16)

**The `gpn-agent` GPU miner is wired to the external-mining RPC and verified end-to-end on the RTX 4060.** The new `gpn_external_miner` binary fetches a block template via `gpn_getTemplate`, mines on its own GPU (OpenCL), and submits the seal via `gpn_submitWork` — all over plain HTTP JSON-RPC, with **no full node required on the miner machine**. A single node (no internal miner) + the external GPU miner produced blocks #0–#9, all imported and confirmed via `chain_getHeader` (best = #9). The stale-template negative path was exercised and handled correctly.

This **completes the production mining architecture**: node and miner are fully separable, and mining is now just an HTTP call. That is the substrate for the headline **GPU auto-switch** feature — the GPU can alternate between serving AI compute and mining by starting/stopping this loop.

**Verified (cycle 15):**
- `gpn_getTemplate` returns `{best_hash, pre_hash, difficulty, block_number}` — confirmed live.
- Reference CPU miner (`gpn_mine_seal`) mined a valid Ethash seal → `gpn_submitWork` returned `true` → node logged `✅ Successfully mined block` + `🏆 Imported #1`; `chain_getHeader` shows the submitted seal in the digest.
- Negative paths: stale pre_hash → `false`; invalid hex → clean RPC error.
- `cargo test -p gpn-node` → **5/5 passed** (seal verify suite), no regression. Release build clean.
- Detail: `shared/artifacts/project7/cycle15-external-miner-rpc-log.md`

**Key finding:** a node should run EITHER the internal self-miner OR serve external miners, not both — the self-miner consumes every build within ms, so external miners see `None`. Production design: mining node exposes RPC (no internal miner); compute node runs the agent.

---

## Build Progress (Phase 3)

| Component | Status |
|-----------|--------|
| 5 pallets (gpu-pow, gpu-attestation, compute-marketplace, gpn-token, profitability-oracle) | ✅ compile |
| gpn-runtime | ✅ compiles |
| gpn-node binary | ✅ builds & runs |
| Dev testnet producing blocks (cycle 7) | ✅ |
| End-to-end signed extrinsic (cycle 7) | ✅ |
| **Real Ethash PoW in consensus (cycle 8)** | ✅ **LIVE** |
| **Self-tuning difficulty from pallet (cycle 9)** | ✅ **LIVE** |
| **Real GPU (OpenCL) Ethash miner in gpn-agent on RTX 4060 (cycle 10)** | ✅ **LIVE** |
| **GPU miner wired into live node (cycle 11)** | ✅ **LIVE** |
| **PoW seal-format + extrinsic tests in repo (cycle 12)** | ✅ **LIVE** |
| **Two-node GPU p2p testnet (cycle 13)** | ✅ **LIVE** |
| Pallet unit tests (81 across all 5) | ✅ |
| GPU miner validation tests (DAG, trace-diff, matches-reference) | ✅ |
| **Fee mechanism (TransactionPayment) (cycle 14)** | ✅ **LIVE** |
| **External miner RPC — `gpn_getTemplate`/`gpn_submitWork` (cycle 15)** | ✅ **LIVE** |
| **External GPU miner wired to RPC — `gpn_external_miner` (cycle 16)** | ✅ **LIVE** |
| **Stoppable mining engine `ExternalMiner` + ModeManager start/stop (cycle 17)** | ✅ **LIVE** |
| **Auto-switch start/stop proven (Mining↔Compute) (cycle 17)** | ✅ **LIVE** |
| **Auto-switch DECISION pure + tested; scripted feed + headless sim (cycle 18)** | ✅ **LIVE** |
| **Compute engine folded into ModeManager — `Compute` runs real GPU AI GEMM (cycle 19)** | ✅ **LIVE** |
| **Auto-switch BOTH legs proven on RTX 4060: mining & compute engines start/stop (cycle 19)** | ✅ **LIVE** |
| **END-TO-END auto-switch: scripted feed drives Mining↔Compute on RTX 4060, node imported 17 GPU-mined blocks (cycle 20)** | ✅ **LIVE** |
| **Real subxt ChainClient: on-chain difficulty + profitability feed read live (cycle 21)** | ✅ **LIVE** |
| **LIVE on-chain profitability feed: `gpn_seed_feed` signs `update_feed` + ChainClient head-block read (cycle 22)** | ✅ **LIVE** |
| **Measured GFLOPS = on-chain attestable metric: attestation tier gated by proof-of-compute, `min_gflops` in marketplace, agent reports real GFLOPS (cycle 24)** | ✅ **LIVE (pallet+agent)** |
| **Agent submits REAL `GPUAttestation::submit_attestation` extrinsic on-chain (shared signer module; challenge read; `with_signer` wiring) (cycle 25)** | ✅ **LIVE (agent)** |
| **`min_gflops` ENFORCED at `accept_job` — cross-pallet `GPUAttestationProvider` trait gates providers on attested GFLOPS (cycle 26)** | ✅ **LIVE (pallet+runtime)** |
| **Live end-to-end attest on RTX 4060: self-deposit `ProviderStake` → measure 188 GFLOPS → on-chain `tier=Bronze`; signed-tx nonce & finality-hang fixes (cycle 27)** | ✅ **LIVE** |
| gpn-agent subxt chain client (stub) | ✅ **REPLACED (cycle 21)** |
| Wire real `GPUAttestation::submit_attestation` extrinsic (reuse gpn_seed_feed Signer) | ✅ **DONE (cycle 25)** |
| Enforce `min_gflops` matching at `accept_job` (cross-pallet attested read) | ✅ **DONE (cycle 26)** |

## Next Concrete Actions (priority order)
1. [x] Two-node testnet docs in README + CI wiring for `gpn-node/scripts/gpn_two_node_testnet.sh` (cycle 23, 6/6 live).
2. [x] Compute marketplace integration: surface `ComputeExecutor` throughput/GFLOPS as an on-chain attestable metric (cycle 24 — attestation tier gated by GFLOPS + `min_gflops` in marketplace + agent measures/reports).
3. [x] Wire the real `GPUAttestation::submit_attestation` extrinsic in the agent by reusing the `gpn_seed_feed` custom Signer + `GpnConfig` plumbing (cycle 25 — shared `chain::signer` module; challenge read; `with_signer` wiring).
4. [x] Enforce `min_gflops` matching at `accept_job` — cross-pallet read of the provider's attested `compute_gflops` (trait from gpu-attestation into compute-marketplace) (cycle 26).
5. [x] Live end-to-end attest on the RTX 4060: deposit stake (pallet requires `ProviderStake`), measure ~830 GFLOPS, submit, confirm tier on-chain (cycle 27 — staked 1,000 GPN, attested **Bronze** @ 188 GFLOPS; fixed signed-tx nonce + finality-hang bugs).
6. [ ] Live end-to-end marketplace flow on the 4060: attest (~188 GFLOPS) → register provider → create job with `min_gflops ≤ 188` → accept (gated by cycle-26 trait) → run real GEMM → submit result → complete + payout. Check compute-marketplace extrinsic names + wire a provider-registration/job-lifecycle helper (mirror `gpn_check_state`).

## Phase 1 Findings

### 1. GPU-Mineable Coins — Detailed Analysis

#### Kaspa (KAS)
- **Algorithm:** kHeavyHash — a modified version of HeavyHash (originally developed for HPB). It's a memory-light PoW designed to be ASIC-friendly (ASICs already exist).
- **Consensus:** PHANTOM/ GhostDAG — a blockDAG, not a blockchain. Allows parallel blocks, sub-second block times (1 BPS, upgraded to 10 BPS in May 2025 Crescendo hardfork). DAG-scale with GHOSTDAG ordering.
- **ASIC Resistance:** LOW. kHeavyHash was designed with ASICs in mind. ASICs from IceRiver and others dominate Kaspa mining (KHeavyHash ASICs). Not GPU-friendly in practice despite initial GPU mining support.
- **Tokenomics:** Fair launch (no premine, no presale). 28.7B max supply. Disinflationary emission (block reward halves monthly, not by block count). Currently ~25B mined.
- **Written In:** Go (original, now deprecated), Rust (current reference: rusty-kaspa full node)
- **Key Innovation:** BlockDAG architecture achieving high throughput with PoW security. Instant confirmations.
- **Relevance to Project 7:** BlockDAG is interesting for scalability but kHeavyHash is ASIC-dominated — contradicts our GPU-only requirement.

#### Ergo (ERG)
- **Algorithm:** Autolykos v2 — memory-hard PoW based on Equihash-inspired design. Requires ~4GB VRAM minimum. Uses memory-hard hashing to resist ASICs by requiring significant memory bandwidth.
- **Consensus:** Proof-of-Work (Nakamoto consensus modified). Smart contracts via eUTXO model (extended UTXO similar to Cardano).
- **ASIC Resistance:** GOOD. Autolykos v2 is memory-hard and specifically designed for GPU mining. ASIC development is economically unviable due to memory requirements.
- **Tokenomics:** 97.7M max supply. No premine. Storage rent fee model (demurrage) — part of UTXOs not spent for 4+ years are reclaimed. Emission halves every 3 months (approx). ~67M mined as of 2025.
- **Written In:** Scala (reference client), with Rust components (sigma-rust for ErgoScript)
- **Key Innovation:** Storage rent fee model, eUTXO with ErgoScript (powerful smart contracts), NiPoPoWs (light-client proofs), Sigma protocols for zero-knowledge.
- **Relevance to Project 7:** Strongest candidate. GPU-only, ASIC-resistant, mature smart contract platform. The eUTXO model and NiPoPoWs are directly applicable. Storage rent is innovative.

#### Ravencoin (RVN)
- **Algorithm:** KawPow (derived from ProgPow — Programmatic Proof-of-Work). Changes the block hash computation every block using random programs to break ASIC efficiency.
- **Consensus:** Proof-of-Work (Bitcoin codebase fork). Focused on asset transfer (not general smart contracts).
- **ASIC Resistance:** HIGH (in theory). KawPow's random-program-per-block model makes ASIC design extremely difficult. However, some KawPow ASICs have emerged. ~4GB VRAM minimum for DAG.
- **Tokenomics:** 21B max supply. No premine. Block reward halves every 4 years (like Bitcoin). ~12B mined as of 2025.
- **Written In:** C++ (Bitcoin Core fork)
- **Key Innovation:** Asset issuance protocol built into the chain (not smart contracts — native assets). Asset transfer focus.
- **Relevance to Project 7:** KawPow is a viable GPU-only mining algorithm. However, RVN lacks smart contract capability. Asset-only focus limits compute marketplace viability.

#### Ethereum Classic (ETC)
- **Algorithm:** Ethash — ASIC-resistant memory-hard PoW used by Ethereum pre-merge. DAG size ~5GB. Requires GPU with >=5GB VRAM.
- **Consensus:** PoW (Nakamoto consensus), full EVM smart contracts.
- **ASIC Resistance:** MODERATE. Ethash was initially GPU-only, but Ethash ASICs (E3, etc.) exist. Growing ASIC dominance. ETC has ~5K nodes.
- **Tokenomics:** Fixed supply of 210.7M ETC (modeled after Bitcoin's scarcity). No premine. Block reward decreases via "5M20" era system (every 5M blocks, reward reduces by 20%).
- **Written In:** Multiple (Go, Rust, Python, C++, Scala)
- **Key Innovation:** Largest PoW smart contract platform after ETH merge. Full EVM compatibility.
- **Relevance to Project 7:** EVM compatibility is massive for tooling/ecosystem. But Ethash ASICs exist, contradicting GPU-only requirement. If we fork ETC we get Solidity/EVM out of the box.

#### Comparison Table (GPU Mining)

| Feature | Kaspa | Ergo | Ravencoin | ETC |
|---|---|---|---|---|
| Algorithm | kHeavyHash | Autolykos v2 | KawPow | Ethash |
| ASIC Resistant | ❌ No | ✅ Yes | ✅ Mostly | ⚠️ Moderate |
| GPU-Only Feasible | ❌ | ✅ | ⚠️ Some ASICs | ⚠️ Some ASICs |
| Smart Contracts | ❌ No | ✅ eUTXO | ❌ Asset only | ✅ EVM |
| Consensus | DAG-PoW | PoW | PoW | PoW |
| Written In | Rust | Scala/Rust | C++ | Go/Rust/Python |
| Min VRAM | ~1GB | ~4GB | ~4GB | ~5GB |
| Token Supply | 28.7B | 97.7M | 21B | 210.7M |

---

### 2. Decentralized GPU Compute Networks — Detailed Analysis

#### io.net
- **Type:** Aggregated GPU marketplace (decentralized physical infrastructure network — DePIN)
- **Architecture:** Cloud + edge — aggregates GPUs from data centers (Tier 2/3/4) and individual providers. Focuses on AI inference/training.
- **Token:** IO (Solana-based) — used for payments and staking. ~800M max supply.
- **Pricing:** Dynamic — providers set prices, IO algorithm recommends. Currently ~$1-3/hr for H100s.
- **GPU Matching:** Order-book style on Solana. Customers create orders, providers fulfill.
- **Key Features:** IO Worker (provider CLI), IO SDK for devs. Supports major ML frameworks.
- **Weaknesses:** Centralized control surfaces in matching. Quality of service varies by provider. Heavy dependency on Solana.

#### Akash Network (AKT)
- **Type:** Decentralized cloud marketplace (built on Cosmos SDK)
- **Architecture:** Reverse auction — providers bid on deployment orders. GPU support added in 2023 (Akash + GPUs).
- **Token:** AKT (Cosmos IBC-compatible) — used for staking, governance, fees.
- **Pricing:** Reverse auction — lowest bid wins. GPU pricing ~$0.50-1.50/hr for mid-range GPUs. Significantly cheaper than AWS.
- **GPU Matching:** Tenant posts deployment specs (cpu, ram, gpu model), providers bid. Lease won by lowest bidder. Time-bound leases.
- **Key Features:** Cosmos SDK IBC interoperability, Tendermint consensus. Audited provider attributes. GPU verification.
- **Weaknesses:** Reverse auction favors lowest price over quality. Limited GPU availability. No built-in staking delegation for GPU providers.

#### Render Network (RNDR)
- **Type:** Decentralized GPU rendering network (OctaneRender, Blender Cycles, Redshift)
- **Architecture:** Creator submits render job → nodes compete for work → nodes render → Creator approves → payment released.
- **Token:** RNDR → RENDER (migrated to Solana). Used for payment and staking.
- **Pricing:** Job-based pricing. Creators set budget, nodes accept. RNDR priced per Ob (OctaneBench) — a GPU rendering benchmark unit.
- **GPU Matching:** OctaneBench-based reputation system. Nodes with higher OB score and uptime get prioritized.
- **Key Features:** OctaneBench as GPU "score" (reputation). Escrow-based payment. Focused on rendering (not general AI compute).
- **Weaknesses:** Specialized for rendering/AI imaging only (Octane/Redshift/Cycles). Not general-purpose compute. Centralized elements in job dispatch.

#### Golem Network (GLM)
- **Type:** Decentralized compute marketplace (oldest in space, ~2018)
- **Architecture:** Yagna (v2) — requestor/provider model. Requestor defines task template, providers execute in sandboxed environments.
- **Token:** GLM (Ethereum-based, migrated from GNT). Used for payments.
- **Pricing:** Requestor-defined budget. Providers set prices in GLM/hr.
- **GPU Matching:** Limited GPU support — primarily CPU-based compute historically.
- **Key Features:** WASM-based task execution. Provider reputation.
- **Weaknesses:** Limited GPU adoption. Small network effect. Complex setup for GPU workloads. Unclear GPU compute roadmap.

#### Spheron Network
- **Type:** Enterprise GPU rental marketplace (centralized/Web2-style)
- **Architecture:** Aggregates enterprise data center GPU supply. Not fully decentralized — centralized marketplace.
- **Token:** None (fiat/stablecoin payments). Not crypto-native.
- **Pricing:** Live market pricing based on real-time availability. Per-minute billing.
- **GPU Matching:** Search/browse/cart model (like AWS Marketplace). No on-chain matching.
- **Weaknesses:** Centralized. No token. Not a blockchain project. No mining/staking. More of a GPU aggregator than a crypto network.

---

### 3. Hybrid Consensus & "Proof of Useful Work" — Analysis

#### Primecoin (XPM)
- **Concept:** PoW based on finding Cunningham chains and bi-twin prime number chains. "Useful" mathematical computation.
- **Status:** Active but tiny. Largely abandoned by development community.
- **Relevance:** The original "useful PoW" — but prime finding has limited real-world value. Not GPU-friendly (CPU-only mining due to prime search characteristics).

#### Curecoin (CURE)
- **Concept:** PoW + Folding@home integration. Mining requires running Folding@home protein folding work units. Folding score determines block rewards.
- **Status:** Low adoption. POS sidechain. Small market cap.
- **Relevance:** First crypto to directly reward distributed computation. But protein folding has specialized requirements, not general GPU compute.

#### Gridcoin (GRC)
- **Concept:** PoS-based with BOINC integration. Stake weight increases with BOINC (Berkeley Open Infrastructure for Network Computing) research contribution.
- **Status:** Active community, low market cap. Uses PoS for network security + "Proof of Research" via BOINC credits.
- **Relevance:** Most mature "proof of useful work" implementation. BOINC integration works well for scientific computing. But BOINC tasks are pre-defined scientific projects — not on-demand AI compute.

#### Key Takeaway on Proof of Useful Work:
"Useful PoW" projects have struggled with:
1. **Market failure** — useful computation is worth more than block rewards, creating perverse incentives
2. **Verification problem** — verifying arbitrary useful work is computationally expensive (you need to re-run or use ZK)
3. **Demand matching** — scientific/BOINC work has no paying customer; it's subsidized by inflation
4. **GPU utilization** — many useful tasks are CPU-bound

---

### 4. Critical Assessment: What's Missing

After surveying the landscape, these are the gaps our project could fill:

#### Gap 1: No chain natively integrates GPU mining + GPU compute marketplace
- Existing GPU chains (Ergo, Ravencoin, ETC) don't have compute marketplaces
- Existing compute networks (io.net, Akash, Render) don't have native GPU mining
- **Our opportunity:** A chain where the same GPU hardware can seamlessly switch between securing the network (mining) and serving customers (compute)

#### Gap 2: GPU attestation for staking doesn't exist
- No chain bonds staking rewards to verifiable GPU compute activity
- i.e., "I stake my tokens AND prove I'm running real GPU compute" → higher staking rewards
- **Our opportunity:** Design a proof-of-GPU-compute mechanism for staking multipliers

#### Gap 3: Profitability-aware GPU resource switching is novel
- No existing system automatically switches GPU between crypto mining and AI compute based on real-time profitability
- io.net doesn't mine crypto. Mining pools don't sell compute.
- **Our opportunity:** Build the profitability oracle + switching fabric

#### Gap 4: Decentralized GPU compute lacks quality-of-service mechanisms
- Akash's reverse auction leads to race-to-bottom pricing
- io.net has variable quality with no on-chain reputation
- **Our opportunity:** GPU attestation + reputation scoring built into the protocol

---

## Decisions & Recommendations (Phase 2 Starting Point)

### Best Chain Strategy
**Recommended: Build on Ergo's Autolykos v2 model as inspiration, but write custom code in Rust**

Rationale:
- Autolykos v2 provides the best GPU-only mining algorithm with strong ASIC resistance
- Rust ecosystem is ideal for blockchain development (Kaspa and Ergo both moving to Rust)
- eUTXO model enables complex smart contracts (compute marketplace, staking, profitability oracle)
- Ergo's NiPoPoWs and stateless clients provide light-client infrastructure for GPU node verification

Alternative consideration: **Substrate framework** (Polkadot SDK) for modular blockchain construction — provides pluggable consensus, EVM pallets, and built-in staking.

### Mining Algorithm Recommendation
**Ethash-compatible custom algorithm** — GPU miners already have Ethash software. By building an Ethash-compatible algorithm:
- Support existing GPU mining software (TeamRedMiner, lolMiner, etc.)
- Require 4GB+ VRAM (kills ASIC efficiency)
- Add memory-hard tweaks to prevent ASIC optimization
- Consider ProgPow-style periodic algorithm changes

### Staking Mechanism
**Hybrid PoS + GPU Attestation:**
- Traditional delegated staking for network validation
- GPU compute proof boosts staking rewards (multiplier of 1x-3x based on verified compute)
- GPU attestation via periodic zero-knowledge proofs of model inference / hash computation
- Node must prove it ran X hash iterations or Y model inferences in a time window

### Compute Marketplace Design
**Dutch auction + reputation system:**
- GPU providers list capabilities (model, VRAM, bandwidth) and starting price
- Price decreases until a customer accepts
- On-chain reputation from completed jobs and customer ratings
- Payment escrow (like Render's model) for customer protection
- Auto-switch trigger: when compute orders are below price floor → switch to mining mode

### What's Next (Phase 2 — Design)
1. Formalize tokenomics model (supply, emission, inflation)
2. Design the GPU attestation protocol (stake.compute proof)
3. Design the profitability oracle architecture
4. Choose the blockchain framework (custom Rust vs Substrate vs Cosmos SDK)
5. Write a technical spec document

---

## Key Learnings
- **Ergo's approach** is closest to what we need: GPU-only, ASIC-resistant, smart contracts, sustainable economics
- **Kaspa's blockDAG** could solve throughput issues but the algorithm is ASIC-dominated
- **Compute marketplace** needs reputation, escrow, and quality metrics — not just a price auction
- **Auto-switching** is genuinely novel — nothing in the space does this yet
- **GPU attestation** for staking is also novel — no existing chain links staking weight to verified GPU compute

## Build Progress (Phase 3) — LEGACY (superseded by table at top)

| Component | Status |
|-----------|--------|
| 5 pallets (gpu-pow, gpu-attestation, compute-marketplace, gpn-token, profitability-oracle) | ✅ compile |
| gpn-runtime | ✅ compiles |
| gpn-node binary | ✅ builds & runs |
| **Dev testnet producing blocks** | ✅ **LIVE (cycle 7)** |
| End-to-end signed extrinsic | ✅ verified (cycle 7) |
| **Real Ethash PoW in consensus** | ✅ **LIVE (cycle 8)** |
| Unit tests for all 5 pallets (65 total) | ✅ |
| Fee mechanism (no TransactionPayment pallet → 0 fees) | 🟡 pending |
| gpn-agent subxt chain client (stub) | 🟡 pending |
| Two-node p2p testnet | 🟡 pending |

*See top of file for the current milestone and priority list (cycle 8: difficulty-adjustment wiring + real GPU miner next).*

*Progress: Phase 1 (research) ✅, Phase 2 (design) ✅, Phase 3 (prototype) — dev testnet live with real Ethash PoW; difficulty self-tuning + GPU miner next.*
