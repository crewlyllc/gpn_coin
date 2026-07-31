# HEARTBEAT.md — Project 7: GPU-Native Crypto & AI Compute Network

**Status: Phase 3 (Prototype) — REAL ETHASH POW LIVE 🎉**
**Last Updated:** 2026-07-30 22:10 PT
**Caretaker:** PM07

---

## 🚀 Current Milestone (Cycle 8)

**Block authoring now uses REAL Ethash PoW** — the placeholder CPU blake2 hashing is gone. Blocks are sealed with a genuine 40-byte `nonce(8) ++ mix_hash(32)` Ethash solution and verified on-chain via `ethash::hashimoto_light` (light-cache mode, epoch 0, 16 MB cache). Same algorithm + seal layout a GPU miner produces with the full dataset — **GPU-mineable by construction.**

**Verified live (cycle 8):**
- `./target/release/gpn-node --dev --tmp --force-authoring` → mines + imports blocks #1 → #46+ using real Ethash
- ~1 block / ~20 s cadence = exactly the light-cache CPU rate at target 2^244 (P≈2.4e-4/hash) — proof the miner is running genuine Ethash, not instant blake2
- Seals present as `pow_` consensus digests (`0x05706f775f...`); `verify_seal` hard-requires 40-byte nonce+mix + recomputed matching mix_hash, so every import proves real Ethash verification passed
- Full detail: `shared/artifacts/project7/cycle8-ethash-pow-log.md`

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
| Pallet unit tests (65 across all 5) | ✅ |
| Wire pallet-gpu-pow difficulty adjustment into node `difficulty()` | 🔴 next |
| Real GPU (CUDA/OpenCL) Ethash miner in gpn-agent on RTX 4060 | 🔴 next |
| Fee mechanism (TransactionPayment) | 🟡 pending |
| gpn-agent subxt chain client (stub) | 🟡 pending |
| Two-node p2p testnet | 🟡 pending |

## Next Concrete Actions (priority order)
1. [ ] Wire `pallet-gpu-pow` difficulty adjustment into `GPNPowAlgorithm::difficulty()` so block time self-tunes (read stored difficulty via runtime API)
2. [ ] Real GPU Ethash mining on the RTX 4060 — CUDA/OpenCL miner in `gpn-agent` producing the same 40-byte seal, pointed at the dev node
3. [ ] Add a fee pallet before any non-dev network
4. [ ] Promote extrinsic test + PoW seal-format test into repo (`gpn-node/scripts/`)
5. [ ] Two-node local testnet (p2p + longest-chain)

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
