# GPN — A GPU-Native Network for Mining and AI Compute

**Whitepaper v0.96 (draft)** · GPN Network · **last updated 2026-10-22**

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

**The switch is a decision, not a guess.** *By design*, every cycle the node estimates each mode's revenue in **USD/hr** — using an on-chain oracle feed of token price, GPU-hour rental price, utilization, and staking APR — and switches **only when the other mode beats the current one by a hysteresis margin (~5%)**, so the GPU doesn't thrash on tiny price crossings. A switch would be a real **stop/start of GPU work**: entering Compute stops the miner and frees VRAM before the model loads.

**What runs today is a yield, not that switch.** Mining is the default; when a node accepts an AI job, mining **stops completely** for the job's duration and resumes when it completes. The economic decision (and the oracle feed that would drive it) is **not yet wired** — see §6.1 and §12.

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
Rates are **cost-derived** — a roofline estimate of the provider's true throughput (compute- and memory-bandwidth-bound), divided by realized tokens/sec, times a margin — taken as **half of the comparable market GPU-rental rate**, and clamped between the provider's cost and the market so **a GPU always at least covers its cost**. The full formula, its worked example and its two missing inputs (no oracle feed, no input-token metering) are in §6.1.

### 5.3 Routing: who runs a job
A buyer hits a single **access point** (`gpn_scheduler`) and never needs a provider's address. The scheduler:
1. **Filters** by capability (GFLOPS, VRAM/RAM, model availability).
2. **Requires availability** — a node currently running a job is not available (one GPU, one job).
3. **Picks the lowest-latency (least-ms)** eligible provider, with a model-aware fallback (a provider that *can pull* a model wins only if downloading still finishes sooner).
4. **Dispatches** the assignment (pushed over HTTP to the provider) and the chain escrows + enforces the capability gate.

Providers **register and advertise their endpoint on-chain**, so access points **discover** them from the chain rather than a hand-maintained list. Providers can also pull a model on demand (via Ollama/HuggingFace), so requestor demand grows the catalog organically.

### 5.4 Off-chain escrow (payment channels)
The block-bound part of a job is the *coordination* (create/escrow → accept → settle), one transaction per step. To remove it, GPN supports **payment channels**: funds are locked **once** on-chain, and each job is settled **off-chain** by mutually-signed state updates. Either party can force-settle on-chain, so the chain remains the root of trust — and the **AI path has zero per-job transactions**, i.e. **no block delay**.

### 5.5 Job lifecycle safety
Jobs that stall are reclaimable: a **permissionless `expire_job`** (after `created_at + max_duration`), a **per-job failure refund** (`fail_job`), a **bounded on-chain expiry sweep**, and a **per-requestor active-job cap** — so a single actor can't deny service by exhausting slots. Providers are paid only for work actually delivered: once work has started, only the assigned provider (or the permissionless expiry) can finalize a job.

### 5.6 The public API and the model catalogue

**One HTTPS endpoint.** `https://gpncoin.ai/api/v1/*` exposes the marketplace as an **OpenAI-compatible** surface — chat completions, model list, provider list, job lookup. A **Cloudflare Worker** terminates TLS and proxies to the access point over a raw socket, injecting the access point's bearer token; this exists because the public access point speaks plain HTTP on :8092, which a browser page cannot call directly (mixed content). The edge also applies **rate limits** — 12 chat requests per 10 minutes per IP, 120 reads per minute — and returns a typed `gpn_rate_limited` error rather than an opaque failure. **Inference on the testnet is free.**

