# GPN — A GPU-Native Network for Mining and AI Compute

**Whitepaper v0.9 (draft)** · GPN Network · 2026-10-09

---

## Abstract

GPN is a **GPU-native blockchain network** that turns a single GPU into an earning machine two ways: it can **mine** the network's proof-of-work token, or it can **serve AI compute** to paying users — and it **auto-switches** between the two based on which pays more. Unlike GPU rental markets and mining pools, which force an owner to pick one and lock into it, GPN watches both markets continuously and moves the GPU to the more profitable one.

The chain does **coordination, escrow, and settlement**; the heavy compute runs **off-chain**. Rewards are **Bitcoin-style**: the node that produces a block keeps that block's reward. Eligibility is **GPU-only**, enforced by proof-of-work and hardware attestation. AI jobs are priced per token against a cost-derived rate, escrowed on-chain (optionally off-chain via payment channels for zero block delay), and routed to the **nearest, least-loaded** verified provider.

GPN's thesis: the durable value of a commodity GPU is not mining, but **serving real AI demand** — and a network that lets every GPU do both, without friction, captures more of that value than either market alone.

---

## 1. Introduction

### 1.1 The problem
Owning a GPU presents two ways to earn, and they have always been **different products**:

1. **Mine** a proof-of-work coin — you're paid whatever the coin is worth today.
2. **Rent** the GPU for AI work (inference, image/video generation, fine-tuning) — you're paid whatever compute is worth today.

The catch: **you have to pick one.** Point the card at mining, and if the coin dips you mine at a loss. List it on a compute marketplace, and if nobody rents it, it earns $0. The GPU can't see that "the other side is paying more right now" and move.

Meanwhile, AI compute is expensive and concentrated: buyers pay hyperscaler prices, and consumer GPUs (the cheapest compute on earth, ~$0.14–0.34/hr on spot markets) sit idle.

### 1.2 The idea
**Don't make the owner pick.** GPN is a network where each GPU **mines _or_ serves AI**, **auto-switching** to whichever earns more, and where AI buyers get a single, simple interface that routes their work to that same pool of GPUs — coordinated and settled by a blockchain that no single party controls.

---

## 2. System Overview

A GPN node is **one entity** — one identity, one wallet, one GPU — that operates in **one of three modes** at a time:

| Mode | What the GPU does | Earns |
|---|---|---|
| **Mining** | Hashes GPN blocks (Ethash-derived GPU PoW) | Newly-minted GPN (block reward) |
| **Compute** | Serves AI jobs (LLM, image/video, fine-tune) | GPN from job escrow (paid by requestors) |
| **Staking** | *(later)* staked-provider rewards | Staking APR |

At launch the practical switch is **Mining ⇄ Compute**.

**The switch is a decision, not a guess.** Every cycle the node estimates each mode's revenue in **USD/hr** — using an on-chain oracle feed of token price, GPU-hour rental price, utilization, and staking APR — and switches **only when the other mode beats the current one by a hysteresis margin (~5%)**, so the GPU doesn't thrash on tiny price crossings. A switch is a real **stop/start of GPU work**: entering Compute stops the miner and frees VRAM before the model loads.

**The chain is thin and trust-minimal.** It holds the job lifecycle, escrow, and payouts, enforces capability and eligibility gates, and settles rewards. The compute — inference, generation, training — **never touches the chain**.

---

## 3. Consensus and Mining

- **Proof-of-work.** GPN uses a memory-hard, Ethash-derived PoW. Anyone can mine; there is **no permissioned validator set**. A node produces blocks by finding a valid seal; the network's consensus verifies it.
- **GPU-only by construction.** PoW difficulty is tuned to a target block time; at network difficulty a general-purpose CPU cannot competitively produce blocks, so mining is effectively GPU-only. A **minimum-GPU capability gate** (discrete GPU, or ≥ a measured-TFLOPS floor) makes this explicit for eligibility.
- **Block time: 15 s.** Chosen to keep a Bitcoin-like cadence and a ~2-year halving. (The network briefly ran at 4 s to speed up *block-bound AI coordination*; once AI escrow moved off-chain, 4 s bought nothing, so the original 15 s cadence was restored.)
- **Self-tuning difficulty.** A single global difficulty target retargets toward the target block time using **millisecond-resolution** timing (so it can harden even at sub-second block rates — no runaway for a fast miner).
- **Bitcoin-style rewards.** The **block producer keeps that block's reward** — a coinbase model. The producer's account is bound to the block via a **pre-runtime digest**, so the reward is deterministic at execution. There is no separate claim and no shared beneficiary.
- **Per-node wallets.** Each node mines with its **own** key; its own account is its payout address. Two miners on one chain each grow their own balance.
- **Fork tolerance.** Identity is explicit: **(chain-id, genesis hash, runtime version)**. An incompatible node fails to peer; a divergent network is **warned about loudly and starved of value**, never banned (banning would break legitimate private chains).

---

## 4. Eligibility: Attestation, Staking, and Liveness

