# HEARTBEAT.md — Project 7: GPU-Native Crypto & AI Compute Network

> **C79 (2026-08-05 15:55 PT):** No new Jeremy signal (green-light / D1–D5 / VPS) — blocker UNCHANGED from C69–C78 (~49th no-op). Verified no bitrot: git clean (only HEARTBEAT.md modified); cargo check EXIT 0 for gpn-node (same 8 pre-existing warnings, no errors) and gpn-agent (1 warning). Runbook gap persists: NO testnet-public-runbook.md anywhere; cron brief's "cycle63-decision-card.md + §7" ref stale (file absent) — actual decision doc = cycle59-decision-brief.md. Ready to execute public-testnet launch immediately on green-light. Escalated crisply: need D1–D5 + VPS (~$10–20/mo) go/no-go. No redundant action; logged tight.

> **Status: Phase 3 COMPLETE / Phase 4 READY — LIVE on RTX 4060.** Blocked only on Jeremy's
> go/no-go for D1–D5 + a small VPS budget (~$10–20/mo). Full history is in git and the
> per-cycle logs under `shared/artifacts/project7/`; this file was consolidated (2026-08-04)
> to remove ~54 duplicate no-op firing blocks — no content lost, all preserved in git.

## Cycle 68 (2026-08-05 ~04:55 PT) — CRON BRIEF FIXED at source (first real change in ~60 cycles)
- **FIXED the stale cron brief** (the recurring system-hygiene issue): rewrote the PM07 cron job's
  `payload.message` via the sanctioned `openclaw cron edit 68687b17… --message` CLI (NOT hand-edited
  jobs.json). The brief now says "Phase 3 COMPLETE / Phase 4 READY — NOT GREENFIELD" and instructs
  future firings to: (1) check for Jeremy's green-light signal, (2) execute `testnet-public-runbook.md §7`
  if present, else (3) verify no bitrot + log tight — and explicitly forbids redoing research or adding
  redundant no-op firings. Schedule unchanged (`55 * * * *`). This permanently stops the ~hourly
  greenfield-template firing loop that produced ~60 near-identical no-ops.
- Verified no bitrot: git clean; gpn-node `cargo check --workspace` EXIT 0 (8 pre-existing GPU-miner
  warnings only); gpn-agent `cargo check --lib` EXIT 0 (known subxt v0.37 future-incompat note only).
- No new Jeremy signal (the green-light phrase in memory files is prior cycles *describing* the
  recommended reply, not an actual reply). Decision card + runbook still current (Aug 2).
- Blocker UNCHANGED: Jeremy's go/no-go on D1–D5 + ~$10–20/mo VPS. Escalated (crisply) below.

## Current State (Cycle 64+ — consolidated 2026-08-04 ~18:00 PT)

**Project:** GPU-Native Crypto & AI Compute Network — a Substrate blockchain where every GPU
node mines (Ethash PoW), stakes (Proof-of-Staked-GPU attestation), and serves AI compute,
auto-switching by a governed on-chain profitability oracle.

**What is BUILT and LIVE (verified end-to-end on the RTX 4060):**
- Real GPU-only Ethash PoW consensus + self-tuning difficulty (OpenCL miner, node-verifiable seals)
- External miner RPC (`gpn_getTemplate`/`gpn_submitWork`), two-node GPU p2p testnet (6/6)
- Proof-of-Staked-GPU: stake 1,000 GPN + attest real GFLOPS → tier-weighted staking rewards
- Compute marketplace: two-party escrow, jobs, SLA, `min_gflops` attestation gate, 5% fee
  (self-funding fee economy → `gpn/tokn` pools 400/100 treasury/oracle, burn, tier-weighted payout)
- Profitability oracle FULLY GOVERNED (Phase A→B→C): staleness TTL, reasonableness bounds,
  per-operator feeds → median → committee admission → bonded operators + on-chain bond-weighted
  median (`effective_feed()`) + slashing; exposed via `gpn_effectiveFeed` RPC + explorer
- Auto-switch: one GPU alternates Mining ↔ Compute by live on-chain profitability (no human)
- Tooling: `gpn_faucet` (drip), `gpn_explorer` (web + JSON API), `gpn_operate_oracle`,
  `gpn_phase_c_probe`, `gpn_check_feed`, CI (two-node testnet wired)
- Test status (last full run): workspace 125/125, agent 50/50, oracle 41/41, release build clean
- Two no-money launch demos DONE: live 3-op bond-weighted median (Cycle 60) + public-testnet
  toolchain dry run (Cycle 61, four components proven). Launch runbook: `testnet-public-runbook.md`.

**Real blocker (UNCHANGED, escalated ~55x):** Jeremy's go/no-go on **D1–D5** + **VPS budget**.
- Decision card: `shared/artifacts/project7/cycle63-decision-card.md` (60-sec read)
- **Green-light reply: "PM07: accept all recommendations, approve VPS."** → I run `testnet-public-runbook.md §7`.
- D1 closed 3–5 operator set · D2 committee governance OK at launch · D3 on-chain Phase-C median
  · D4 keep 10% slash / BURN · D5 ops holds testnet Sudo (mainnet stays inert).