A measured caveat, stated plainly: end-to-end latency on the public route is dominated by the **create-inclusion wait** (the job's on-chain create must land in a block before a provider may claim it — tens of seconds at the current cadence), not by inference itself, which is on the order of one second.

**A curated, VRAM-gated catalogue.** The network does not advertise everything a provider happens to have installed:

- **A model must fit.** A weight-size table plus a 1.15× headroom factor (KV cache, CUDA context, tokenizer buffers) decides whether a model can be held by a given card. The gate is applied at **three** points: what the provider advertises, what it will claim, and what the scheduler will route. A `deepseek-r1:14b` (~10.3 GB) is unusable on an 8 GB node and is therefore never offered.
- **The public list is a shop window.** `/v1/models` lists assistant-grade chat models only — sixteen families by default, prefix-matched so quantisation tags resolve. **Hard-excluded** even when an operator asks for everything: per-job fine-tune artifacts (`tuned-*`), toy models below ~0.3 B parameters (`stories260K` is 0.00026 B), and embedding/reranker models.
- **Excluded is not unusable.** A caller who names a model explicitly still gets served. The catalogue governs advertising, not permission.

On the reference 8 GB node this yields four served models: `llama3.2:1b`, `qwen2.5:0.5b`, `qwen2.5:7b`, `deepseek-r1:7b` — a deliberate floor of *assistant-grade* models rather than an inventory dump. *(Shipping status, 2026-10-22: the code is live in the 0.2.0 line; the public endpoint is still served by a pre-fix AP process, so the curated list appears once that process is restarted.)*

---

## 6. Tokenomics

- **Token:** GPN, 12 decimals. **Fair launch via mining** — no ICO, no liquid premine.
- **Emission:** all GPN is minted per block and **halves every 4,204,800 blocks (~2 years at 15 s)**. Halving is the sole emission mechanism.
- **Block reward:** 50 GPN/block (≈ 3.33 GPN/s). The reward and block time are coupled via `emission_rate = reward / target_block_time`.
- **Supply:** the **hard supply cap is ≈ 520.5 M GPN** — ≈ 420.48 M emitted over the halving schedule (2 × 50 GPN × 4,204,800) plus 100 M allocated to the treasury. The treasury allocation is fixed and does not grow, and emission converges at 420.48 M, so 520.5 M is the true maximum.
- **Treasury:** 100 M GPN held by a keyless pallet account — 70 M unlocked at genesis and 30 M vesting over 10 years — spendable only by a **13-of-25** governance roster.
- **Fees:** a 5% marketplace fee is split **40% staking rewards · 30% treasury · 20% burn · 10% oracle operators**.
- **Value anchor:** an oracle supplies a GPN/USD price (seeded at $0.15 for the testnet); the token's long-run value is intended to derive from **real AI-marketplace demand**, not mining inflation.

### 6.1 Derived pricing: throughput, not a rental market

GPN does not price tokens off a competitor's price list, nor off a GPU-rental market. It prices them off
**throughput**:

```
anchor_tok_s = roofline(30,000 GFLOP/s, 272 GB/s, 1B) = 95.2 tok/s
anchor_rate  = $0.10/hr                               # GPN_GPU_ANCHOR_USD_HR
k            = anchor_rate / (anchor_tok_s × 3600)    # USD per output token
price_out    = k × (1 + 0.30)                         # GPN_MARGIN_BPS=3000 → $0.379/1M
price_in     = price_out × 0.25                       # GPN_INPUT_RATE_FRAC
provider_hr  = roofline(gflops, bw, params_b) × 3600 × k
escrow       = prompt_tokens/4 + max_tokens           # output-equivalents
```

The anchor is a single tunable constant: what a 4060-class consumer card earns per hour. Dividing it by
the token rate that card achieves on a 1B model gives `k`, the pre-margin price of one output token;
margin is added on the buyer side. The GPN amount is then the dollar price converted at the on-chain
token price.

Two properties are deliberate. **The buyer price is GPU-independent**: every request costs the same per
token whichever card serves it, so a market of heterogeneous hardware can clear at all — a per-GPU price
would make the cheapest card the only one ever selected. Faster hardware earns the *provider* more per
hour instead:

| GPU (1B stream) | Bandwidth | Throughput | Provider earns |
|---|---|---|---|
| RTX 4060 8GB (anchor) | 272 GB/s | 95.2 tok/s | **$0.100/hr** |
| RTX 4090 24GB | 1008 GB/s | 352.8 tok/s | **$0.371/hr** |
| A100 80GB | 2039 GB/s | 713.7 tok/s | **$0.750/hr** |
| H100 80GB | 3350 GB/s | 1172.5 tok/s | **$1.232/hr** |

And **throughput, not FP32 GFLOPs, is the metric**: decode of a small model is memory-bandwidth-bound, so
a FLOP-based price would rank a 4090 above an H100.

The result is $0.379 per 1M output tokens and $0.095 per 1M input tokens — next to `gpt-5-nano`'s $0.40,
rather than the ~$8.75 (22×) that the old `$3.00 GPU-hour` anchor produced. A price outside
**[$0.05, $5.00] per 1M** refuses to publish unless `GPN_PRICING_UNSAFE=1`, and a provider whose modelled
earning falls below its own declared cost is logged as such rather than hidden.

**Two limitations we are not papering over.** (1) **Input metering is channel-first.** The off-chain
channel bills input at 25% of the output rate. The on-chain pallet stores one
`(price_per_token, max_tokens, actual_tokens)` and charges a single rate, so an on-chain job publishes a
*blended* rate over `prompt+max` — exact when the completion runs to `max_tokens`, slightly under-billing
short completions. Exact two-rate settlement on-chain needs a pallet change (a second rate in
`JobBilling`). (2) **The oracle feed is absent** — the USD→GPN conversion falls back to $0.15/GPN; the
legacy market model, still available as `GPN_PRICING_MODEL=market`, keeps its $3.00 GPU-hour fallback.

---

## 7. Privacy: Shielded Pool

GPN includes a **shielded pool** with Sapling-style note commitments and nullifiers:

- **Deposit** converts transparent GPN into a hidden **note** (a commitment in a Merkle tree) — value and owner are hidden.
- **Spend** proves, in zero knowledge, *"I own a note in the tree and haven't spent it"* — publishing only an **unlinkable nullifier** — and supports **shielded→shielded** (private transfer) and **shielded→transparent** (withdraw), enforcing value conservation.
- Because the tree grows, the chain keeps a **bounded history of Merkle roots** so an earlier note stays spendable.
- The zero-knowledge setup requires a **trusted ceremony**: a multi-party Powers-of-Tau / phase-2 setup where **the trapdoor is destroyed concurrently by independent contributors**. The network ships the **ceremony tooling**; the production CRS is produced by a **public ceremony** (secure as long as a single participant is honest). Until then, testnet proofs are guarded only by the on-chain nullifier/root/value checks.

---

## 8. Governance and Upgrades

### 8.1 What is live on chain today — spec 41

- **Committee governance.** A committee proposes and executes **root** calls (e.g., fee splits, parameters) after a timelock — **no single key acts alone**. The mainnet sudo key is **inert**; governance is the committee.
- **Current parameters, stated exactly:** a **3-member** committee, **2-of-3** approvals, a **3-day** timelock, no `cancel`, no proposal expiry, no preimage proposals, and **zero weights** (`WeightInfo = ()`, so proposal weight is unmetered).
- **Forkless upgrades.** The runtime upgrades **in place** via `set_code` with a separately-built WASM; the genesis — chain-spec, difficulty, supply, and the canonical runtime WASM — is **immutable**. A change that would reset the chain requires explicit sign-off.
- **Review-first.** Every change is logged (what/why/risk/rollback) and reviewed before it lands.

### 8.2 What is staged — spec 42, built and tested, **not applied**

A hardening upgrade exists as an artifact (**512,888 bytes**, sha256 `7694f7f6…d30f1`, `spec_version 42`) with 22 pallet tests, 10 runtime-rehearsal tests and a storage migration. It would change:

| Today (live) | Staged (spec 42) |
|---|---|
| 3-member committee | **5 members**, seedable and replaceable (`set_members`, Root-only) |
| 2-of-3 approvals | **3-of-5** |
| proposals live forever | **14-day expiry** + pruning, bounded at 64 open proposals |
| cannot withdraw a proposal | **`cancel(hash)`** at the same 3-of-5 threshold |
| call data inline (a 500 KB WASM in a public extrinsic) | **preimage proposals** — approve a `blake2_256` code hash, verify before the timelock elapses |
| `WeightInfo = ()` (zero weight) | **hand-set per-byte weights** (benchmarked weights remain a TODO) |

**None of it is on chain.** No `set_code` has been submitted, no committee key has been used, and the canonical genesis WASM is untouched. A reader must not conflate the two columns: the live chain is the left one. The artifact, its review checklist and its exact enactment sequence live in `staged/gov-hardening-v1.md`.

---

## 9. Network and Decentralization

- **Access points** are permissionless and redundant — anyone can run one; a buyer hits the nearest, and the scheduler routes network-wide.
- **Peer discovery** follows the Bitcoin pattern: a **persistent peer store**, **DNS seeds**, and **published seed lists**, plus **on-chain discovery** from the network's own node registry — with the compiled-in bootnode as a hardcoded fallback.
- **On-chain node registry** (liveness + provider endpoints) lets the network find its own reachable nodes without a central index.
- **Address publication is best-effort, never a gate.** A provider publishes a dialable address by walking an evidence ladder: an operator-declared `GPN_PUBLIC_ADDR` → the state file the node writes when it has proof (an inbound peer observed, or UPnP/NAT-PMP mapped) → a public listen address → a STUN mapping on the listen port. Loopback, RFC1918, link-local, CGNAT and ULA ranges are never published. When no rung qualifies, the published list is empty **and the log enumerates which rungs were rejected and the exact fix** — an empty list is not an error, because routing is **pull-based** and works over the provider's outbound poll.

### 9.1 Node operations: dashboard, Idle mode, and self-updating

GPN is meant to run on a **gaming PC**, which imposes two requirements a datacenter design would never notice: the owner must be able to take the GPU back, and the node must not need babysitting.

**Local dashboard.** `gpn_node --gui` (or `GPN_GUI=1`) serves `http://127.0.0.1:8099` from the supervisor process — mode, peers, block height, GPU/VRAM, current job, child-process health and the idle panel, refreshed every three seconds from a page compiled into the binary (no CDN, no build step, works offline). It is **loopback-only by design**: the panel can stop the miner, so binding it to `0.0.0.0` or a LAN address is refused at startup with an explanatory error. There is no authentication because there is no network exposure; remote access is an SSH tunnel.

**Idle mode.** A single switch that stops the two things competing for the GPU — **Ethash mining and AI job claims** — for up to **six hours**, while the node keeps **syncing, peering and serving the chain to everyone else**. Nothing is stopped, uninstalled or de-registered. At the deadline the node resumes on its own; a *Resume now* button (`POST /api/resume`) or `gpn_node resume` ends it early. The whole state is one file (`~/.gpn/idle.json`, mode 0600, absolute deadline), so idle **survives a restart, a crash or a reboot**, and an expired or corrupt file always reads as *active* — there is no way to accidentally leave a box parked forever. A job already running **finishes normally**, because a half-run job would wedge escrow; a job is claimed and executed inside a single provider pass, so nothing is ever cut in half. One safety valve: if the node has **zero peers** it may be the **only block producer**, so an idle request is **refused** (HTTP 409) rather than halting the chain for everyone — `--force-idle` overrides on a single-box dev chain.

**Auto-update — on by default, opt-out.** A node is a consensus participant, so running a stale binary is not a neutral choice: it can seal blocks its peers reject, or serve jobs under a schema nobody else speaks. GPN therefore ships a signed update channel at `https://gpncoin.ai/releases.json`: an **ed25519 signature** over a canonical, line-oriented text blob (not the JSON, so signer and verifier cannot disagree about formatting) covering the version, the **minimum-version cutoff**, every asset URL, size and sha256. The public key is compiled into the node, so a compromised web host can serve a forged manifest but cannot sign it.

Installs are **bundle-aware**, because a GPN release is ~60 executables, not one binary — replacing only the supervisor would leave it and its children on different versions. The updater extracts a complete tree into `versions/<v>/`, proves identity from the **build stamp baked in at compile time** (a bundle that says 0.1.9 while the manifest says 0.2.0 is refused), and flips a `current` symlink (a junction on Windows) in one atomic rename. The previous tree stays reachable via `current.previous`, so rollback is a single link flip.

The fleet is not allowed to stampede. Each node derives a **stable phase** from its payout account and start time, checks within that window each interval, and applies **±5–10% jitter** after every check; finding a new release still waits a deterministically-random **install spread** (`GPN_UPDATE_SPREAD_SECS`, default 1200 s) before downloading. In simulation, 100 nodes occupy 87 distinct one-minute buckets of a 6-hour interval with no minute holding more than 5% of the fleet — versus all 100 in one second.

Operators who prefer to update by hand may: a node whose version falls below `min_version` prints **`UPGRADE REQUIRED`**, keeps **syncing**, and stops **mining and announcing** until it is upgraded. It is not banned and not slashed; it is simply not allowed to produce blocks that would have to be reorged out. Opting out is `GPN_AUTO_UPDATE=0` / `auto_update = false`.

---

## 10. Security Considerations

- **Double-spend / theft-of-service.** The marketplace's authorization and escrow rules are designed so a party cannot prematurely finalize a job to reclaim escrow while a provider is mid-work, and so failures/expiries refund trust-minimally.
- **PoW integrity.** Block rewards are bound to the block's producer and to chain difficulty; a claim can't be validated against a self-chosen (easier) target.
- **Privacy caveats.** A shielded pool is only as strong as its trusted setup — hence the public ceremony requirement — and its on-chain nullifier/root checks.
- **Eligibility spoofing.** Because capability is self-reported, it is layered with **proof-of-work** and a **capacity challenge** so eligibility reflects *demonstrated* compute.
- **Fork/clock/version.** Persistent desync is guarded against by explicit network identity and loud warnings; transient forks self-heal via the heaviest-chain rule (GPN has no finality gadget; confirmation is probabilistic by depth).

---

## 11. Roadmap

- **Shipped and live on testnet:** PoW chain (spec 41, 15 s, 50 GPN/block) with Bitcoin-style per-node rewards; GPU-only, stake-free, work-verified eligibility; per-token escrow, cost-derived pricing, least-ms routing to idle providers, on-chain provider discovery, VRAM/RAM gates; payment channels; shielded spends; the liveness layer; peer discovery; the **public HTTPS AI API** (with a curated, VRAM-gated catalogue in the 0.2.0 line); the **node dashboard, Idle mode and signed auto-update**.
- **Shipped but not publicly routed:** image generation (on-chain jobs) and the fine-tune/ML providers.
- **Blocked:** video (LTX) — the encoder exceeds 8 GB of VRAM.
- **Staged, not applied:** the spec-42 governance hardening (§8.2).
- **Next:** the public **CRS ceremony**; applying governance hardening; the on-chain **GPU-hour oracle feed** and **input-token metering**; **client-side RTT** (user→nearest node); folding a node's own miner into the **economic auto-switch**; **mainnet** genesis and launch; a **cross-chain bridge** (post-mainnet).

## 12. Current status and honest limitations

| Capability | Status | What that means |
|---|---|---|
| Text / LLM inference | **LIVE** | Public HTTPS, OpenAI-compatible, free on testnet |
| Image generation | **WORKS INTERNALLY** | Completes via on-chain `POST /jobs` (`ImageGeneration`, sd-turbo); **no public `/v1` route** |
| Video (LTX) | **BLOCKED** | Implemented, never completed — T5-XXL encoder exceeds 8 GB VRAM |
| Fine-tune / ML | **WORKS INTERNALLY** | Providers ship and are claimable on chain; not publicly routed |
| GPU-hour price oracle | **GAP** | Feed absent; scheduler runs on hardcoded fallbacks |
| Input-token metering | **GAP** | Output only |
| Governance hardening | **STAGED** | Chain is still 3-member / 2-of-3 / no expiry / zero weights |
| Economic auto-switch | **GAP** | Live behaviour is a mine→stop→resume yield, not oracle-driven switching |
| Fork choice | **GAP** | Longest-chain by height, not most-work (bounded, non-zero risk) |

The honest summary: **the chain, the LLM marketplace, the public API, and the node tooling are real and running.** The multi-modal marketplace is real in code and narrow in exposure. Two economic inputs are placeholders. And the network is a **testnet**: one to three authorities, free inference, a $0.15 placeholder token price, and a trusted setup that is not yet the production ceremony. Nothing here is a claim about mainnet readiness.

---

## 13. Conclusion

The GPU is the most abundant and most under-used source of compute in the world. Mining monopolized it; AI rental markets fight over it; neither lets the hardware move to where the money is. **GPN makes a GPU earn from both markets at once** — mining when that pays, serving AI when that pays — coordinated by a trust-minimal chain, priced against real cost, and settled without a middleman. The token is a fair-launch proof-of-work coin whose durable value comes from the AI compute it enables.

*A GPU should not have to choose.*

---

### References (project corpus)
Design synthesis and full technical detail live in the GPN project artifacts: `goals-and-guidelines.md`, `design-tokenomics-spec.md`, `design-pricing-model.md`, `design-access-points-scheduler.md`, `design-offchain-escrow-channels.md`, `design-verifiable-gpu-proof-2026-10-06.md`, `design-liveness-selforg-fullnode-tiers.md`, `design-ceremony.md`, `design-crosschain-bridge.md`, `staged/gov-hardening-v1.md`, `GPN-MASTER-REFERENCE.md`, and the security report `redteam-2026-10-08/REPORT.md`. Operator references in the repo: `docs/updates.md`, `docs/node-gui-and-idle.md`, `docs/provider-address-and-models.md`, `docs/upgrades-governance.md`.