Earning requires proving you are a **real GPU** — not spoofable, and not a CPU/APU.

- **Proof-of-Staked-GPU (PoSG).** An operator **attests** a GPU (count + measured GFLOPS) and, historically, posted a **stake** (1,000 GPN). Attestation carries a **tier** (Bronze/Silver/Gold) and a **reward multiplier** (1.0× / 1.5× / 2.5×) — i.e., **staking increases your reward**.
- **Stake-free eligibility, verified by work.** The mandatory stake was removed. A miner becomes pay-eligible by **producing blocks**: a seal-verified block *is* proof of real GPU work (the node verifies the Ethash seal on import), so after a small number of authored blocks the producer is paid directly. Staking remains **optional** — a lever that raises your tier and your reward share, not a gate.
- **Capability gates.** Jobs carry requirements — `min_gflops`, `min_vram_mb`, `min_ram_mb` — and the scheduler **will not route** a job to a provider that can't meet them (so a video job only goes to a big-VRAM card).
- **Capacity challenge (proof, not claim).** Because GFLOPS is self-reported, a random **compute challenge** lets a verifier confirm the device actually delivers the throughput it claims, gating high tiers on *proven* compute.
- **Self-organizing full-node layer.** A **liveness** pallet rewards always-on, publicly-reachable full nodes (peer-witnessed uptime) and gates top tiers behind being one — so the network's bootnode/reachability layer emerges organically rather than depending on a founder.

---

## 5. The AI Compute Marketplace

### 5.1 Jobs and escrow
A requestor submits a job; the chain runs it through a lifecycle:
`Pending → Accepted → Running → AwaitingVerification → Completed` (+ `Cancelled`, `Failed`).
Payment is **per token**: the requestor escrows `rate × max_output_tokens` (a cap), the provider meters **actual** usage, the charge is `rate × actual`, and the **surplus auto-refunds**. Priced in dollars, settled in GPN, so a buyer's *dollar* cost is stable while the GPN amount floats.

### 5.2 Pricing
Rates are **cost-derived** — a roofline estimate of the provider's true throughput (compute- and memory-bandwidth-bound), divided by realized tokens/sec, times a margin — targeting roughly half of comparable market GPU-rental, and clamped at or above the provider's cost so **a GPU always at least covers its cost**.

### 5.3 Routing: who runs a job
A buyer hits a single **access point** (`gpn_scheduler`) and never needs a provider's address. The scheduler:
1. **Filters** by capability (GFLOPS, VRAM/RAM, model availability).
2. **Requires availability** — a node currently running a job is not available (one GPU, one job).
   ("One job" = mutual exclusion — the GPU does one job *at a time*; it is **not** a limit on job
   *types*. Every node serves every job type — LLM · image · video · ML · fine-tune — by design.)
3. **Picks the lowest-latency (least-ms)** eligible provider, with a model-aware fallback (a provider that *can pull* a model wins only if downloading still finishes sooner).
4. **Dispatches** the assignment (pushed over HTTP to the provider) and the chain escrows + enforces the capability gate.

Providers **register and advertise their endpoint on-chain**, so access points **discover** them from the chain rather than a hand-maintained list. Providers can also pull a model on demand (via Ollama/HuggingFace), so requestor demand grows the catalog organically.

### 5.4 Off-chain escrow (payment channels)
The block-bound part of a job is the *coordination* (create/escrow → accept → settle), one transaction per step. To remove it, GPN supports **payment channels**: funds are locked **once** on-chain, and each job is settled **off-chain** by mutually-signed state updates. Either party can force-settle on-chain, so the chain remains the root of trust — and the **AI path has zero per-job transactions**, i.e. **no block delay**.

### 5.5 Job lifecycle safety
Jobs that stall are reclaimable: a **permissionless `expire_job`** (after `created_at + max_duration`), a **per-job failure refund** (`fail_job`), a **bounded on-chain expiry sweep**, and a **per-requestor active-job cap** — so a single actor can't deny service by exhausting slots. Providers are paid only for work actually delivered: once work has started, only the assigned provider (or the permissionless expiry) can finalize a job.

---

## 6. Tokenomics

- **Token:** GPN, 12 decimals. **Fair launch via mining** — no ICO, no liquid premine.
- **Emission:** all GPN is minted per block and **halves every 4,204,800 blocks (~2 years at 15 s)**. Halving is the sole emission mechanism.
- **Block reward:** 50 GPN/block (≈ 3.33 GPN/s). The reward and block time are coupled via `emission_rate = reward / target_block_time`.
- **Supply:** the **hard supply cap is ≈ 520.5 M GPN** — ≈ 420.48 M emitted over the halving schedule (2 × 50 GPN × 4,204,800) plus 100 M allocated to the treasury. The treasury allocation is fixed and does not grow, and emission converges at 420.48 M, so 520.5 M is the true maximum.
- **Treasury:** 100 M GPN held by a keyless pallet account — 70 M unlocked at genesis and 30 M vesting over 10 years — spendable only by a **13-of-25** governance roster.
- **Fees:** a 5% marketplace fee is split **40% staking rewards · 30% treasury · 20% burn · 10% oracle operators**.
- **Value anchor:** an oracle supplies a GPN/USD price (seeded at $0.15 for the testnet); the token's long-run value is intended to derive from **real AI-marketplace demand**, not mining inflation.