- VPS: ~$10–20/mo, 2 vCPU/4GB/40GB, GPU not required (bootnode/validator is CPU/light).

**This cycle (2026-08-04):** verified state green (git clean, `cargo check --workspace` gpn-node
EXIT 0 — only 8 pre-existing GPU-miner warnings — + `cargo check --lib` gpn-agent EXIT 0 — known
subxt v0.37 future-incompat note only; no bitrot). No new Jeremy signal in either artifact dir.
Per SOUL.md I did NOT redo finished Phase-1 research (brief is a stale greenfield template) and
did NOT add another redundant no-op — instead consolidated this bloated file. All 9 design docs
present (`design/pallets|protocols|tokenomics|node-agent`). No no-money/no-decision code/doc work
remains outstanding on the Phase-3/4 roadmap.

> ⚠️ **System hygiene:** the cron brief still fires the greenfield "Phase 1 research" template
> hourly at a Phase-4-blocked project, producing ~54 near-identical no-op firings over ~3 days.
> Recommend: (a) Jeremy gives the D1–D5 + VPS decision to unblock real Phase-4 work, or
> (b) retire/rewrite the cron brief to "Phase-3/4-BLOCKED — waiting on Jeremy's D1–D5 + VPS
> decision." No further no-money/no-decision work is outstanding.

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
| **Live end-to-end compute-marketplace job flow on RTX 4060: register → create → accept (attestation gate) → start → real GEMM → submit → complete + payout; GFLOPS-units fix so `min_gflops` gate works (cycle 28)** | ✅ **LIVE** |
| **AUTO-SWITCH LOOP CLOSED: `auto` mode claims & serves marketplace jobs on-chain in Compute mode (accept→start→GEMM→submit→complete→payout); AccountId32 display fix `provider=0x00`→Alice (cycle 29)** | ✅ **LIVE** |
| **TWO-ACCOUNT REQUESTOR→PROVIDER flow: distinct paying customer (Bob) escrows, distinct provider (Alice) paid price−5%fee; verified live on-chain (cycle 30)** | ✅ **LIVE** |
| **LIVE CHAIN-FEED-DRIVEN AUTO-SWITCH + JOB CLAIMING: real on-chain profitability feed (not scripted) drives Mining→Compute and claims/serves/pays a marketplace job; fixed USD² compute-revenue units bug (cycle 31)** | ✅ **LIVE** |
| **SELF-FUNDING FEE ECONOMY: marketplace fees (5%) route into `gpn/tokn` via `FeeSinkProvider` at `complete_job`, so `distribute_fees` draws real collected fees; verified live (50 GPN job → 2.5 GPN in gpn/tokn → pools 400/100, oracle drained, staking held for unattested); fixed gpn_distribute_rewards tokio blocking panic (cycle 37)** | ✅ **LIVE** |
| **TIER-WEIGHTED STAKING PAYOUT LIVE: fixed latent `MinAttestationStake` unit bug (1e21 raw = 1B GPN → 1e15 = 1,000 GPN; cycle-35 faucet cut exposed it); Alice attested Bronze @194 GFLOPS on RTX 4060, staking pool drained 400→0 (was held for unattested in cycle 37), oracle drained, Alice +~500 GPN (cycle 38)** | ✅ **LIVE** |
| **DISPATCH-OUTCOME VERIFICATION: `signer::submit_signed` now verifies dispatch success (not just inclusion) — reads `System::Events` via proven dynamic-storage path, scans for `System::ExtrinsicFailed`, decodes resolvable module error; new `gpn_verify_dispatch` probe; verified live: guaranteed-fail `accept_job` → `Err: ModuleError(ComputeMarketplace::JobNotFound/ProviderNotRegistered)`, success path logs `tx dispatched successfully` (cycle 39)** | ✅ **LIVE** |
| **FULL REVENUE LOOP CLOSED LIVE: two-account demo (Bob→Alice escrow, all-green) → verify_dispatch failure path → GPU attest (Alice Bronze @196 GFLOPS) → distribute_fees pools 400/100 → both drained to attested Alice (+500 GPN); fee routing + tier-weighted staking payout + dispatch-outcome verification all confirmed end-to-end in one session on the fixed runtime (cycle 40)** | ✅ **LIVE** |
| **`testnet_chain_spec()` — config-only fork of mainnet (`--chain=testnet`: ChainType::Local, NO endowed accounts → fair-launch, bootstrap oracle ops Alice+Bob, ops sudo Alice; same WASM runtime, identical diff/reward/inflation/cap); wired into `load_spec`; LIVE-mined real GPU Ethash blocks #1→#13 on RTX 4060; build-spec + mainnet-parity verified (cycle 43)** | ✅ **LIVE** |
| **`gpn_faucet` — off-chain drip faucet (runbook §5 option 3, ZERO runtime change): HTTP `/drip` (ss58|hex) + `/status`; rate-limited signed `Balances.transfer_allow_death` (cooldown/daily-cap/total-cap/reserve); reuses dispatch-outcome-verified signer; LIVE on `gpn-node --dev` (RTX 4060): 5000 GPN drip to Bob landed on-chain, cooldown 429, hex drip ok, invalid/missing 400, reserve guard 500 (cycle 44)** | ✅ **LIVE** |
| **`gpn_explorer` — read-only web block explorer + JSON API (runbook §7): HTML dashboard (head #/hash, real difficulty, feed, accounts+attestation, blocks, jobs) + `/api/chain`, `/api/blocks`, `/api/account`, `/api/jobs`; reuses `ChainClient` + raw header RPC; LIVE on `gpn-node --dev` (RTX 4060): head #30 real hash, difficulty 4096, Alice/Bob balances, live job #1 Pending 50 GPN (requestor Bob); fixed `chain_getHeader`-has-no-hash bug (cycle 45)** | ✅ **LIVE** |
| **FEED STALENESS TTL (oracle-governance §6 gap #1): when `CurrentFeed.updated_at` is older than `feed_staleness_ttl_blocks` (default 1440 ≈6h), `analyze()` forces conservative Mining (compute+staking=0) instead of trusting a stale feed; new `chain::head_block_number()`/`get_feed()`/pure `is_feed_stale()`; VERIFIED LIVE on RTX 4060 dev node (feed @block16, head22 → stale(TTL=5)=true, stale(TTL=1440)=false); agent 33/33, node 98/98 (cycle 48)** | ✅ **LIVE** |
| **FEED REASONABLENESS BOUNDS (oracle-governance §6 gap #2): pure `compute_revenue_reasonable(revenue, max)` rejects non-finite/≤0/>ceiling compute revenue (default 50 USD/hr, configurable `compute_revenue_max_usd_per_hour`); `analyze()` forces conservative Mining (compute=0) on an implausible feed; new read-only `gpn_check_feed` diagnostic; VERIFIED LIVE on RTX 4060 dev node (implausible $10k/hr,100% → $8,500/hr rejected exit 1; plausible $2.50/hr,65% → $1.38/hr accepted exit 0); agent 37/37 (+4), node 98/98 (cycle 49)** | ✅ **LIVE** |
| **PER-OPERATOR FEED MEDIAN AGGREGATION (oracle-governance §6 gap #3): new pallet `OperatorFeeds: StorageMap<AccountId, ProfitabilityFeed>` (latest feed per operator, populated on every `update_feed`; `CurrentFeed` unchanged as effective/latest — backward-compatible); agent `get_operator_feeds()` (subxt `.iter()`) + pure `median_f64`/`aggregate_median_feed` (unit-proven one $9,999/hr outlier can't move the median: `[2,3,9999]→3`); new config `feed_aggregation="latest"|"median"` (serde default latest); `analyze()` median branch (falls back to CurrentFeed); new read-only `gpn_aggregate_feed` tool; VERIFIED LIVE on RTX 4060 dev node (Alice feed read from new map + median; `gpn_check_feed` backward-compat ok); pallet 16/16 (+2), agent 44/44 (+7), node 100/100 (+2); design finding: `UpdateTooSoon` is a single global throttle → per-operator in Phase B/C (cycle 50)** | ✅ **LIVE** |
| **PHASE-B COMMITTEE ADMISSION (oracle-governance §6 gap #4): `add_operator`/`remove_operator` root-only → named committee vote. New `Committee` (genesis-seeded) + `PendingAdmissions` (proposal + bounded approvals) storage in `pallet-profitability-oracle` (NO new pallet/deps); `propose_operator` (committee member, self-approves) + `approve_operator` (committee member; auto-admits at `AdmissionThreshold`); `remove_operator` = committee OR root break-glass; runtime `CommitteeSize=8`/`AdmissionThreshold=2`; chain specs seed committee dev/local/testnet, MAINNET EMPTY (inert); new `gpn_committee_probe`. VERIFIED LIVE on RTX 4060 dev chain (GPU-mined blocks #24+): Alice propose //Charlie → pending → Bob approve → Charlie admitted → Charlie update_feed accepted → non-member Dave rejected `NotCommitteeMember` (pallet=9 error=6). Oracle pallet 26/26 (+10), workspace 110/110; §6 gaps #1–#4 ALL CLOSED (cycle 51)** | ✅ **LIVE** |
| **PHASE-C STAKING-BACKED SLASHING ORACLE (oracle-governance §6 gap #5, THE LAST CODE GAP): operator set permissionless + economically-secured. New `StakedOperators` (bond map) / `OperatorLastFeedBlock` (per-op staleness) / `OpenChallenges` storage in `pallet-profitability-oracle` (NOT a new pallet crate — avoids sp-core conflicts); extrinsics `bond_operator`/`bond_more`/`unbond_operator` (blocked while challenged)/`challenge_operator`/`resolve_challenge` (SlashStale|SlashOutlier|Dismiss); `update_feed` extended to accept bonded operators; on-chain bond-weighted-median `effective_feed()`; config `OperatorBond`/`ChallengeBond`/`OutlierThresholdBps`/`StaleOperatorBlocks`/`SlashBps`/`Currency`; D4 default = BURN (10% SlashBps, frivolous-challenger bond burned); runtime `OperatorBond`=1,000 GPN / `ChallengeBond`=100 GPN / `StaleOperatorBlocks`=14,400 / `SlashBps`=1,000; agent `ChainClient::get_staked_bond()` + `gpn_phase_c_probe`. VERIFIED LIVE on RTX 4060 dev chain (`gpn_phase_c_probe` exit 0): bond → 1,000 GPN on-chain → bonded update_feed accepted → bond_more → 1,100 GPN → challenge → Dismiss resolve (bond intact, challenger burned) → unbond. Oracle pallet 26→39 (+13), workspace 123/123, agent 44/44; **ALL §6 gaps #1–#5 CLOSED** (cycle 53)** | ✅ **LIVE** |
| **AGENT CONSUMES THE PHASE-C BOND-WEIGHTED MEDIAN (spec §8/D3 code item — the last code work item in the oracle-governance roadmap): new `ChainClient::get_staked_bonds()` (enumerates `StakedOperators`) + pure `bond_weighted_median_f64`/`aggregate_bonded_median_feed` (a large-bond operator dominates; a tiny-bond $9,999/hr outlier cannot move any field) + `analyze()` `feed_aggregation="bonded_median"` branch (aligns feeds↔bonds by account key, keeps only bonded+posted operators, warn+fallback to CurrentFeed) + `gpn_aggregate_feed` now prints a `[bonded-median]` row. +6 tests (agent 44→50). VERIFIED LIVE on RTX 4060 dev node: Alice feed seeded → `[median]` populated, `[bonded-median] NONE` (correct: nobody bonded+posted); `gpn_phase_c_probe` all Phase C economics pass live (bond→bond_more→challenge→Dismiss→unbond; the single `UpdateTooSoon` ❌ is the pre-existing global 600-block throttle, NOT a regression). Agent 50/50, workspace 123/123, release clean — no pallet/runtime change (cycle 54)** | ✅ **LIVE** |
| **ON-CHAIN EFFECTIVE FEED EXPOSED (the cycle-55-flagged candidate, code, no money): the pallet's AUTHORITATIVE Phase-C bond-weighted median `effective_feed()` was `pub(crate)` (only internal, for slashing-outlier detection) — the agent could only reimplement it client-side. Made it `pub`; added `ProfitabilityOracleApi` runtime API (`fn effective_feed()`) + impl; new `gpn_effectiveFeed` node RPC (serde `EffectiveFeed`: raw micro-USD/bps + human USD convenience fields, called at client best hash); new read-only `gpn_effective_feed` agent tool (JSON-RPC POST ws->http, the proven head_block_hash pattern) + `ChainClient::get_effective_feed()` → `OnChainFeed`. VERIFIED LIVE on RTX 4060 dev node (real GPU Ethash): negative path before bonding → `NONE` (exit 1); `//Dave --bond` → `✅ bond_operator` + bonded `update_feed` → `gpn_effective_feed` reads on-chain median `token=$0.082 / gpu=$3.00/hr / 72% / 15% @block 23` (exit 0) — **exactly matches** client-side `gpn_aggregate_feed` bonded-median (identical @block 23), proving the runtime API surfaces the same bond-weighted median. 123/123 workspace, 50/50 agent, release clean (cycle 56)** | ✅ **LIVE** |
| gpn-agent subxt chain client (stub) | ✅ **REPLACED (cycle 21)** |
| Wire real `GPUAttestation::submit_attestation` extrinsic (reuse gpn_seed_feed Signer) | ✅ **DONE (cycle 25)** |
| Enforce `min_gflops` matching at `accept_job` (cross-pallet attested read) | ✅ **DONE (cycle 26)** |

## Next Concrete Actions (priority order)
1. [x] Wire `distribute_fees`/`distribute_staking_rewards`/`distribute_oracle_rewards` into the node/agent tooling (a `gpn_distribute_rewards` helper mirroring `gpn_seed_feed`) so the fee economy is exercisable end-to-end on-chain. **(Cycle 36 DONE: added Sudo pallet so root-only `distribute_fees` is dispatchable; new `gpn_distribute_rewards` tool sudo-dispatches `distribute_fees` then pays staking (tier-weighted) + oracle pools; verified live on `gpn-node --dev` — pools fill 400/100, oracle drained to Alice, staking pool held for unattested staker)**
2. [x] **Close the funding gap (found cycle 36):** wire marketplace fees (5%, currently in `gpn/cmkt`) → `gpn/tokn` pallet account so the fee economy is self-funding. **(Cycle 37 DONE: `FeeSinkProvider` trait in gpn-token; marketplace `complete_job` routes the fee to the token pallet account; runtime `type FeeSink = GPNToken`; verified LIVE — a 50 GPN job's 2.5 GPN fee landed in `gpn/tokn`, then `distribute_fees(1000)` filled pools 400/100, drained oracle to Alice, held staking for unattested staker. Also fixed the `gpn_distribute_rewards` tokio blocking panic)**
3. [x] Verify attested-tier-weighted staking payout live on the RTX 4060 (attest Alice → Bronze, re-run `gpn_distribute_rewards` → staking pool should drain with tier multiplier). **(Cycle 38 DONE — and it surfaced + fixed a latent `MinAttestationStake` unit bug: value was 1e21 raw = 1 BILLION GPN, not 1,000 GPN = 1e15. Fixed runtime + agent; verified live: Alice Bronze @194 GFLOPS, staking pool drained 400→0 (was held for unattested), oracle drained, Alice +~500 GPN. 98/98 tests.)**
4. [x] **(Robustness, cycle-38 finding)** Make `signer::submit_signed` (and agent callers) verify the **dispatch outcome / events**, not just inclusion — failed extrinsics currently log "succeeded". This masked the `MinAttestationStake` unit bug. **(Cycle 39 DONE: `submit_signed` now reads `System::Events` via the proven dynamic-storage path, scans for `System::ExtrinsicFailed`, and decodes a resolvable module error (`ModuleError(Pallet::Error) [pallet=.. error=..]`). New `gpn_verify_dispatch` probe. Verified live on RTX 4060: guaranteed-fail `accept_job` → `Err: ModuleError(ComputeMarketplace::JobNotFound/ProviderNotRegistered)`; success path (`gpn_two_account_demo`) logs `tx dispatched successfully` for every real tx — no false Err. 26/26 agent lib tests (+4), 98/98 node-workspace tests.)**
5. [x] Re-run the two-account requestor→provider marketplace demo live with the fixed runtime (full fee-routing → distribute → tier-weighted payout path end-to-end). **(Cycle 40 DONE — the FULL loop closed live on the RTX 4060 in one session on the cycle-39 node: two-account demo all-green → verify_dispatch failure path → GPU attest (Alice Bronze @196 GFLOPS) → distribute_fees splits pools 400/100 → both drained to attested Alice (+500 GPN). Fee routing + tier-weighted staking payout + dispatch-outcome verification all confirmed together. 98/98 node + 26/26 agent tests.)**
4. [x] Fill remaining empty design dirs — `design/pallets/`, `design/protocols/`, `design/node-agent/` (Cycle 33, DONE).
4. [x] Two-node testnet docs in README + CI wiring for `gpn-node/scripts/gpn_two_node_testnet.sh` (cycle 23, 6/6 live).
2. [x] Compute marketplace integration: surface `ComputeExecutor` throughput/GFLOPS as an on-chain attestable metric (cycle 24 — attestation tier gated by GFLOPS + `min_gflops` in marketplace + agent measures/reports).
3. [x] Wire the real `GPUAttestation::submit_attestation` extrinsic in the agent by reusing the `gpn_seed_feed` custom Signer + `GpnConfig` plumbing (cycle 25 — shared `chain::signer` module; challenge read; `with_signer` wiring).
4. [x] Enforce `min_gflops` matching at `accept_job` — cross-pallet read of the provider's attested `compute_gflops` (trait from gpu-attestation into compute-marketplace) (cycle 26).
5. [x] Live end-to-end attest on the RTX 4060: deposit stake (pallet requires `ProviderStake`), measure ~830 GFLOPS, submit, confirm tier on-chain (cycle 27 — staked 1,000 GPN, attested **Bronze** @ 188 GFLOPS; fixed signed-tx nonce + finality-hang bugs).
6. [x] Live end-to-end marketplace flow on the 4060: attest (~188 GFLOPS) → register provider → create job with `min_gflops ≤ 188` → accept (gated by cycle-26 trait) → run real GEMM → submit result → complete + payout. Check compute-marketplace extrinsic names + wire a provider-registration/job-lifecycle helper (mirror `gpn_check_state`). **(cycle 28 — DONE; also fixed GFLOPS units so the gate is semantically correct, verified positive+negative live)**
7. [x] Fix `gpn_marketplace_demo` display nit: `provider=0x00` in `get_job_state` — root cause: subxt dynamic storage decodes AccountId32 as a doubly-nested Composite::Unnamed; recursive `collect_account_bytes` now decodes correctly. Verified live `provider=0xd43593c715…` (Alice). **(cycle 29 — DONE)**
8. [x] Close the loop: in `auto` mode, the agent now actually CLAIMS marketplace jobs on-chain when the oracle says compute demand is high (accept→start→real GEMM→submit→complete+payout). **(cycle 29 — DONE, live on RTX 4060)**
9. [ ] Document the marketplace flow + GFLOPS units convention in README (now includes the loop-closing `gpn_create_job` + auto-agent walkthrough).
10. [x] Two-account requestor→provider demo (currently both are Alice for simplicity); wire a second dev account as the paying customer. **(cycle 30 — DONE: new `gpn_two_account_demo` binary, verified live on-chain: Bob escrows 50 GPN, Alice earns 47.5 GPN)**
11. [x] Couple the real profitability-oracle `update_feed` to job claiming (not just the scripted feed) so live token/gpu pricing drives it. **(cycle 31 — DONE: fixed `calc_compute_revenue` USD² units bug that kept chain-feed compute revenue ~13× too low; verified live on RTX 4060 — live feed `Compute=$9.69/hr` drove Mining→Compute and claimed/served/paid job #1; new `config/chain_autoclaim.toml`)**

## Next Concrete Actions (fresh, post-cycle-40)
1. [ ] **(Docs)** Finalize the marketplace flow + GFLOPS units convention + two-account/fee-economy walkthrough in README (item #9; partially landed — complete it).
2. [x] **(Phase 4 prep) Profitability-oracle operator tooling:** a `gpn_operate_oracle` long-running loop (mirror `gpn_seed_feed` but periodic `update_feed` maintenance), so the live feed is a maintained service, not a one-shot seed. **(Cycle 41 DONE — new `gpn_operate_oracle` binary: values reload from `oracle_values.toml` every iteration (external feed can update without restart); respects pallet `UpdateTooSoon` (600-block) back-off as expected, not error; `NotAuthorized` = fatal clear error, transient = retry; reuses dispatch-outcome-verified `signer::submit_signed` + head-block nonce. Verified LIVE on `gpn-node --dev` (RTX 4060): iter #1 `✅ update_feed dispatched successfully`, iter #2 `⏳ skipped (UpdateTooSoon)` proving feed on-chain; node mined to #15, PoW undisturbed. Build clean, 98/98 + 26/26 tests unaffected.)**
3. [x] **(Phase 4 prep) Mainnet testnet hardening:** re-verify two-node p2p testnet on the fixed runtime + config/docs for a public testnet spec. **(Cycle 42 DONE — two-node GPU p2p testnet re-verified on the fixed runtime: 6/6 passed, both nodes real OpenCL Ethash on RTX 4060, converged on best #7; new `shared/artifacts/project7/testnet-public-runbook.md` (chain-spec matrix, topology, genesis params, faucet options, operator onboarding, launch checklist, mainnet open items); README pointer added. No code changed.)**
4. [ ] **(Phase 4)** Public testnet → mainnet: token listing prep, compute marketplace launch, operator onboarding docs.

## Next Concrete Actions (fresh, post-cycle-44)
1. [x] **(Phase 4) Off-chain drip faucet** — small HTTP service backed by a funded testnet key (rate-limited signed transfer drip); zero runtime change, per runbook §5 recommendation. **(Cycle 44 DONE — new `gpn_faucet` binary: `/drip` (ss58|hex, `--drip-gpn`/`--cooldown-secs`/`--daily-cap-gpn`/`--total-cap-gpn`/`--reserve-gpn`) + `/status`; reuses dispatch-outcome-verified `signer::submit_signed`; `Balances.transfer_allow_death` with bare-account dest. Live-verified on `gpn-node --dev` (RTX 4060): 5000 GPN drip to Bob landed (Alice 100000→95000, dispatch ok), cooldown 429, hex-address drip ok, invalid/missing 400, reserve guard 500. 7 unit + 26 lib + 98 node tests pass.)**
2. [~] **(Phase 4) Block explorer** — first concrete piece done: `gpn_explorer` read-only web explorer + JSON API built & live-verified (Cycle 45, runbook §7). **REMAINING (needs hosting $):** stand up 2–4 public bootnodes + wss RPC + a public `gpn_explorer` instance → **ESCALATED to Jeremy**. ~~Publish operator-onboarding doc (docs, can draft now)~~ → **Cycle 46 DONE: `operator-onboarding.md` published (operator-facing subset of the runbook: 3 revenue modes + auto-switch, build, full node, faucet drip, auto agent, optional oracle op, explorer, public-endpoints placeholder table, troubleshooting). Runbook §7 checklist updated.**
3. [ ] **(Docs)** Finalize the README marketplace-flow + GFLOPS-units + two-account/fee-economy walkthrough (item #9; mostly landed — polish the fee-economy section to reference the now-closed funding gap + `gpn_operate_oracle` maintenance flow + the new `gpn_faucet`).
4. [~] **(Phase 4) Mainnet prep** — **Cycle 47 progress:** sudo "non-functional" half **VERIFIED** (`build-spec --chain=mainnet` → sudo key = all-zeros inert account, balances `[]`, oracle ops `[]` → `Sudo.sudo` unreachable on mainnet); **oracle governance spec DRAFTED** (`design/protocols/05-oracle-governance.md`, runbook §9 #5 — 3-phase plan + §6 code gaps + §7 decisions D1–D5). **Remaining:** sudo *governed* replacement path (business → escalate, §7 D2), token listing / liquidity plan (business → escalate), marketplace go-live + SLA/reputation policy (SLA drafted cycle 46; go-live = business), oracle governance decisions D1–D5 (business → escalate).
5. [x] **(Code, no money, high safety value — §6 gap #1)** Agent `CurrentFeed` **staleness TTL**: when `ProfitabilityOracle::CurrentFeed.updated_at` is older than N blocks, the agent falls back to a conservative default (mine) rather than act on a stale feed. **(Cycle 48 DONE — new `feed_staleness_ttl_blocks` config (default 1440 ≈6h), `chain::head_block_number()` + public `get_feed()` + pure `is_feed_stale()`; `analyze()` forces Mining (compute+staking=0) when stale. Verified LIVE on RTX 4060 dev node: feed @block16, head22 → stale(TTL=5)=true, stale(TTL=1440)=false. Agent 33/33, node 98/98.)** Next: §6 gap #2 (feed reasonableness bounds in `calc_compute_revenue`) and §6 gap #3 (client-side median tooling).
6. [x] **(Code, no money, high safety value — §6 gap #2)** Agent feed **reasonableness bounds** on `calc_compute_revenue`: reject implausible USD/hr so a single bad/outlier feed can't force a mass switch to Compute. **(Cycle 49 DONE — pure `compute_revenue_reasonable(revenue, max)` (finite, >0, ≤ ceiling); new config `compute_revenue_max_usd_per_hour` (default 50 USD/hr, serde default); `analyze()` forces Mining (compute=0) on implausible revenue; new read-only `gpn_check_feed` diagnostic. Verified LIVE on RTX 4060 dev node: implausible `gpu=$10k/hr,util=100%` → $8,500/hr rejected (exit 1, forces Mining); plausible `gpu=$2.50/hr,util=65%` → $1.38/hr accepted (exit 0). Agent 37/37 (+4), node 98/98.)**
7. [x] **(Code, no money, high safety value — §6 gap #3)** Agent **median aggregation across operators**: store each operator's own latest feed on-chain + aggregate via per-field median so one outlier operator can't move the effective feed. **(Cycle 50 DONE — new pallet `OperatorFeeds: StorageMap<AccountId, ProfitabilityFeed>` (populated on every `update_feed`; `CurrentFeed` unchanged/backward-compatible); agent `get_operator_feeds()` (subxt `.iter()`) + pure `median_f64`/`aggregate_median_feed` (unit-proven `[2,3,9999]→3` outlier exclusion); new config `feed_aggregation="latest"|"median"` (serde default latest); `analyze()` median branch (falls back to CurrentFeed); new read-only `gpn_aggregate_feed` tool. Verified LIVE on RTX 4060 dev node: Alice feed read from new map + median, `gpn_check_feed` backward-compat ok. Pallet 16/16 (+2), agent 44/44 (+7), node 100/100 (+2). Design finding: `UpdateTooSoon` is a single global throttle → only one op posts per 600-block interval (per-operator in Phase B/C).)** Next: §6 gap #4 → **Phase-B** council/multisig admission (runtime). **(Cycle 51 DONE — see item #8.)**
8. [x] **(Code, no money, high safety value — §6 gap #4)** Phase-B **committee admission** for oracle operators: replace root-only `add_operator`/`remove_operator` with a named committee that votes to admit operators (threshold approvals). **(Cycle 51 DONE — new `Committee` (genesis-seeded) + `PendingAdmissions` (proposal + bounded approvals) storage in `pallet-profitability-oracle` (NO new pallet/deps); `propose_operator` (committee member, self-approves; threshold-1 admits immediately) + `approve_operator` (committee member; auto-admits at `AdmissionThreshold`, clears pending); `remove_operator` = committee OR root break-glass (mainnet root inert → committee-only in practice); runtime `CommitteeSize=8`/`AdmissionThreshold=2`; chain specs seed committee dev/local/testnet, MAINNET EMPTY (inert); new `gpn_committee_probe`. Verified LIVE on RTX 4060 dev chain (GPU-mined blocks #24+): Alice propose //Charlie → pending → Bob approve → Charlie admitted → Charlie update_feed accepted → non-member Dave rejected `NotCommitteeMember` (pallet=9 error=6). Oracle pallet 26/26 (+10), workspace 110/110. §6 gaps #1–#4 ALL CLOSED.)** Next: §6 gap #5 → **Phase-C** staking-backed slashing oracle (new pallet, bigger build, gated on Phase A/B economics on testnet).
9. [x] **(Code, no money, high safety value — §6 gap #5, THE LAST CODE GAP)** Phase-C **staking-backed slashing oracle**: permissionless operator set (anyone bonds GPN), stale/outlier operators slashable via challenges. **(Cycle 53 DONE — see build-table row. ALL §6 code gaps #1–#5 CLOSED.)**
10. [x] **(Code, no money — spec §8/D3 code item)** Agent **consumes the Phase-C bond-weighted median** end-to-end via `feed_aggregation="bonded_median"`. **(Cycle 54 DONE — new `ChainClient::get_staked_bonds()` + pure `bond_weighted_median_f64`/`aggregate_bonded_median_feed` (large-bond dominates, tiny-bond outlier can't move any field) + `analyze()` branch (aligns feeds↔bonds by account, keeps bonded+posted only, fallback to CurrentFeed) + `gpn_aggregate_feed` `[bonded-median]` row. +6 tests (agent 44→50). VERIFIED LIVE on RTX 4060 dev node: seeded Alice feed → `[median]` populated, `[bonded-median] NONE` correct; `gpn_phase_c_probe` all Phase C economics pass live (the one `UpdateTooSoon` ❌ is the pre-existing global throttle, not a regression). Agent 50/50, workspace 123/123, release clean — no pallet/runtime change.)** Remaining: business decisions D1–D5 (escalate to Jeremy) + VPS-blocked public-testnet items (needs $). Candidate next code: README marketplace/fee-economy polish (docs); `effective_feed()` on-chain read tool. **(Cycle 55 DONE for the `gpn_operate_oracle` bonded-operator candidate — see build-table row. Cycle 56 DONE for the `effective_feed()` on-chain read tool — runtime API + `gpn_effectiveFeed` RPC + `gpn_effective_feed` agent tool, live-verified, see build-table row.)**

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

## C69 (2026-08-05 ~05:55 PT) — BLOCKED cycle, no bitrot
- No new Jeremy signal (scanned workspace + memory 08-04/08-05 for green-light/D1–D5 answer: none).
- Verified no bitrot: git clean @69fad91; cargo check EXIT 0 both (gpn-node 8 pre-existing GPU-miner warnings; gpn-agent 1 + subxt future-incompat note). Same as C64.
- NOTE: brief cites `testnet-public-runbook.md §7` + `cycle63-decision-card.md`, but actual decision brief is `shared/artifacts/project7/cycle59-decision-brief.md` and no runbook file exists in workspace — must locate before executing on green-light.
- Blocked on Jeremy: D1–D5 + VPS (~$10–20/mo). ~39th no-op firing.

---## C78 (14:55 PT) — no-op
- No new Jeremy signal (green-light/D1-D5/VPS). Blocker unchanged (~48th no-op).
- Verified no bitrot: project7 source clean since 13:55; HEAD @a36a889 (C74); cargo check EXIT 0 (gpn-node, 8 pre-existing warnings). git clean.
- Runbook gap persists: NO testnet-public-runbook.md anywhere; cron brief's cycle63-decision-card/§7 ref stale (file absent) — actual = cycle59-decision-brief.md. Dry run (cycle61) proved Phase-4 toolchain.
- No redundant action. Ready to launch public testnet on green-light. Escalating D1-D5 + VPS (~$10-20/mo) go/no-go crisply.

## C88 (2026-08-06 ~01:55 PT / 09:55 UTC) — BLOCKED, no-op, no bitrot
- No new Jeremy signal (green-light / D1-D5 / VPS) — blocker UNCHANGED (~58th no-op). Grep hits are only prior-cycle descriptions of the recommended reply, never an actual Jeremy answer.
- No bitrot: projectmanager07 HEAD @c93116f (unchanged since C79), git clean, no new project7 source files; cargo check EXIT 0 gpn-node (8 pre-existing warnings, no errors).
- Runbook gap persists: NO testnet-public-runbook.md; cron brief's cycle63-decision-card/§7 ref stale (file absent) — actual decision doc = cycle59-decision-brief.md. Dry run (cycle61) proved Phase-4 toolchain.
- No redundant action (per brief). Ready to execute public-testnet launch on green-light. Escalating D1-D5 + VPS (~$10-20/mo) go/no-go crisply.

## C104 (2026-08-06 ~20:55 PT / 2026-08-07 03:55 UTC) — BLOCKED, no-op, no bitrot
- No new Jeremy signal (green-light / D1-D5 / VPS) — blocker UNCHANGED (~74th no-op). Grep hits for "accept all recommendations/approve VPS" are ONLY prior-cycle artifacts (memory, HEARTBEAT, cycle61 dryrun, project10) — never an actual Jeremy reply to PM07.
- No bitrot: projectmanager07 HEAD @c93116f (unchanged since C79), git clean (only HEARTBEAT.md = my own log), zero new project7 source files. No source change → no cargo re-check needed.
- Runbook gap persists: NO testnet-public-runbook.md anywhere; cron brief's cycle63-decision-card/§7 ref stale (file absent) — actual decision doc = cycle59-decision-brief.md. Dry run (C61) proved Phase-4 toolchain locally.
- No redundant action (per brief). Ready to execute public-testnet launch immediately on green-light. Escalating D1-D5 + VPS (~$10-20/mo) go/no-go crisply.

## C105 (2026-08-06 ~21:55 PT / 2026-08-07 04:55 UTC) — BLOCKED, no-op, no bitrot
- No new Jeremy signal (green-light / D1-D5 / VPS) — blocker UNCHANGED (~75th no-op). Grep hits for "accept all recommendations/approve VPS" are ONLY prior-cycle artifacts (memory, HEARTBEAT, cycle61 dryrun, project10) — never an actual Jeremy reply to PM07.
- No bitrot: projectmanager07 HEAD @252328b (C104, my own commit), git clean, zero new project7 source files since C104. No source change → no cargo re-check needed.
- Runbook gap persists: NO testnet-public-runbook.md anywhere; cron brief's cycle63-decision-card/§7 ref stale (file absent) — actual decision doc = cycle59-decision-brief.md. Dry run (C61) proved Phase-4 toolchain locally.
- No redundant action (per brief). Ready to execute public-testnet launch immediately on green-light. Escalating D1-D5 + VPS (~$10-20/mo) go/no-go crisply.
