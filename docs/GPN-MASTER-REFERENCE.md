# GPN — MASTER REFERENCE (single-document review)

**Project:** Project 7 — GPN, a GPU-native crypto + AI-compute network
**Last updated:** 2026-10-08 · **Owner:** main / PM07
**Purpose:** One place to review *everything* decided about GPN — vision, goals, architecture,
tokenomics, mining, attestation, marketplace, governance, infra, current state, and open items.
Synthesized from the full `shared/artifacts/project7/` corpus + session history (see §17 doc index).
**If this doc and the code disagree, the CODE wins** (this is a guide, not the spec-of-record).

> ⚠️ **Why this exists:** too many design details were spread across dozens of artifacts, and some
> goals got forgotten mid-design. This is the canonical "read me first" for GPN.

---

## 1. What GPN is (one paragraph)

A **GPU-native network** where each GPU either **mines** (PoW) or **serves AI compute**,
**auto-switching** between the two. **The chain = coordination + escrow + reward settlement;
the heavy AI compute runs entirely off-chain.** Anyone with a GPU can earn (mine or serve AI);
anyone can buy AI compute through one simple REST/OpenAI-compatible interface, paying per token
at cost-derived prices; on-chain values are salted/opaque so prompts/completions are never
recorded. Everything is decentralized via committee governance; the genesis is immutable;
runtime changes ship **forklessly** via `set_code`.

**The core premise (do not lose this):** *every node/miner must be able to run AI jobs.* A GPN
node is a **miner AND an AI worker**. Mining-only is half the machine.

---

## 2. Goals (from `goals-and-guidelines.md` — the standing doc)

1. **Turn idle GPUs into income.** Any GPU owner earns by mining or serving AI, minimal setup.
2. **Cheap, accessible AI compute.** Buyers use a simple REST/OpenAI-compatible interface, pay
   per token, priced to real cost.
3. **Trustless coordination.** Chain holds escrow, enforces capability gates, settles rewards;
   nobody can run off with funds.
4. **Privacy by default.** Q&A are **never** on-chain — only opaque/salted commitments.
5. **Decentralization.** Committee-based governance; no single key acts alone.
6. **Sustainability for providers.** Price = compute cost + margin; a GPU always covers its cost.
7. **A usable, fork-tolerant network.** Mainnet unambiguously identifiable (genesis + chain-id);
   forks tolerated but starved of value.

**Guidelines (architecture):**
- Chain = coordination + escrow. **Never move AI compute on-chain.**
- **One GPU does one job at a time.** An AI job yields mining; a machine never mines *and* infers.
- Privacy: on-chain values salted/opaque.
- **Reward and block time are coupled:** `emission_rate = block_reward / target_block_time`.

---

## 3. Architecture overview

- **Chain:** Substrate/FRAME, **custom PoW consensus** (`sc-consensus-pow`, Ethash-style).
  Runs as `gpn-node`.
- **On-chain pallets:** `gpu-pow` (consensus + rewards + difficulty), `gpu-attestation` (PoSG,
  tiers, stake), `compute-marketplace` (jobs/escrow), `gpn-token` (supply/emission/fees),
  `profitability-oracle` (token price / GPU-hour price / utilization / APR feed),
  `gpn-treasury` (vested treasury), `gpn-liveness` (full-node/bootnode layer), `gpn-shield`
  (privacy/shielded, WIP).
- **Off-chain agents (`gpn-agent`):** provider (`gpn_llm_provider`, `gpn_diffusion_provider`),
  miner (`gpn_external_miner`), access-point/scheduler (`gpn_scheduler`), ops tools
  (`gpn_upgrade_runtime`, `gpn_send`, `gpn_attest`, etc.).
- **Split:** on-chain = jobs, escrow, attestations/capabilities, payouts (source of truth).
  Off-chain = latency, live load, endpoint discovery, the actual AI compute.

### 3.1 A node runs ONE mode at a time — the auto-switch (Mining ⇄ Compute)

A GPN node does **not** mine and serve AI simultaneously — the one GPU does one thing. The node is
in **one of three modes**:
- **Mining** — hashing GPN blocks (GPU-PoW) → newly-minted GPN.
- **Compute** — serving AI jobs (LLM / image / video / fine-tune) → GPN from job escrow.
- **Staking** — *(Phase C, not live at launch)* → staking APR.

At launch the practical switch is **Mining ⇄ Compute**.

**When the switch fires** (`profitability::decide_switch` in `gpn-agent`): every cycle the node
estimates each mode's revenue in **USD/hr** (from the profitability-oracle feed: token price,
GPU-hour price, utilization, staking APR) and switches **only when the other mode beats the current
one by the hysteresis buffer (~5%)**. That buffer is the anti-flapping rule — small crossings don't
move the GPU; only a real gap does. Verified: a scripted demand-spike flips Mining → Compute →
Mining (`gpn_simulate_autoswitch`).

**What a switch does** (`ModeManager`): `switch_to(Mining)` starts the stoppable GPU mining engine;
`switch_to(Compute)` **stops the miner, releasing the DAG from VRAM**, then runs the AI job. So the
switch is a real start/stop of GPU work, not just a flag.