---

## 7. Privacy: Shielded Pool

GPN includes a **shielded pool** with Sapling-style note commitments and nullifiers:

- **Deposit** converts transparent GPN into a hidden **note** (a commitment in a Merkle tree) — value and owner are hidden.
- **Spend** proves, in zero knowledge, *"I own a note in the tree and haven't spent it"* — publishing only an **unlinkable nullifier** — and supports **shielded→shielded** (private transfer) and **shielded→transparent** (withdraw), enforcing value conservation.
- Because the tree grows, the chain keeps a **bounded history of Merkle roots** so an earlier note stays spendable.
- The zero-knowledge setup requires a **trusted ceremony**: a multi-party Powers-of-Tau / phase-2 setup where **the trapdoor is destroyed concurrently by independent contributors**. The network ships the **ceremony tooling**; the production CRS is produced by a **public ceremony** (secure as long as a single participant is honest). Until then, testnet proofs are guarded only by the on-chain nullifier/root/value checks.

---

## 8. Governance and Upgrades

- **Committee governance.** A committee proposes and executes **root** calls (e.g., fee splits, parameters) after a timelock — **no single key acts alone**. The mainnet sudo key is **inert**; governance is the committee.
- **Forkless upgrades.** The runtime upgrades **in place** via `set_code` with a separately-built WASM; the genesis — chain-spec, difficulty, supply, and the canonical runtime WASM — is **immutable**. A change that would reset the chain requires explicit sign-off.
- **Review-first.** Every change is logged (what/why/risk/rollback) and reviewed before it lands.

---

## 9. Network and Decentralization

- **Access points** are permissionless and redundant — anyone can run one; a buyer hits the nearest, and the scheduler routes network-wide.
- **Peer discovery** follows the Bitcoin pattern: a **persistent peer store**, **DNS seeds**, and **published seed lists**, plus **on-chain discovery** from the network's own node registry — with the compiled-in bootnode as a hardcoded fallback.
- **On-chain node registry** (liveness + provider endpoints) lets the network find its own reachable nodes without a central index.

---

## 10. Security Considerations

- **Double-spend / theft-of-service.** The marketplace's authorization and escrow rules are designed so a party cannot prematurely finalize a job to reclaim escrow while a provider is mid-work, and so failures/expiries refund trust-minimally.
- **PoW integrity.** Block rewards are bound to the block's producer and to chain difficulty; a claim can't be validated against a self-chosen (easier) target.
- **Privacy caveats.** A shielded pool is only as strong as its trusted setup — hence the public ceremony requirement — and its on-chain nullifier/root checks.
- **Eligibility spoofing.** Because capability is self-reported, it is layered with **proof-of-work** and a **capacity challenge** so eligibility reflects *demonstrated* compute.
- **Fork/clock/version.** Persistent desync is guarded against by explicit network identity and loud warnings; transient forks self-heal via the heaviest-chain rule (GPN has no finality gadget; confirmation is probabilistic by depth).

---

## 11. Roadmap

- **Shipped on testnet:** PoW chain with Bitcoin-style per-node rewards; GPU-only, stake-free, work-verified eligibility; the AI marketplace (LLM, image, video, ML, fine-tuning) with per-token escrow, pricing, least-ms routing, on-chain provider discovery, and VRAM/RAM gates; payment channels; shielded spends; the liveness layer; peer discovery; committee governance.
- **Next:** the public **CRS ceremony**; **client-side RTT** (user→nearest node); folding a node's own miner into the **auto-switch**; **mainnet** genesis and launch; a **cross-chain bridge** (post-mainnet).

---

## 12. Conclusion

The GPU is the most abundant and most under-used source of compute in the world. Mining monopolized it; AI rental markets fight over it; neither lets the hardware move to where the money is. **GPN makes a GPU earn from both markets at once** — mining when that pays, serving AI when that pays — coordinated by a trust-minimal chain, priced against real cost, and settled without a middleman. The token is a fair-launch proof-of-work coin whose durable value comes from the AI compute it enables.

*A GPU should not have to choose.*

---

### References (project corpus)
Design synthesis and full technical detail live in the GPN project artifacts: `goals-and-guidelines.md`, `design-tokenomics-spec.md`, `design-pricing-model.md`, `design-access-points-scheduler.md`, `design-offchain-escrow-channels.md`, `design-verifiable-gpu-proof-2026-10-06.md`, `design-liveness-selforg-fullnode-tiers.md`, `design-ceremony.md`, `design-crosschain-bridge.md`, `GPN-MASTER-REFERENCE.md`, and the security report `redteam-2026-10-08/REPORT.md`.
