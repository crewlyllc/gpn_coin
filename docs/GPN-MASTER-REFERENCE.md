# GPN — MASTER REFERENCE (single-document review)

**Project:** Project 7 — GPN, a GPU-native crypto + AI-compute network
**Last updated:** 2026-10-22 · **Owner:** main / PM07
**Purpose:** One place to review *everything* decided about GPN — vision, goals, architecture,
tokenomics, mining, attestation, marketplace, governance, infra, current state, and open items.
Synthesized from the full `shared/artifacts/project7/` corpus + session history (see §17 doc index).
**If this doc and the code disagree, the CODE wins** (this is a guide, not the spec-of-record).

> ⚠️ **Why this exists:** too many design details were spread across dozens of artifacts, and some
> goals got forgotten mid-design. This is the canonical "read me first" for GPN.

---

## 0. Delta since 2026-10-20 (read this first if you read the doc before)

Status labels used throughout: **LIVE** · **STAGED (not applied)** · **WORKS-INTERNALLY** · **BLOCKED** · **GAP**.

| Change | Status | Where |
|---|---|---|
| **Auto-update now ON by default** (opt out: `GPN_AUTO_UPDATE=0` / `auto_update=false`); signed manifest, bundle installs, per-node randomized schedule, install stagger | **LIVE** (shipped in 0.2.0; not yet on every box) | §9.1 |
| **AP auth token self-generates on first boot** (24 random bytes → 48 hex, `~/.gpn/accesspoint.env` 0600; env > file > generated) | **LIVE** | §6.4 |
| **Public AI API over HTTPS** (`https://gpncoin.ai/api/v1/*`, Cloudflare Worker → AP raw socket, rate-limited, free on testnet) | **LIVE** | §6.4 |
| **Curated + VRAM-gated model catalogue** (4 served on an 8 GB node; `tuned-*`/toy/oversized never advertised) | **LIVE** | §6.4 |
| **Provider address publication fixed** (doubled `/p2p/<id>/p2p/<id>` normalized; 4-rung dialability ladder; empty publication logged) | **LIVE** | §6.4 |
| **Node GUI + Idle mode** (`127.0.0.1:8099`, up to 6 h, auto-resume, sole-authority valve) | **LIVE** | §9.2 |
| **Derived GPU-hour pricing** in the scheduler + a public economics page | **LIVE formula, GAP inputs** (no oracle feed, no input metering) | §7.1 |
| **Governance hardening (spec 42)** — 5-member/3-of-5, expiry, cancel, preimages, weights, migration | **STAGED, NOT APPLIED** — chain is still 3/2-of-3/3-day/no-expiry/zero weights | §11.2 |

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
- **Off-chain agents (`gpn-agent`):** provider (`gpn_llm_provider`, `gpn_diffusion_provider`,
  `gpn_ml_provider`, `gpn_finetune_provider`), miner (`gpn_external_miner`), access-point/scheduler
  (`gpn_scheduler`, which also generates its own auth token — `ap_token.rs`), the supervisor
  (`gpn_node`: auto-update, GUI, idle), ops tools (`gpn_upgrade_runtime`, `gpn_gov_upgrade`,
  `gpn_send`, `gpn_attest`, `gpn_seedlist`, …).
- **Shared provider policy modules:** `gpn_agent::models` (VRAM fit + public catalogue) and
  `gpn_agent::seedlist::clean_published_multiaddr` (one multiaddr sanitizer used by both the
  heartbeat and the AP) — the two places where what a provider advertises and what the AP accepts
  must agree.
- **Split:** on-chain = jobs, escrow, attestations/capabilities, payouts (source of truth).
  Off-chain = latency, live load, endpoint discovery, the actual AI compute.