**Reality / gap (2026-10-06):** the clean switch is the **agent `ModeManager`** path (external miner
+ compute agent). On the current live testnet the boxes run the **node's internal miner**
(`--force-authoring`) *and* a separate provider process, which **contend** for the GPU instead of
switching cleanly. Wiring the node-internal miner into the same switch is part of the
"every node is miner + AI-worker" work (§15).

**⚑ Design intent — a node IS a provider (don't split them):** a GPN node is **one entity** — a GPU
that mines **OR** serves AI, under **one identity/wallet**, auto-switching between the two. The
current split into two separate processes (a `gpn-node` miner + a separate `gpn_provider` agent) and
the scheduler's registry tracking only provider heartbeats are **transitional implementation
artifacts, not the design**. The target: a node registers **once**, advertises both mining and
compute capability (attestation + heartbeat), is routable as a provider, and switches its single GPU
between mining and serving. *Any place the docs/code treat "miner" and "provider" as different
populations is a gap to close.*

---

## 4. Consensus & mining

- **PoW, GPU-only-by-design** (Ethash-style, memory-hard). Anyone can mine; **no validator set**
  (`sc-consensus-pow`). A node mines iff `role.is_authority()` — run with `--validator`, or
  `--force-authoring` (required on the testnet boxes).
- **Difficulty:** a **single global value** (`pallet-gpu-pow::Difficulty`), self-tuning every
  `DifficultyAdjustmentWindow` toward `TargetBlockTime`. **Target = 15 s** (spec 14, 2026-10-08 — reverted
  from 4 s; see §7/D1: 4 s only sped the block-bound AI *coordination*, which off-chain escrow (#2)
  removes, so we returned to the original ~2-yr-halving / Bitcoin-like cadence).
  - ✅ **Retarget fixed (spec 13, 2026-10-08):** the retarget now averages in **milliseconds** (was
    whole seconds → difficulty froze below 1 s/block). Bitcoin-like ∩ runaway-proof.
- **Shielded note trapdoor — FIXED + STAGED (red-team finding #22, 2026-10-09; spec 30).** Previously `ShieldedPool::deposit` derived the note trapdoor from its **public** call args and `spend` never authenticated the caller, so the full note-spend witness was publicly reconstructible and **any account could forge a spend and steal any deposited note** (independent of the testnet-CRS caveat). `deposit` now takes a **caller-supplied commitment `cm`** (computed off-chain from a secret random `rcm`; the pallet no longer derives the trapdoor), records `DepositedValue[cm] = amount`, and `spend` rejects a release greater than the recorded deposit (`Error::ValueExceedsDeposit`). Staged artifact `wasm/gpn_runtime_spec30_redteam_shielded_trapdoor.compact.compressed.wasm` (spec 30; **not applied** — chain-wide = review-first; on-chain is 22). Analysis: `design-shielded-note-trapdoor.md`.
- **Fork choice is BY HEIGHT, not cumulative work (⚠️ red-team finding #17, 2026-10-09 — remedial, review-first).** The node builds the miner + PoW import queue on `sc_consensus::LongestChain`, and `sc-network-sync` reorgs on `best_number` independently of `SelectChain` ⇒ GPN follows the **longest** chain, not Bitcoin's **heaviest** (most-work) chain. The earlier claim "difficulty is global ⇒ per-block work ~equal ⇒ longest == heaviest ⇒ no change needed" is **only true while every branch shares one difficulty trajectory**; `pallet-gpu-pow::Difficulty` is *branch-local* state retargeted every 10 blocks from that branch's timestamps, so an easier (larger-target) branch can be longer with less work (no finality gadget bounds it). Bounded-but-nonzero today (≤30 s timestamp drift, ≥1 s/block, ×2/window clamp, lone authority). Remediation + full analysis: `design-fork-choice-heaviest.md` (cumulative-work accumulator + work-aware node/sync fork choice). **Not yet implemented.**
- **Bitcoin-style rewards (DONE, spec 10):** the block **producer** keeps that block's reward
  (coinbase model). The producer's account is stamped in the **PRE-runtime digest** (the PoW *seal*
  is post-runtime → unavailable at execution; that was the original bug). Implemented via
  `FindAuthor` reading `as_pre_runtime()` + `start_mining_worker(..., pre_runtime=<acct>)`.
- **Per-node wallets:** each node runs with its **own** key/keystore; its own account is its payout
  address (`GPN_MINER_ACCOUNT` overrides). No shared `--alice`.
- **Block reward:** **50 GPN/block** (restored 2026-10-08 via `set_block_reward`; was briefly 12 for
  the 4 s era). **Halving every 4,204,800 blocks (~2 yrs at 15 s).** ⚠️ **Max mineable = 2 × reward ×
  interval ≈ 420 M** (+100 M treasury ⇒ **~520 M effective cap**, not the advertised 1 B) — see §7 / D1.

### 4.1 Forks, desync & self-healing (at scale)

**Two different things are called “desync”:**

1. **Transient forks (normal, self-healing).** In PoW two miners can find a block near-
   simultaneously → two tips; nodes then converge on the heaviest chain and reorg back. Frequency ≈
   (propagation time) ÷ (block interval). GPN's **15 s blocks** (Bitcoin 10 min → ~0.1%
   orphans), so short reorgs are **routine** on a wide network. With one authority, forks don't arise;
   on a multi-miner network the default fork choice is longest-by-height (see the ⚠️ fork-choice note
   in §4 — the "longest == heaviest" assumption is not guaranteed). **Common, not broken.**
2. **Real desync (rare, not always self-healing):**
   - **Different genesis** = a *different network*, not a fork of ours → **NOT healed by design**
     (fork-tolerance posture: warn loudly, starve it of value). *(Windows `0x07f8…` case.)*
   - **Runtime/version mismatch** → handshake rejects, or blocks fail to execute → **self-heals**
     (node pulls the upgraded on-chain `:code` on sync).
   - **Clock skew** → difficulty/timestamp checks reject valid blocks → **not healed**; needs NTP.
   - **Consensus bug** → persistent split needing release + coordination. *(Latent example: the
     ms-retarget bug — a fast-enough miner could push blocks <1 s, freeze difficulty, run away.)*

**Self-healing GPN has:** **block sync** (lags),
**automatic on-chain runtime upgrade**, **loud identity warnings** for wrong-network nodes.
**Deliberately absent:** **no finality gadget** (`finalized` stays at genesis ⇒ confirmation is
**probabilistic by depth**; wait *k* blocks), and **no fork prevention** (bans break dev/private nets).
**Scale effect:** more nodes ⇒ more propagation delay ⇒ *more frequent, shallow* reorgs (normal); if
orphan rate is high, raise `TargetBlockTime`/difficulty. *Persistent* desync comes from
**genesis / version / clock / bug — not scale.**

---

## 5. Attestation, tiers & staking

**Purpose:** prove "this is a real GPU" (anti-spoof) and set provider capability/tiers.

- **Pallet:** `gpu-attestation` — "Proof of Staked GPU (PoSG)".
- **Tiers** by GPU count + measured GFLOPS floor: **Bronze** (≥1 GPU, ≥100 GFLOPS) ·
  **Silver** (≥4, ≥1,000) · **Gold** (≥16, ≥5,000).
- **Staking multiplier** (`staking_multiplier_bps`): **Bronze 1.0× / Silver 1.5× / Gold 2.5×** —
  the intended "staking increases your reward" mechanism, paid from the staking-reward pool.
- **Stake:** `MinAttestationStake = 1,000 GPN`. Cadence: attest every ~3,600 blocks; max age
  14,400 blocks (~60h); max 3 misses → 10% stake slashed/BURNED.
- **Full-node gating (spec, `gpn-liveness`):** Silver/Gold additionally require being an **active,
  publicly-reachable full node** with uptime (≥90% Silver / ≥95% Gold). Bronze stays open.
- **Miss-slash evasion — FIXED + STAGED (red-team finding #23, 2026-10-09; spec 31).**
  `submit_attestation` used to reset `missed_count = 0` and clear `LastMissBlock` on **every** call,
  and the open (Bronze) attestation's `challenge_response` is not verified on-chain — so an idle
  provider could zero its miss history just by re-attesting (self-reported) and dodge the
  `MaxMissedAttestations` slash forever. A re-submission now **preserves** the miss history; only a
  fresh attestation starts clean, and an honest provider clears it via a confirmed liveness window
  (`gpn-liveness::submit_liveness`). Staged artifact `wasm/gpn_runtime_spec31_redteam_attestation_miss_reset…` (spec 31; **not applied**).
- **Miss-slash evasion — CLOCK residual FIXED + STAGED (red-team finding #26, 2026-10-09; spec 33).**
- **Attested-provider counter integrity — FIXED + STAGED (red-team finding #27, 2026-10-09; spec 34).** `AttestedProviderCount` is bumped only by a *fresh* `submit_attestation`, but `withdraw_stake` decremented it **unconditionally** — so any account could `deposit_stake` → `withdraw_stake` *without ever attesting* and drive the counter below its true value (repeatable to 0). The decrement now mirrors the increment (only when an attestation actually existed). `pallet-gpu-attestation` 39/39. Staged artifact `wasm/gpn_runtime_spec34_redteam_attested_count.compact.compressed.wasm` (spec 34; **not applied**; on-chain 22).
- **Miss-slash evasion — CLOCK residual FIXED + STAGED (red-team finding #26, 2026-10-09; spec 33).**
  #23 pinned the miss *counter* but the miss *clock* in `check_expired` was still anchored to the
  refreshable `submitted_at`, so a re-attestation just before each window closed kept the clock
  fresh and the provider still never accumulated `MaxMissedAttestations` misses (slash evasion).
  The clock is now anchored to `LastMissBlock` (seeded at a fresh attestation; left alone by
  re-submissions), and the missing genuine reset path is implemented: new cross-pallet hook
  `gpn-liveness::AttestationLivenessSink::reset_misses` is called by `submit_liveness` on a
  confirmed window, clearing `missed_count` and restarting the window (runtime wires
  `type Attestation = GPUAttestation`). Staged artifact
  `wasm/gpn_runtime_spec33_redteam_attestation_miss_clock…` (spec 33; **not applied**).

### 5.1 Decision change — 2026-10-06 (stake-free, verifiable GPU proof) — **APPLIED (spec 11)**
Jeremy decided the model should be **more Bitcoin-like, GPU-only**, and that **staking should be a
carrot (reward boost), not a gate**. Reconciled with the original "staking increases your reward"
design (the tier multiplier), which was **designed but never implemented** (stub).
- **Chosen approach (C):** remove the mandatory stake from attestation + add **real verification**;
  keep staking **optional** (raises your tier ⇒ raises your reward multiplier).
- **Implemented (spec 11, live):**
  - `gpu-pow`: new `BlocksAuthored` counter + `MiningAttestationThreshold = 3`. The block reward
    goes to the **author once it has authored ≥3 seal-verified blocks** (or is grandfathered via an
    existing attestation). Producing a block **is** the GPU proof (node verifies Ethash on import).
  - `gpu-attestation`: **removed** the hard `ProviderStake` requirement from `submit_attestation`
    (staking now optional). `deposit_stake`/`withdraw_stake` kept.
  - Applied forkless via `sudo(System.set_code)`; WASM artifact
    `…/wasm/gpn_runtime_v11_stakefree_mining.wasm`; canonical bundled WASM untouched.
  - **Verified live:** two miners (Linux→Alice, Windows→Bob) each credited their own block rewards.
- **Still open (Phase 2):** implement the real tier-weighted `distribute_staking_rewards` (the
  actual "staking increases your reward" payout), and (per design) a **native challenge verify**
  for marketplace capacity tiers. See `design-verifiable-gpu-proof-2026-10-06.md`.

---

## 6. AI compute marketplace

- **Pallet:** `compute-marketplace`. Job state machine:
  `Pending → Accepted → Running → AwaitingVerification → Completed` (+ `Cancelled`, `Failed`).
- **Per-job escrow, per-token pricing:** escrow = rate × max_output_tokens (**cap**); actual charge
  = rate × tokens used; **surplus auto-refunds**. Priced in dollars, settled in GPN
  (`gpn_per_token = usd_per_token / token_price_usd`) so dollar cost is stable while GPN floats.
- **Cost-derived rate** (roofline): `min(TFLOPs·eff/(2·params), BW·eff/(2·bytes))`
  ÷ `(tok/s × 3600 × utilization)` × (1+margin). Target ~**50% of market GPU-rental**, clamped ≥ cost.
- **Fee:** `MarketplaceFeeBps = 500 (5%)`; provider gets price − fee.
- **Providers:** register (attested, min_gflops gate) → accept → start → run (real GPU) →
  submit result → complete → escrow payout. `provider = price − 5% fee`.
- **Job-decryption/privacy:** Q&A off-chain; on-chain salted commitments.

### 6.1 Who runs a job — the routing (access point → scheduler → one provider)

Flow: `customer → access point (gpn_scheduler, e.g. :8092) → scheduler picks ONE provider →
writes an assignment → that provider claims + runs it`. The chain stays the escrow/capability layer.

**Selection (`gpn_scheduler::pick`), in order (updated 2026-10-06):**
1. **Capability filter** — keep providers with a **fresh heartbeat**, `gflops ≥ job.min_gflops`, **and**
   (if the job sets a hardware floor) enough **VRAM/RAM**: a fail-closed `min_vram_mb`/`min_ram_mb` gate
   (#14, 2026-10-08) excludes providers advertising less — or nothing. (Scheduler reads all this from the
   off-chain `/status` heartbeat; the **chain** enforces `min_gflops` at `accept_job` — the on-chain
   VRAM gate is a carry-forward for a later `set_code`.)
2. **Availability** — a node **currently running an AI job** (`active > 0`) is **NOT available**
   (one GPU does one job at a time). Only **idle** nodes are candidates.
3. **Model match** — among idle nodes: *ready* = has the requested model (or any model if none
   requested); else **pull-eligible** (`can_pull: true`).
4. **Pick = lowest latency (least-ms).** Idle+ready nodes are sorted by `latency_ms` ascending; the
   lowest wins. **No load-weighting.** (Ready-vs-puller: a puller wins only if
   `latency + pull_penalty < ready.latency`.)
5. **All busy → queue.** If no idle compatible node exists, fall back to the least-loaded, then
   lowest-latency node — the job **waits** (it is *not* run concurrently).
6. **Assign + DISPATCH** — write `{job_id: {provider, model, pull}}` to the local assignment store
   **and PUSH it over HTTP** (`POST /assign`) to the chosen provider's heartbeat, which writes it into
   the provider's **own** local file (#4 fix, 2026-10-08 — so **remote** providers get assignments too).
   Provider **discovery is on-chain** (#1, spec 12): the scheduler builds its registry from
   `ComputeMarketplace::ProviderEndpoints` ∪ the static `--providers` fallback.
7. **On-chain** — escrow + `accept_job` capability gate (min_gflops) enforced by the marketplace.

**Dual path:** if a provider has **no** assignment, it will still claim **any compatible `Pending`
job** (first-come). So routing is primarily **scheduler-assigned**, with an open **first-claim**
fallback.

**Decentralized:** access points are permissionless — anyone can run a `gpn_scheduler`; they share the
same on-chain state, so no single AP is required (redundancy + geo-latency).

**Scaling & caps (reality, 2026-10-06):**
- **Nodes / miners: NO maximum.** Permissionless PoW — anyone can join and mine; the chain imposes no
  node cap. Adding miners just raises total hashrate (difficulty retargets).
- **Providers the scheduler routes to:** the scheduler polls a **configured `--providers` list** of
  `/status` heartbeat URLs — it does **not yet auto-discover** providers from the chain. So today it
  tracks exactly the endpoints it's given. No protocol cap, but an operational limit; multi-access
  point + on-chain provider discovery is a design item (`design-access-points-scheduler.md`), not built.
  (Note: the scheduler filters capacity using the heartbeat's **advertised** gflops; the **chain** is
  what actually enforces `attested_gflops ≥ min_gflops` at `accept_job`.)
  — Per the design, these **are** the same nodes (a node is a provider, §3.1); the separate provider
  list is transitional.
- **On-chain caps that DO bound throughput:** `MaxActiveJobs = 100` (global concurrent unfinished jobs
  — the real network-wide bottleneck), `MaxJobsPerProvider = 10`, `MaxOracleOperators = 10`
  (launching with 5 operators), `MaxJobDuration = 100_000` blocks. Raising `MaxActiveJobs` (and adding
  a per-requestor cap) is a known open item.

**Entry path & the “root node is always first” trap:** the intended topology is
`user → nearest access point → scheduler → best provider`, where **anyone can run an access point**,
an access point does **not** have to be a provider (and vice-versa), and routing is by score
(least-loaded, then lowest latency) across providers **discovered from the chain**. Today's testnet
**collapses** this: the only public entry is an access point running **on the root node**, and its
provider list is a **static local URL** (`--providers http://127.0.0.1:8093`) — so the root node is
gateway + sole provider and every job routes to it. That's a **bootstrap artifact, not the design**.
To decentralize: (a) let others run access points (and/or move ours off the root node), (b)
**discover providers on-chain** instead of a static list, (c) keep AP and provider roles independent.
Latency-based routing *should* favor the provider nearest the *user's* access point — but on a
one-box network that is trivially the founder's machine.

**Latency / “connect to the lowest-ms node”:** routing now picks the **lowest-latency (least-ms)
idle node** — a node running a job is **excluded** (unavailable), and there is **no load-weighting**.
Caveat: the latency measured is the **access-point → provider** RTT, **not** the **user → provider**
RTT. To make the *user's* path genuinely lowest-ms: run **multiple geo-distributed access points**
(user hits the nearest), have the AP **factor the client's RTT**, and/or let the **client** measure
RTT to each provider/AP and choose. (Routing discipline changed 2026-10-06 per Jeremy: least-ms, busy
node unavailable.)

**Provider heartbeat:** `GET /provider/status` → `{peer_id, attested_gflops, models[], gpu,
current_jobs, capacity, can_pull, last_seen}` (+ measured `latency_ms`). The scheduler polls these
into a registry each cycle.

### 6.2 Job lifecycle gaps & fixes
- Root incident 2026-10-05: `TooManyActiveJobs` — `ActiveJobCount` is a **global** count of all
  non-terminal jobs; Pending jobs only exit via the **requestor** cancelling; no expiry/timeout;
  `Failed` was dead code. → Self-inflicted DoS.
- **Shipped:** AP rejects unservable models; `gpn_cancel_jobs` drain; AP **stale-job janitor**
  (cancels the AP's own Pending jobs after a **token-scaled** TTL, capped 15 min).
- **Applied (spec 7/8):** `fail_job` (refund escrow on failure, wire `Failed`, decrement count);
  permissionless `expire_job`. See `job-lifecycle-gap-analysis-2026-10-05.md`.
- **Open:** per-requestor active-job cap; on-chain expiry sweep.
- **Id-consumed-before-escrow — FIXED + STAGED (red-team finding #25, 2026-10-09; spec 32).**
  `create_job` bumped `NextJobId` (and `open_channel` bumped `NextChannelId`) **before** the fallible
  escrow `Currency::transfer`; FRAME does not roll back storage on an `Err`-returning dispatch, so a
  failed create/open still consumed an id. Since the access point seeds its off-chain job-id
  predictor from `NextJobId`, one failed create desyncs the predictor from the chain by one and the
  next real job is labeled with a stale id (the on-chain/off-chain mislabel → orphan class, items
  (a)/(d)). Any account can trigger it (`create_job` with an unaffordable `price`) ⇒ non-root
  griefing/DoS. Now the escrow transfer runs first and the counter/record are written only on
  success. Staged artifact `wasm/gpn_runtime_spec32_redteam_createid_atomicity…` (spec 32; **not
  applied** — chain-wide = review-first; on-chain is 22).

### 6.3 Known ops facts (2026-10-06)
- AP requires **`Authorization: Bearer <GPN_AP_TOKEN>`** (or `X-Api-Key`) on **non-loopback**
  POSTs; loopback is exempt. Token lives in `~/.gpn/accesspoint.env`. (Value is a secret — read
  from that file, don't hardcode.)
- Image gen end-to-end works after restarting a **wedged** provider (`systemctl --user restart
  gpn-diffusion`); a stale provider instance can sit idle (its `list_claimable_jobs` returns nothing
  if its attestation/state is stale) and jobs get reaped by the janitor.

---

## 7. Tokenomics (`design-tokenomics-spec.md` is the source-of-record)

- **Token:** GPN, 12 decimals, 1 GPN = 1e12 raw. **Fair launch via mining** (no ICO, no liquid
  premine). Hard cap **1,000,000,000 GPN**.
- **Emission:** all GPN minted by `gpn-token::mint_block_reward` on block seal. **Halving is the
  sole emission** (every 4,204,800 blocks — **~2 yr at the 15 s cadence**, restored 2026-10-08); the
  vestigial 10%→1% inflation curve was removed 2026-09-16.
- **Block reward / block time coupled:** `emission_rate = reward / target_block_time`. Current:
  **50 GPN / 15 s ≈ 3.33 GPN/s**. `TargetBlockTime` is the shared runtime const → mainnet uses the
  same 15 s cadence. **(D1 RESOLVED 2026-10-08: reverted 4→15 s. The ~520 M effective cap is a
  reward × interval fact — see below — not a cadence issue.)**
- **Supply split:** cap is nominally 1 B, but the implemented emission path makes **max mineable
  ≈ 420 M** (2 × 50 GPN × 4,204,800) → **effective max supply ≈ 520 M** (+100 M treasury). The old
  "900 M mineable" figure was **never reachable** with these params (D1; docs corrected). Treasury
  **100 M (10%)**: 70 M genesis-unlocked + 30 M vested
  over 10 yr (20 × 1.5M / 6mo), held by `gpn/trsy` pallet account, spendable only via
  **13-of-25** governance roster.
- **Fee distribution** (`distribute_fees`, must sum to 500 bps = 5%): **Staking pool 200 (40%) ·
  Treasury 150 (30%) · Burn 100 (20%) · Oracle operators 50 (10%)**.
- **Initial token price anchor:** oracle `token_price_usd = $0.15` (owner decision 2026-09-15);
  cost-of-production floor at launch ~$0.001–0.02. Value ultimately derives from **AI-marketplace
  demand**, not mining economics.
- **Open:** resolve burn wiring (share logged, not actually removed); implement staking-reward &
  oracle-reward distributions; decide capped vs disinflationary.

---

## 8. Liveness / self-organizing full-node layer (`pallet-gpn-liveness`, implemented)

- **Problem:** bootnodes (always-on, public IP) are an unrewarded cost; tiers measured hardware, not
  service.
- **Design:** permissionless reachability oracle. Register endpoint + bond → **peer-witness** within
  a `LivenessWindow` (≥K distinct bonded witnesses) → `submit_liveness` refreshes `active`/uptime.
  Misses → **10% bond slashed**. **Silver/Gold gated behind active full-node status + uptime**
  (≥90%/95%); Bronze stays open.
- Retires the founder-VPS need once ≥2–3 independent full nodes exist.
- Open: D-L1..D-L5 defaults (bond size, K/window, uptime thresholds, Bronze gate, bootnode reward).

---

## 9. Network identity & fork tolerance (Jeremy-approved 2026-10-02)

- Identity is explicit: every node prints/exposes **(chain-id, genesis hash, spec_version)**.
- Protocol-version handshake rejects incompatible/outdated nodes.
- **Warn loudly, don't forbid** (no "0-peer mining ban" — would break dev/private nets).
- Canonical registry (`network.json`): `{chain_id, genesis_hash, spec_version, bootnodes[],
  latest_version}`, embedded; node reports MATCH / DIFFERENT NETWORK / OUTDATED.
- **Value starves forks** — no listings/liquidity ⇒ fork token worthless.

---

## 10. Cross-chain bridge (`design-crosschain-bridge.md`)

A design for bridging GPN ↔ other chains (locked/escrowed on one side, minted on the other, with a
relayer/committee). Not yet built; see the artifact for the full design.

---

## 11. Governance, safety & HARD RULES

- **Committee governance:** Phase-B committee; the 2-of-3 committee proposes/executes **Root** calls
  after a timelock (no single key acts alone). Mainnet sudo is **inert (all-zeros)** at launch —
  the committee *is* governance.
- **Genesis is IMMUTABLE:** never change the genesis hash, genesis WASM, chain-spec difficulty, or
  supply. **Never regenerate the canonical `gpn_runtime.compact.compressed.wasm`.**
- **Runtime changes go via forkless `set_code`** with a **separately-built** WASM (bump
  `spec_version` above the on-chain value; move the canonical aside to build, then restore it).
  If a change would reset the chain → **STOP + ask Jeremy**.
- **Review before change:** log WHAT/WHY/RISK/ROLLBACK, reviewed against `goals-and-guidelines.md`
  (`REVIEW-PROTOCOL.md`); escalate to Jeremy only for genuinely undecidable, high-blast-radius items.
- **Root-cause first:** no blind restarts, no retry loops, no state wipes as a first response.
- **Dispatch rollback (red-team methodology, 2026-10-09):** `#[pallet::call]` wraps every dispatchable body in `frame_support::storage::with_storage_layer`, so **any `Err`/panic reverts that call's storage writes** — the “state mutated before a fallible op” class is a **non-issue for extrinsics** (it only matters in `on_initialize`/`on_finalize` hooks, which are *not* wrapped). Findings #25/#33 assumed otherwise; their premise was false (fixes harmless; #33's also fixed a genuine functional payout bug).
- **Small, reversible changes**; build clean; test; commit; revert on failure.
- **Locked launch decisions (2026-09-15):** D1 closed set of 5 oracle operators (seed all at
  genesis; no root `add_operator`); D2 committee governance; D3 on-chain median feed (client-side
  median fallback, since Phase-A operators don't feed the on-chain bonded median); D4 10% slash
  BURNED; D5 fresh offline sudo key (testnet) + separate hot faucet key.

---

## 12. Infrastructure & operations (current)

- **Host:** Jeremy's **static-IP machine** (self-hosted, $0; public IP `142.129.54.88`;
  duckdns `gpn-testnet.duckdns.org`, updated by cron). GPU: **RTX 4060**.
- **Services (systemd user units):** `gpn-fleet` (node via `run-fleet.sh`), `gpn-accesspoint`
  (`gpn_scheduler` :8092), `gpn-provider`/`gpn-diffusion`/`gpn-provider-heartbeat` (:8093),
  `gpn-explorer`, `gpn-faucet`, `gpn-ap-topup.timer`.
- **Ports:** p2p **30333**, RPC **9944**, AP **8092**, provider heartbeat **8093**.
- **Bootnode:** peer id `12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo`;
  `/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooW…`.
- **Chain:** on-chain **spec 38** (2026-10-09). History: **spec 13** = channels +
  capacity challenge + per-requestor cap + expiry sweep + root-only distributions · **spec 14** =
  block target 4→15 s + reward restored to 50 GPN · **spec 15** = shielded **spend** path (VK + `spend`
  + root history) · **spec 16–38** = the red-team sweep (min-GPU floor + ~31 findings: escrow theft-of-service, unvalidated governance/threshold writes, `submit_solution` PoW bypass, shielded pool drain/fee stranding, channel refund window, fork-choice, …).
  genesis `0xb8e4636a…` (canonical). **All applies authorized** (Jeremy green-lit the work; MAIN
  executed per REVIEW-PROTOCOL). Node rebuilt + restarted so its native runtime matches.
  **Ongoing:** the red-team keeps finding → latest staged artifact applied each time (`wasm/README-pending-upgrade.md` is the canonical pointer).
- **Repos:** `~/.openclaw/workspace/projectmanager07` (crates `gpn-node/`, `gpn-agent/`);
  remote `https://github.com/jeremyzguo/gpn_coin.git` (branch `main`). PAT retrievable at
  `~/.openclaw/credentials/github-gpn-pat.txt` (git credential store configured).
- **Keys:** `governance-keys.txt` (sudo, faucet, committee-1..3, reward-wallet);
  `~/.gpn/accesspoint.env` (`AP_SEED`, `GPN_AP_TOKEN`); `~/.gpn/node-key`.
- **Nodes/payouts:** Linux node → **Alice** `5GrwvaE…` (attested, mining). Windows box → **Bob**
  `5FHneW46…` — as of 2026-10-08 it runs as an **AI provider** (LLM; port **8093** forwarded) rather
  than mining; it's on-chain-discoverable (`ProviderEndpoints`). Beneficiary = reward wallet `5D1sq…`.
- **Windows run:**
  `.\gpn-node.exe --chain C:\gpn\testnet-spec.json --bob --force-authoring --node-key 8f9e… --rpc-port 9944 --bootnodes $BN`
  (needs the **chain-spec file**, not `--chain testnet`; `--force-authoring` to mine; §13).

---

## 13. Gotchas learned the hard way

- **Genesis mismatch:** a Windows build embedded a **different runtime WASM** → different genesis
  (`0x07f8…` vs canonical `0xb8e4…`) → couldn't peer. Fix: run with the **exported chain-spec file**
  (`--chain testnet-spec.json`) so the genesis `:code` is pinned. AP serves it at `GET /genesis-spec`.
- **Fork poisoning:** a node started with `--force-authoring` **mines from genesis before syncing** →
  announces a private fork → the main chain rejects those blocks → drops the peer → flapping.
  Fix: **sync first without `--force-authoring`**, then restart WITH it.
- `spawn_miner` is called **only** under `if force_authoring` (verified in `service.rs`).
- **Attestation staleness:** providers whose attestation is old can appear "up" but claim nothing.
- **Post-upgrade client stall:** a long-lived client (AP/providers) keeps stale subxt metadata after a
  `set_code` → its txs get dropped + the pipelined nonce diverges → creates stop landing. Fix
  (`5289ce5`): the submitter detects the spec change + re-seeds; ALSO restart dependent clients after
  any runtime upgrade.
- **Remote-assignment gap:** the AP assigned jobs only in its host-local file → **remote** providers
  never claimed them. Fix (`0b9977a`): the AP **pushes** the assignment over HTTP (`POST /assign`) to
  the provider's heartbeat, which writes the provider's own local file.
- **Wedged provider:** a stale provider process sits idle; a restart clears it.
- **Janitor:** the AP **auto-cancels its own stale Pending jobs** (token-scaled TTL) — jobs can
  vanish before a slow provider claims them.
- **GPU contention (transitional):** on the current testnet the node's internal miner and a separate
  provider process can contend for the one GPU (image gen was slow while the miner held the GPU). By
  design this **shouldn't** happen — a node runs ONE mode at a time and the **auto-switch** (§3.1)
  stops the miner *before* compute runs. Wiring the node-internal miner into that switch is pending.
- Node peer count takes **~40s to establish after a restart** (0 → 1 peers is normal, not a fault).

---

## 14. Current state (2026-10-08)

- **LIVE:** PoW chain at **spec 14**; **15 s cadence**, **50 GPN/block**; two miners historically
  (Linux→Alice; the Windows box now **serves AI** rather than mining) each keeping their own rewards
  (Bitcoin-style, verified). Stake-free, PoW-verified GPU-only mining gate (spec 11). LLM + image +
  ML + fine-tune job pipelines functional; AP with token auth + janitor + **on-chain provider
  discovery** + **HTTP assignment dispatch** + **VRAM/RAM capability gate**; liveness pallet;
  channels + capacity challenge + per-requestor cap + expiry sweep + **root-only** distributions live
  (spec 13).
- **Windows box:** runs as an **AI provider** (LLM via ollama; **8093 forwarded**; on-chain-discoverable).
  Video + auto-switch pending; remote-assignment end-to-end verify pending.
- **Monitors:** silenced when healthy (§16). **Queue-driver cron** (`d496e0b0`) advancing `GPN-TASKS.md`.
- **Open:** shield **spend** path (#15, building); video on the Windows box (#5b); remote-provider
  end-to-end; the 15 s cadence transition (difficulty retargets automatically).

---

## 15. Roadmap / open items (status as of 2026-10-08)

**✅ DONE:** #1 on-chain provider discovery · #3 client-side RTT · #5a fine-tune provider · #5c
MLInference executor · #6 staking-weight + capacity challenge (applied) · #7 ms retarget (applied) ·
#8 clock/NTP warning · #9a submit auto-recover · #9b janitor→`expire_job` · #9c RPC-readiness gate ·
#9d per-requestor cap (applied) · #9e on-chain expiry sweep (applied) · #10 diffusion wedge watchdog ·
#11 root-only distributions (applied) · #14 VRAM/RAM capability gate.

**⏳ IN PROGRESS / BLOCKED:**
- **#2 off-chain escrow (channels)** — on-chain primitives + agent manager done + applied (spec 13);
  remaining = the **AP hub routing** (route jobs via channels so AI skips per-job block txs). Design:
  `design-offchain-escrow-channels.md`.
- **#4 Windows AI provider** — binaries + staging + dispatch fix done; remaining = redeploy the box's
  binaries, restart the provider, and **verify a job routes + completes + pays** end-to-end.
- **#5b video** — code fixed; needs the Windows box (running the test).
- **#15 shielded SPEND path** — building (sub-agent): on-chain note-spend VK + `spend` dispatchable +
  root history; runtime `set_code` staged for go-ahead.
- **#12 mainnet** — owner decisions **done** (D1 15 s + docs, D2–D6 accepted); remaining = genesis cut
  + launch (a later explicit go).
- **#13 bridge** — code complete agent-side; **post-mainnet** (Eth/Hyperlane deploy + liquidity).

---

## 16. Cron / monitors

- **PM07 monitors:** `1a4f19ab` (GPN provider debug monitor) + `95b27deb` (GPN driver), both every
  10 min → Discord. **2026-10-06:** updated to be **SILENT WHEN HEALTHY** (reply exactly `NO_REPLY`,
  no note, no change-log commit) — only message/log on a real error/anomaly or an approved change.
- Other GPN auto-fix-loop crons exist but are currently **disabled**.
- **GPN master queue driver:** `d496e0b0`, every 30 min (isolated session → Discord) — advances
  `GPN-TASKS.md` **one item per run**, **restarts dependent clients after any runtime upgrade**
  (rule 5a), marks un-solvable items **BLOCKED** + moves on, and is **silent when the queue is done**.
- **PM07 monitor crons previously emitted noisy `docs(change-log): monitor run` commits** — now
  instructed not to commit when nothing changed.

---

## 17. Document index (the GPN corpus, `shared/artifacts/project7/`)

**Design/spec:** `goals-and-guidelines.md` · `launch-decisions-locked.md` ·
`design-tokenomics-spec.md` · `design-pricing-model.md` · `design-access-points-scheduler.md` ·
`design-model-aware-scheduling.md` · `design-network-identity-fork-tolerance.md` ·
`design-liveness-selforg-fullnode-tiers.md` · `design-crosschain-bridge.md` ·
`design-per-token-escrow.md` · `design-treasury-vesting.md` ·
`design-verifiable-gpu-proof-2026-10-06.md` · `spec-one-gpu-one-reward.md` ·
`spec-per-miner-wallets.md` · `spec-fleet-control.md` · `marketplace-sla-reputation-proposal.md` ·
`job-lifecycle-gap-analysis-2026-10-05.md` · `multi-miner-bitcoin-style-plan-2026-10-05.md` ·
`merge-blockproduction-reward-bitcoin-plan-2026-10-06.md` · `economics-gpu-mining-and-gpn-valuation.md`.
**Ops/runbooks:** `testnet-public-runbook.md` · `testnet-status.md` · `operator-onboarding.md` ·
`USE-GPN.md` · `production-readiness-checklist.md` · `production-readiness-workplan.md` ·
`linux-install-guide.md` · `windows-client-test.md` · `REVIEW-PROTOCOL.md` ·
`competitive-landscape-review-2026-10-03.md` · `gpn-differentiation-and-advantage-2026-10-03.md`.
**Logs:** `change-log.md` (running ops changelog) · `cycle*-*.md` (historical cycle logs).
**Live ops/tracking:** `GPN-MASTER-REFERENCE.md` (this doc) · `GPN-TASKS.md` (the cron-driven task
queue) · `mainnet-open-decisions-memo.md` · `design-offchain-escrow-channels.md` ·
`access-point-install-guide.md` · `wasm/` (staged spec-N upgrade artifacts).
**Marketing:** `marketing/` (brand, content, GTM, listings) + `marketing-plan.md`.