- **Membership (decision 2026-10-19, v2 — `design-network-membership-bootstrap.md`):** **every node is
  a backbone node / seed BY DEFAULT** (opt-OUT, not opt-in) — a node announces its endpoint, is
  discoverable and accepts inbound `30333` unless the operator passes `--no-seed` / `--no-discoverable`
  (`GPN_DISCOVERABLE=0`). Dialability is **never a gate on joining**: sync, mine and serve AI jobs work
  behind NAT. If the node cannot become dialable it **auto-degrades to a non-discoverable participant
  with a one-time, actionable warning** (port-forward instructions, CGNAT explanation) — it never exits.
  Startup tries explicit `GPN_PUBLIC_ADDR` → inbound probe → **UPnP-IGD/NAT-PMP (`GPN_UPNP=auto`)**, and
  only then degrades. `--require-backbone` retains the old exit-78 behavior for operators who must
  guarantee it. Rewards: non-discoverable = base block/job pay only; backbone = base + `gpn-liveness`
  credit → Silver/Gold staking multiplier; seed = backbone + routing tie-break (badges are derived from
  peer witnesses, never self-reported). Consequence: the AP **never dials a provider**; jobs are
  **pulled** (`GET /assignments` long-poll + lease) and results are pushed out (`POST /result/{id}`,
  unchanged). **Bootstrap ships TWO sources in every build:** (A) a hardcoded list of **≤5 seed node IP
  addresses** (`network.json` → `bootstrap_seeds[]`, compiled in — the no-DNS/no-HTTP/no-chain floor,
  each entry `ip4` + fixed `peer_id` + optional `dns4` alias) and (B) a **DDNS/bootstrap list** of URLs
  and DNS records (`bootstrap_urls[]` → `https://seeds.gpn.network/seeds.json` + `seeds2.gpn.network`,
  plus `_gpn-seeds._tcp.gpn.network` TXT) giving the **current** live node list so it can change without
  a client release. Also unioned: on-chain registry + local `peer-store` (existing `discovery.rs`).
  `seeds.json` is published from liveness rankings by `gpn_seedlist`; the hardcoded 5 are a separate,
  deliberately stable, reviewed set (see `GPN-TASKS` #46).

### 3.1 A node is ALWAYS both — mine by default, yield to a job (one GPU, one job)

**A GPN node is ONE entity: a miner AND an AI provider, always** — there is **no role/opt-in**. (The
supervisor `gpn_node` starts the chain node + the provider heartbeat + the provider loop together;
mining is **structural** — the node forces `Role: AUTHORITY` so a plain `gpn-node`/`gpn_node` mines
with **no** `--force-authoring` flag.)

**⚑ “One GPU, one job” = MUTUAL EXCLUSION, not a restriction on job types.**
> The single GPU runs **exactly ONE job at a time** — never two concurrent — so it never
> over-subscribes its compute/VRAM. It does **NOT** mean only some job types exist: **every node is
> equipped by design to serve EVERY job type** — LLM chat · image · video · ML inference ·
> fine-tune — and **advertises all of them**. The scheduler routes each incoming job to a node that
> can serve it; that node runs that **one** job to completion (mining paused) before accepting
> another. All capabilities are available on all nodes by default (per-capability opt-out exists for
> constrained boxes, but the default is “everything available”).

**The one-GPU rule — the GPU does one thing at an instant:**
- **Mining is the default.**
- When a node **accepts an AI job, mining FULLY STOPS** for the job's duration (the provider holds a
  `~/.gpn/gpu-yield` flag; the miner's loop does **zero** nonce-search while it's set) and **resumes the
  instant the job completes**.
- **One job at a time per machine** — provider **capacity = 1** (the scheduler excludes a node already
  running a job).
- Modes: **Mining · Compute** (Staking = Phase C).

**Current mechanism = a yield/preempt, NOT economic switching.** What's live is *“mine until a job →
stop → resume.”* The **profitability auto-switch** — `ModeManager::switch_to` + `decide_switch` over the
oracle feed (USD/hr per mode, ~5% hysteresis, real GPU stop/start) — is **designed but NOT wired** for
the node's internal miner; it's the deferred “no profitability math yet” step (Jeremy 2026-10-13).
Verified live: mining stops while a job runs and resumes after; capacity=1.

*(Historical note: earlier drafts said the miner “yields but still mines once per cycle” — that
confirmed a duty-cycle, now replaced by a **full stop** (#24).)*

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
  the 4 s era). **Halving every 4,204,800 blocks (~2 yrs at 15 s).** **Max mineable = 2 × reward ×
  interval = 420.48 M** (+100 M treasury ⇒ **~520.5 M hard supply cap**) — see §7 / D1.

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
- **Membership is NOT gated (2026-10-19, v2):** Bronze is full participation, not a consolation tier —
  a NAT'd, outbound-only node syncs, mines and serves AI jobs with no inbound port. **Backbone/seed is
  the DEFAULT posture** (opt-out): the node tries to be dialable — explicit addr, inbound probe,
  UPnP/NAT-PMP — and only becomes non-discoverable if it can't be, printing a clear warning rather
  than failing. Backbone (dialable + witnessed uptime) unlocks Silver/Gold gating, seed-list
  eligibility and a routing tie-break; it is never a requirement to join. The old
  `GPN_REQUIRE_DIALABLE` hard gate (queue #45) is demoted to `--require-backbone`;
  `--no-seed`/`GPN_DISCOVERABLE=0` opt out of discoverability. See
  `design-network-membership-bootstrap.md` §2, §5.
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

> **Decision 2026-10-19 — transport is PULL (design: `design-network-membership-bootstrap.md`).**
> The AP must **not** need to reach the provider. Step 6 below (`POST /assign` PUSH to the provider's
> `:8093`) is replaced by: the provider long-polls `GET /assignments?provider=…&since=…`, receives a
> **lease** (`lease_id`, full job payload, `callback` + `callback_token`, deadline), claims on-chain
> (`accept_job`), and pushes the result **out** to `POST /result/{id}` (unchanged). The AP's
> load/latency view moves from polling `:8093/status` into `caps` carried by the poll itself
> (RTT is provider→AP), so `ProviderEndpoints`/`endpoint` becomes **optional backbone metadata** and
> must never be required to route. `POST /assign` survives only as a best-effort **wake hint**, and
> `dispatch_assignment` failing is no longer a lost job. The existing first-claim fallback below is
> kept — it is already a pull path and makes mixed fleets safe. Migration is flag-gated
> (`GPN_JOB_TRANSPORT=push|pull|both`, `GPN_ASSIGN_SOURCE=push|pull|both`).
>
> **Bootstrap note (v2):** routing must also survive an empty/slow seed layer. Every build ships the
> hardcoded ≤5-seed IP list (`network.json:bootstrap_seeds`, no DNS needed) **and** the DDNS/HTTP list
> (`bootstrap_urls` + `_gpn-seeds._tcp` TXT) telling it the **current** node set; both are best-effort
> and non-fatal, and a returned list is dialed in full (shuffled), never first-only.

**Selection (`gpn_scheduler::pick`), in order (updated 2026-10-06):**
1. **Capability filter** — keep providers with a **fresh heartbeat**, a **verified** `gflops ≥ job.min_gflops`, **and**
   (if the job sets a hardware floor) enough **VRAM/RAM**: a fail-closed `min_vram_mb`/`min_ram_mb` gate
   (#14, 2026-10-08) excludes providers advertising less — or nothing. (Scheduler reads all this from the
   off-chain `/status` heartbeat; the **chain** enforces `min_gflops` at `accept_job` — the on-chain
   VRAM gate is a carry-forward for a later `set_code`.)

   **What `gflops` means (item #97):** the canonical unit **`GPN-GEMM-F32-1024`** — measured dense FP32
   GEMM throughput (N = 1024, `2·N³` FLOPs per run, best of 3 idle runs), *not* spec-sheet TFLOPS. It
   reaches ~5–7 % of a card's FP32 peak: an RTX 4060 (15.1 TFLOPS peak) measures 823–912, an RTX 5070 Ti
   ~2 300–2 650 (estimate). The eligibility floor is **250 GFLOPS** (30 % of the weakest supported card's
   measurement), replacing the old `3000` that was written on the peak scale and which no consumer card
   could ever clear — **staged for the next runtime upgrade (spec 44); the chain still enforces 3000
   until it enacts**, so a fresh attestation below that is rejected (`BelowEligibilityFloor`) and the
   box keeps its old record. `30000` on a 15 TFLOPS card is a pre-2026-10 unit bug, not a fast GPU.

   **Only `gflops_source: "attested-idle"` counts.** A hand-typed `--gflops`, an older chain record or a
   contended sample is **unknown**, never evidence: the AP will not admit *or* exclude a box on it. With
   a verified candidate present, unknown-source boxes serve ungated jobs only; with none, they are
   appended as flagged last resorts (`gflops_unverified`) and the chain still rejects `accept_job` if
   their attestation is short. Every such decision is logged, never silent. A record above 1.5× the
   card's estimated FP32 peak is replaced by a fresh measurement regardless of age; `GPN_ATTEST_FORCE=1`
   forces a re-measure for a wrong-but-plausible record.
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
  (Note: the scheduler filters capacity using the heartbeat's **advertised**, `attested-idle`-sourced
  gflops; the **chain** is what actually enforces `attested_gflops ≥ min_gflops` at `accept_job`.)
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

### 6.4 Public API, model catalogue, address publication (2026-10-21)

**AP token — generated, not hand-rolled (`gpn_agent::ap_token`).** The AP always *required* a token,
but nothing produced one; an install that skipped the step ran a public job API with an empty token
and the non-loopback gate silently off. `gpn_scheduler` now resolves it at boot: **env var > env file
(`$GPN_AP_TOKEN_FILE`, default `~/.gpn/accesspoint.env`) > generated**. Generation = 24 `getrandom`
bytes → 48 lowercase hex, persisted atomically (temp + rename) at 0600 with the file's other
variables preserved, `~/.gpn` created 0700. The full value is printed **once** (with the instruction
to set the Worker's `AP_TOKEN`); later boots print `AP token loaded from <file> (first8…)`. An
explicitly supplied token is never overwritten. A corrupt file is **quarantined**
(`.corrupt.<epoch>`), not shredded; an unwritable path degrades to an ephemeral token with a loud
warning — startup never fails on this. 18 tests.

**Public HTTPS API (`worker/`) — LIVE.** `https://gpncoin.ai/api/v1/*` proxies to the AP
(`GET /v1/models`, `POST /v1/chat/completions`, `/api/v1/models`, `/api/providers`, `/api/mode`,
`/api/jobs/<id>`, `/api/health`). It exists because the AP speaks plain HTTP on 8092 (mixed content)
and Workers `fetch()` refuses non-standard ports — the Worker terminates TLS and speaks raw HTTP/1.1
over `cloudflare:sockets`, adding the Bearer token. Rate limits at the edge: **chat 12 / 10 min / IP**,
reads 120/min, typed `gpn_rate_limited` errors. **Testnet inference is free.** Measured e2e is tens of
seconds (34.7 s on 2026-10-22), dominated by the on-chain create-inclusion wait (#49/#52), ~1 s of
which is inference.

**Model catalogue — curated + VRAM-gated (`gpn_agent::models`) — LIVE.** Three gates: (1) **VRAM fit**
from a weight table with 1.15× headroom, applied at advertisement (`/status models[]`), at claim
(`resolve_model`), and at AP routing (`pick()`); (2) **public allowlist** of 16 assistant-grade
families (prefix-matched) for `/v1/models`; (3) **hard exclusions** even under `GPN_PUBLIC_MODELS=all`:
`tuned-*`, sub-0.3B toys (`stories260K` = 0.00026 B), `*embed*`/`*rerank*`. Excluded ≠ unusable — a
model named explicitly is still served; the catalogue is a shop window, not a permission system.
**On the 8 GB reference node this yields 4 served models** (`llama3.2:1b`, `qwen2.5:0.5b`,
`qwen2.5:7b`, `deepseek-r1:7b`); `deepseek-r1:14b` (~10.3 GB) is dropped. Overrides:
`GPN_PUBLIC_MODELS`, `GPN_MODEL_VRAM_HEADROOM`, `GPN_MODEL_VRAM_OVERRIDE` (`model`, `model=GB`, `all`).

**Address publication — fixed 2026-10-21.** The node RPC (`system_localListenAddresses`) returns
addresses already carrying `/p2p/<id>` and litep2p sometimes **doubles** it; the heartbeat relayed
that verbatim and the AP's sanitizer (correctly) rejected every entry, so a box published
`multiaddrs: []` and was unroutable. The heartbeat now strips identity and appends `/p2p/<peer_id>`
exactly once, using the **same** `clean_published_multiaddr` the AP gate uses. Ladder:
`GPN_PUBLIC_ADDR` → node-written `~/.gpn/dialability.json` → public listen address → STUN mapping on
the listen port; loopback/RFC1918/link-local/CGNAT/ULA never published; when all rungs fail the log
enumerates each rejection plus the fix. Empty publication is **not an error** (pull transport).
New `address_source` field reports which rung won.

**Deployment reality check (2026-10-22):** the live AP process on this host is still the **pre-fix**
build — `/v1/models` still lists `stories260K:latest`, `tuned-1184:latest`, `deepseek-r1:14b`. The
code is fixed and tested; the process needs a restart/redeploy before the public catalogue matches
this section. Do not describe the *live endpoint* as curated until that restart happens.

---

## 7. Tokenomics (`design-tokenomics-spec.md` is the source-of-record)

- **Token:** GPN, 12 decimals, 1 GPN = 1e12 raw. **Fair launch via mining** (no ICO, no liquid
  premine). Hard supply cap **≈ 520.5 M GPN** (≈ 420.48 M emission + 100 M treasury).
- **Emission:** all GPN minted by `gpn-token::mint_block_reward` on block seal. **Halving is the
  sole emission** (every 4,204,800 blocks — **~2 yr at the 15 s cadence**, restored 2026-10-08); the
  vestigial 10%→1% inflation curve was removed 2026-09-16.
- **Block reward / block time coupled:** `emission_rate = reward / target_block_time`. Current:
  **50 GPN / 15 s ≈ 3.33 GPN/s**. `TargetBlockTime` is the shared runtime const → mainnet uses the
  same 15 s cadence. **(D1 RESOLVED 2026-10-08: reverted 4→15 s. The ~520.5 M hard cap is a
  reward × interval fact — see below — not a cadence issue.)**
- **Supply split:** **hard supply cap ≈ 520.5 M GPN** — **420.48 M emitted** over the halving
  schedule (2 × 50 GPN × 4,204,800) plus **100 M allocated to the treasury**. Emission converges at
  420.48 M and the treasury allocation is fixed, so 520.5 M is the true maximum. Earlier
  (unreachable) cap and mineable figures were **never achievable** with these params
  (D1; docs corrected). Treasury **100 M**: 70 M genesis-unlocked + 30 M vested
  over 10 yr (20 × 1.5M / 6mo), held by a keyless `gpn/trsy` pallet account, spendable only via a
  **13-of-25** governance roster.
- **Fee distribution** (`distribute_fees`, must sum to 500 bps = 5%): **Staking pool 200 (40%) ·
  Treasury 150 (30%) · Burn 100 (20%) · Oracle operators 50 (10%)**.
- **Initial token price anchor:** oracle `token_price_usd = $0.15` (owner decision 2026-09-15);
  cost-of-production floor at launch ~$0.001–0.02. Value ultimately derives from **AI-marketplace
  demand**, not mining economics.
- **Open:** resolve burn wiring (share logged, not actually removed); implement staking-reward &
  oracle-reward distributions; decide capped vs disinflationary.

### 7.1 Derived pricing — what the code actually computes (2026-10-22)

The scheduler prices jobs from **throughput**, not from a competitor's price list or a GPU-rental market:

```
anchor_tok_s = roofline(30,000 GFLOP/s, 272 GB/s, 1B) = 95.2 tok/s
anchor_rate  = $0.10/hr                    # GPN_GPU_ANCHOR_USD_HR
k            = anchor_rate / (anchor_tok_s × 3600)   # $2.918e-7 / output token
price_out    = k × (1 + GPN_MARGIN_BPS/10000)        # 3000 → $0.379/1M
price_in     = price_out × GPN_INPUT_RATE_FRAC       # 0.25 → $0.095/1M
provider_hr  = roofline(gflops, bw, params_b) × 3600 × k
escrow       = prompt_tokens/4 + max_tokens          # output-equivalents
```

`gpn_scheduler` implements this in `gpn-agent/src/pricing.rs` (`PricingConfig::price_per_token_out`,
`provider_rate_hr`). The legacy market model is still selectable with `GPN_PRICING_MODEL=market`
(`GPN_RENTAL_FRACTION_BPS=5000`, $3.00 GPU-hour fallback, util 0.65) so the two can be A/B'd and reverted.

> **Unit note (#97):** the `30,000 GFLOP/s` anchor and the reference table are on the **spec-sheet
> peak** scale, while an attested `gflops` is the measured `GPN-GEMM-F32-1024` value (~5.5 % of peak).
> It does not move prices today because decode of a 1B-model is **memory-bound**: the roofline takes
> `min(compute, bandwidth)` and the bandwidth term wins at every attested value (823 GFLOP/s still
> gives ≥164 tok/s of compute head-room vs 95 tok/s of bandwidth). The provider's *share* is therefore
> set by `mem_bw_gbps`, and the measured value only matters for models big enough to be
> compute-bound — where the anchor should be compared as `measured / 0.055`. Treat the two numbers as
> different scales, never as the same quantity.

| GPU (1B stream) | Bandwidth | Throughput | Provider earns |
|---|---|---|---|
| RTX 4060 8GB (anchor) | 272 GB/s | 95.2 tok/s | **$0.100/hr** |
| RTX 4090 24GB | 1008 GB/s | 352.8 tok/s | **$0.371/hr** |
| A100 80GB | 2039 GB/s | 713.7 tok/s | **$0.750/hr** |
| H100 80GB | 3350 GB/s | 1172.5 tok/s | **$1.232/hr** |

Buyer price is **GPU-independent by design** ($0.379/1M output, $0.095/1M input); faster cards earn more
per hour. Throughput rather than FP32 GFLOPs is the metric because decode is memory-bandwidth-bound.
A worked 4060/1B example: `k = 0.10/(95.2×3600) = 2.918e-7`; ×1.30 → **$0.379/1M** (2.53 GPN at a
$0.15/GPN feed). The old `$3.00` market anchor gave **$8.75/1M** — ~22× `gpt-5-nano`. A derived price
outside **[$0.05, $5.00]/1M** refuses to publish unless `GPN_PRICING_UNSAFE=1`; the anchor and price are
logged at boot; a provider earning below its declared `owner_cost_usd_hr` is logged as losing money.
`GET /pricing` publishes anchor, `k`, both rates and this earnings table (60 s cacheable).

**Limits, both real:**
1. **Input metering is channel-first.** The channel path bills input at 25% of output and escrows
   `prompt/4 + max_tokens`. The pallet stores ONE `(price_per_token, max_tokens, actual_tokens)`, so
   on-chain jobs publish a blended rate — exact at full completion, slightly under-billing short ones.
   Exact two-rate on-chain settlement needs a pallet change (second rate in `JobBilling`).
2. **No on-chain price oracle feed.** The USD→GPN conversion falls back to `$0.15/GPN` (the legacy market
   model keeps its `$3.00` GPU-hour fallback). Batching raises provider earnings without changing the
   buyer's price.

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

### 9.1 Release channel, auto-update, minimum-version cutoff (`docs/updates.md`)

- **Auto-update is ON by default (opt out)** — `GPN_AUTO_UPDATE=0` or `auto_update = false`. The old
  "opt-in, off unless asked" text is superseded: the CLI help, the config default
  (`file.auto_update.unwrap_or(true)`) and the env helper (`auto_update_enabled(None) == true`) agree.
- **Manifest:** `https://gpncoin.ai/releases.json`, **ed25519** over a canonical line-oriented text
  blob (**not** the JSON) covering `version`, `min_version`, every asset URL/size/sha256. The public
  key is compiled in (`update.rs::RELEASE_PUBKEY_HEX`); a forged manifest is rejected **before**
  anything is downloaded. Private key offline at `~/.openclaw/credentials/gpn-release-key.txt`
  (0600); rotation requires a node release.
- **Bundle-aware installs:** a release is ~60 executables, so replacing only the supervisor would
  leave the children on other versions. Staged into `versions/<v>/`, identity proven from the
  **compile-time build stamp** (`gpn-build-v1`) plus per-file hashes, then an atomic `current` flip
  (symlink; junction on Windows), with `current.previous` as a one-step rollback. Only `gpn_node`
  carries the stamp (`build.rs` is in `gpn-agent`); other stamps in the tree must merely agree.
- **No stampede:** per-node **phase** = `sha256("gpn-update-phase-v1" NUL identity)` over the
  interval (+12 bits of restart bounce), **±5–10% jitter** per check, and an **install spread**
  `GPN_UPDATE_SPREAD_SECS` (default 1200 s, deterministic per node+version). 100-node simulation:
  87 distinct minute buckets, ≤5% of the fleet in any minute.
- **Cutoff:** below `min_version` → `UPGRADE REQUIRED` banner, node launched with `--no-seed
  --no-discoverable` + `GPN_STANDBY=1`, role Authority→Full → it **syncs but does not mine or
  announce**. Cached in `update-state.json` so the cutoff survives being offline. Escape hatch
  `GPN_MIN_VERSION_ENFORCE=0` (unsupported).
- **Peer layer (policy, staged):** bump the libp2p protocol id on consensus/wire-breaking releases
  so incompatible peers drop naturally. On-chain `MinNodeVersion` is **STAGED, NOT APPLIED**
  (`docs/design/design-min-node-version.md`).
- **Doc trap:** `docs/updates.md` still shows the `--setup` prompt defaulting to `off` and its intro
  table says "opt-in"; the **code** is opt-out. Trust the code and §9.1.

### 9.2 Node dashboard + Idle mode (`docs/node-gui-and-idle.md`) — LIVE

- `gpn_node --gui` / `GPN_GUI=1` serves **`http://127.0.0.1:8099`** from the supervisor (embedded
  `include_str!` page, no CDN/build). **Loopback only**: any non-127.0.0.1 bind is refused at startup
  (the panel can stop the miner); no auth because no exposure. `GPN_GUI_LISTEN` moves the port.
- **Idle** = stop **mining + AI job claims** for up to **6 h** (360 min), keep syncing/peering/serving,
  auto-resume at the deadline. State is one file `~/.gpn/idle.json` (0600, absolute `until`) →
  survives restart/crash/reboot; expired/corrupt/missing reads as **active**. CLI: `gpn_node idle 30`
  (exit 2 over-long), `gpn_node resume`. HTTP: `POST /api/idle`, `POST /api/resume`, `GET /api/status`.
- Miner **parks** (thread alive, 0% GPU, 500 ms check) so resume needs no restart/DAG rebuild;
  `park_reason()` = idle → ai-job → none; one log line per transition.
- A running job **always finishes**; the LLM gate sits after the reconcile pass so an
  `AwaitingVerification` job still reaches terminal — idle never wedges escrow.
- **Sole-authority valve:** peers = 0 (or RPC unreachable) → **409 / exit 3** refusal; `--force-idle` /
  `GPN_IDLE_FORCE=1` overrides for a single-box dev chain.
- 40 new tests (idle 14, gui 12, idle_mode 6, service 4, CLI 4). `earnings_today` is `null` on purpose
  (no off-chain reward indexer — the panel shows "—" rather than inventing a number).

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

### 11.1 Live governance parameters (spec 41 — DO NOT MIX WITH §11.2)

On chain **today**: **3-member** committee · **2-of-3** approvals (`ApprovalsRequired = 2`) ·
**3-day** timelock (`3 * DAYS` @ 15 s) · **no `cancel`** · **no proposal expiry** · **no preimage
proposals** (call data inline in the extrinsic) · **zero weights** (`WeightInfo = ()`). Proposals are
removed before the Root dispatch, so there is no replay. Mainnet sudo is all-zeros/inert.

### 11.2 Governance hardening — STAGED, NOT APPLIED (spec 42)

Artifact: `wasm/gpn_runtime_spec42_gov_hardening.compact.compressed.wasm`, **512,888 bytes**,
sha256 `7694f7f6c547df93b7c22d53863acae5b83532721a28c10375d2a233333d30f1`, `spec_version 42`.
Plan + checklist + exact enactment sequence: `staged/gov-hardening-v1.md`.

| Live (spec 41) | Staged (spec 42) |
|---|---|
| 3 members | **5 members**, `set_members` (Root-only); migration seeds 2 new |
| 2-of-3 | **3-of-5** |
| proposals never expire | **`ProposalExpiry = 14 days`** + `on_initialize` pruning, bounded 64 open |
| no withdrawal | **`cancel(hash)`** at 3-of-5 |
| inline call (500 KB WASM in the extrinsic) | **`propose_preimage`/`note_preimage`/`peek`** via a local `Preimages` map (deviation from the design's `pallet-preimage`; no deposits, member-gated, 4 MiB cap) |
| `WeightInfo = ()` (unmetered) | **`GpnWeight<Runtime>`** hand-set per-byte weights (benchmark TODO) |
| — | **v0→v1 storage migration** (`ProposalCall::{Inline,Preimage}`, `ProposalIds` index) |

Tests: 22 pallet + 10 runtime-rehearsal (incl. enacting the real 512 KB artifact) + 22 `gov` decoding.
**No `set_code` submitted, no key used, genesis WASM untouched (`76f37b23…16fa97`).** Known
limitations are documented (approvals survive a membership change; no preimage deposits; O(64) prune
scan). Rollback requires a *new* spec — v1 storage is not readable by spec-41 code.

---

## 12. Infrastructure & operations (current)

- **Host:** Jeremy's **static-IP machine** (self-hosted, $0; public IP `142.129.54.88`;
  duckdns `gpn-testnet.duckdns.org`, updated by cron). GPU: **RTX 4060**.
- **Services (systemd user units):** `gpn-fleet` (node via `run-fleet.sh`), `gpn-accesspoint`
  (`gpn_scheduler` :8092), `gpn-provider`/`gpn-diffusion`/`gpn-provider-heartbeat` (:8093),
  `gpn-explorer`, `gpn-faucet`, `gpn-ap-topup.timer`.
- **Ports:** p2p **30333**, RPC **9944**, AP **8092**, provider heartbeat **8093**, `/infer` **8094**,
  node dashboard **8099** (loopback only, `--gui`); the website Worker proxies
  `https://gpncoin.ai/api/*` → AP.
- **Bootnode:** peer id `12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo`;
  `/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooW…`.
- **Node #2 (Windows full node):** peer `12D3KooWRWv4yVdJGiVg7xRgdUJY9G57EeutB596gWVpAwGC9qQ2`;
  `/ip4/142.129.54.88/tcp/30334/p2p/12D3KooW…` (p2p forwarded 30334→30333). Runs the **`gpn_node`
  supervisor** (mine + serve). Both nodes registered in `network.json` + the website.
- **Chain:** on-chain **spec 41** (2026-10-14 — sudo-key-leak incident → rotated, spec-41 runtime).
  History: **spec 13** = channels +
  capacity challenge + per-requestor cap + expiry sweep + root-only distributions · **spec 14** =
  block target 4→15 s + reward restored to 50 GPN · **spec 15** = shielded **spend** path (VK + `spend`
  + root history) · **spec 16–38** = the red-team sweep (min-GPU floor + ~31 findings: escrow theft-of-service, unvalidated governance/threshold writes, `submit_solution` PoW bypass, shielded pool drain/fee stranding, channel refund window, fork-choice, …) · **spec 39–41** = red-team batch apply + the key-leak rotation.
  genesis `0xb8e4636a…` (canonical). **All applies authorized** (Jeremy green-lit the work; MAIN
  executed per REVIEW-PROTOCOL). Node rebuilt + restarted so its native runtime matches.
  **Ongoing:** the red-team keeps finding → latest staged artifact applied each time (`wasm/README-pending-upgrade.md` is the canonical pointer).
- **Repos:** `~/.openclaw/workspace/projectmanager07` (crates `gpn-node/`, `gpn-agent/`);
  remote `https://github.com/jeremyzguo/gpn_coin.git` (branch `main`). PAT retrievable at
  `~/.openclaw/credentials/github-gpn-pat.txt` (git credential store configured).
- **Keys:** `governance-keys.txt` (sudo, faucet, committee-1..3, reward-wallet);
  `~/.gpn/accesspoint.env` (`AP_SEED`, `GPN_AP_TOKEN`); `~/.gpn/node-key`.
- **Nodes/payouts:** Linux node #1 → **Alice** `5GrwvaE…` (attested). Windows box **#2** → its own account;
  as of **2026-10-13** it runs the **`gpn_node` supervisor** — a **mining full node** (peer
  `12D3KooWRWv4…`, p2p `30334`) **AND** an AI provider (on-chain-discoverable via `ProviderEndpoints`).
  *(Both nodes mine + serve.)* Beneficiary = reward wallet `5D1sq…`.
- **Windows run (current):** `\bin\gpn_node.exe` — **no flags** (always mine + serve; mining is the
  default, no `--force-authoring`; §13). Legacy single-node: `gpn-node.exe --chain
  C:\gpn\testnet-spec.json [--bob] --node-key 8f9e… --rpc-port 9945 --bootnodes $BN` (uses the
  **chain-spec file**, not `--chain testnet`).

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

## 14. Current state (2026-10-22)

- **Chain:** PoW at **on-chain spec 41**, **15 s cadence**, **50 GPN/block**; hardened by the **red-team sweep** (~31 findings fixed). Public full nodes on the canonical chain — the Linux bootnode (`…:30333`) and a Windows box — registered in `network.json` + the website.
- **Node model (locked):** a node is **always miner + provider**. The **`gpn_node` supervisor** (one command) runs node + heartbeat + provider; the node **mines by default** (role forced to authority); **capacity = 1**; an accepted job **fully stops mining** then resumes (the GPU-yield). AMD support in progress (#27).
- **Marketplace:** LLM **LIVE**; image **works via on-chain `POST /jobs`** (no public `/v1` route); video (LTX) **BLOCKED** (T5-XXL encoder > 8 GB VRAM); fine-tune + ML **shipped, not publicly routed**. Routing = capability → has-model → fastest-idle (busy excluded), transport **pull** (`GET /assignments` lease + `POST /result`); on-chain provider discovery; VRAM/RAM + min-GPU gates; payment channels; shielded spend; liveness layer; Bitcoin-style peer discovery.
- **Public surface:** `https://gpncoin.ai/api/v1/*` (HTTPS, Worker-proxied, rate-limited, free on testnet). Live e2e is tens of seconds — create-inclusion bound (#49/#52), not inference.
- **Node ops:** dashboard + Idle (§9.2); signed auto-update **on by default** with staggered fleet rollout (§9.1); AP token self-generated (§6.4).
- **Distribution:** every build publishes a rolling **GitHub Release** (`windows-latest` / `linux-latest` / `macos-latest`) on the public mirror `crewlyllc/gpn_coin`; the signed bundle manifest is served at `https://gpncoin.ai/releases.json`.
- **Ops:** `GPN-TASKS.md` queue + a 30-min **queue-driver cron**; monitors silent when healthy (§16).
- **Deployment lag, stated honestly:** the AP process running on this host is still the pre-2026-10-21 build (uncurated `/v1/models`, no VRAM gate at the endpoint). Code fixed and tested; restart/redeploy pending.
- **Open / next:** #27 AMD · #26 single-binary (back burner) · the **deferred profitability auto-switch** · #4/#5b Windows-provider + video verify · #12 mainnet · #13 bridge · #17 ceremony · #22 safety guardrails · **apply governance hardening (spec 42)** · **oracle price feed** · **input-token metering**.

- **Chain:** PoW at **on-chain spec 38**, **15 s cadence**, **50 GPN/block**; hardened by the **red-team sweep** (~31 findings fixed). **Two public full nodes** on the canonical chain — the Linux bootnode (`…:30333`) and **Windows box #2** (`142.129.54.88:30334`) — both at the tip, registered in `network.json` + the website.
- **Node model (locked):** a node is **always miner + provider**. The **`gpn_node` supervisor** (one command) runs node + heartbeat + provider; the node **mines by default** (role forced to authority — no `--force-authoring`); **capacity = 1**; an accepted job **fully stops mining** then resumes (the GPU-yield). AMD support in progress (#27).
- **Marketplace:** LLM + image + video + ML + fine-tune; **routing = capability → has-model → fastest-idle** (busy excluded); **on-chain provider discovery** + **HTTP assignment dispatch**; **VRAM/RAM** + **min-GPU** gates; **payment channels**; **shielded spend** (spec 15); liveness layer; **Bitcoin-style peer discovery**.
- **Distribution:** every build publishes a rolling **GitHub Release** (`windows-latest` / `linux-latest` / `macos-latest`); the Windows CI **deploys directly** to `C:\gpn\bin` on the runner box.
- **Ops:** `GPN-TASKS.md` queue + a 30-min **queue-driver cron**; monitors silent when healthy (§16).
- **Open / next:** #27 AMD · #26 single-binary (back burner) · the **deferred profitability auto-switch** · #4/#5b Windows-provider verify + video · #12 mainnet · #13 bridge · #17 ceremony · #22 safety guardrails.

---

## 15. Roadmap / open items (status 2026-10-22)

**✅ DONE (highlights):** #1 on-chain provider discovery · #3 client RTT · #5a/b/c AI methods (fine-tune,
video, ML) · #6 staking-weight + capacity challenge · #7 ms retarget · #8 clock warning · #9a–e
job-lifecycle hardening · #10 diffusion wedge watchdog · #11 root-only distributions · #14 VRAM/RAM gate ·
#15 shielded spend (spec 15) · #16 red-team (~31 findings fixed) · #18 Bitcoin-style peer discovery ·
#19 min-GPU gate · #20 snapshot + airdrop tooling · #21 node-free wallet · #23 fast-onboard snapshot ·
#24 mine→stop→resume + capacity 1 · #25 node supervisor · **signed auto-update (on by default)** ·
**AP token self-generation** · **public HTTPS API** · **curated/VRAM-gated catalogue** ·
**provider address publication** · **node GUI + Idle** · **derived pricing + economics page**.

**⏳ IN PROGRESS / OPEN:** #27 AMD support (building) · #4 Windows-provider end-to-end verify · #5b video
on the Windows box · #2 channels hub routing · #22 abuse/safety guardrails (design done) · the
**profitability auto-switch** (deferred) · **on-chain oracle feed** · **input-token metering**.
**🟡 STAGED (built + tested, not applied):** governance hardening spec 42 (§11.2) · on-chain
`MinNodeVersion` cutoff.
**🔒 BLOCKED (need Jeremy/external):** #16/#17 the **CRS ceremony** (public multi-party) · #12 mainnet
(genesis/launch) · #13 bridge (post-mainnet) · video until a >8 GB VRAM node is in the fleet.
**🧊 BACK BURNER:** #26 single-binary node.

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
`docs/updates.md` (auto-update, manifest, cutoff, rollback) · `docs/node-gui-and-idle.md` ·
`docs/provider-address-and-models.md` · `docs/upgrades-governance.md` ·
`staged/gov-hardening-v1.md` (spec-42 plan) ·
`competitive-landscape-review-2026-10-03.md` · `gpn-differentiation-and-advantage-2026-10-03.md`.
**Logs:** `change-log.md` (running ops changelog) · `cycle*-*.md` (historical cycle logs).
**Live ops/tracking:** `GPN-MASTER-REFERENCE.md` (this doc) · `GPN-TASKS.md` (the cron-driven task
queue) · `mainnet-open-decisions-memo.md` · `design-offchain-escrow-channels.md` ·
`access-point-install-guide.md` · `wasm/` (staged spec-N upgrade artifacts).
**Marketing:** `marketing/` (brand, content, GTM, listings) + `marketing-plan.md`.
**Publications (2026-10):** `GPN-WHITEPAPER.md` (+ `.html`), `GPN-ONE-PAGER.md`,
`GPN-TECHNICAL-OVERVIEW.md`; repo `docs/` (`downloads.md`, `run-a-node.md`, `fast-sync.md`,
`peer-discovery.md`) + `website/` (6 pages) + `kickstarter/` + `fundraising/` (juicebox, gitcoin).
**Node/run tooling:** `docs/run-a-node.md` + `gpn-node.example.toml`; scripts `install.sh`,
`install-accesspoint.sh`, `scripts/gpn-snapshot.{sh,ps1}`, `scripts/install-ollama.{sh,ps1}`.
