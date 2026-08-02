# HEARTBEAT.md — Project 7: GPU-Native Crypto & AI Compute Network

**Status: Phase 3/4 (Prototype + mainnet prep) — LIVE on RTX 4060. Cycle 58: the oracle's update-feed anti-spam throttle is now PER-OPERATOR instead of network-global (code, no money) — `pallet-profitability-oracle::update_feed` gates on `OperatorLastFeedBlock` (the per-operator map cycle 53 added for staleness) rather than the global `LastUpdateBlock`, so independent committee/bonded operators can each post their own feed without blocking each other; same-operator rate limiting preserved. This closes the cycle-50-flagged design flaw and unblocks the live two-operator aggregation path. VERIFIED LIVE on RTX 4060 dev node (real GPU Ethash): bonded //Dave posted, then //Alice (a different committee operator) posted immediately in the same interval — succeeded (was `UpdateTooSoon` before); Dave's own immediate re-post correctly still `UpdateTooSoon`; `gpn_aggregate_feed` now shows 2 operators with independent feeds + a 2-op median. Oracle pallet 41/41, workspace 125/125, release clean. ALL §6 oracle-governance code gaps #1–#5 CLOSED + per-operator throttle + D3 consumed + Phase-C tooling + on-chain median + explorer integration complete — remaining are business decisions D1–D5 (escalated) + VPS-blocked public-testnet items (needs $).** (Prior cycle 57: the on-chain Phase-C bond-weighted median is now surfaced in the block explorer — `gpn_explorer` reads `get_effective_feed()` (the `gpn_effectiveFeed` RPC from cycle 56) and renders an "On-chain effective feed (Phase-C bond-weighted median)" dashboard section + a dedicated `/api/effective-feed` endpoint + an `effective_feed` object in `/api/chain` (null when no bonded operator has posted). VERIFIED LIVE on RTX 4060 dev node (real GPU Ethash): negative path → `/api/effective-feed` returns `{"error":"no bonded operator has posted a feed yet"}` + `/api/chain` `effective_feed:null`; `//Dave --bond` → bond + bonded `update_feed` ($0.082 / $3.00/hr / 72% / 15%) → `/api/effective-feed` returns exactly that feed @block 25 and the dashboard shows `$0.0820 / $3.00/hr / 72% / 15% @ #25`. Agent 50/50, build clean. ALL §6 oracle-governance code gaps #1–#5 CLOSED + D3 consumed + Phase-C tooling + on-chain median + explorer integration complete — remaining are business decisions D1–D5 (escalated) + VPS-blocked public-testnet items (needs $).** (Prior cycle 56: the pallet's AUTHORITATIVE Phase-C bond-weighted median is now queryable on-chain (code, no money) — `ProfitabilityOracle::effective_feed()` was `pub(crate)` (only used internally for slashing-outlier detection), so the agent could only reimplement the median client-side. Exposed it: pallet `pub`, new `ProfitabilityOracleApi` runtime API (`fn effective_feed()`), new `gpn_effectiveFeed` node RPC (serde struct w/ raw micro-USD/bps + human USD convenience fields), and a read-only `gpn_effective_feed` agent tool (JSON-RPC POST over ws->http, the proven head_block_hash pattern). VERIFIED LIVE on RTX 4060 dev node (real GPU Ethash): negative path before bonding → `NONE` (exit 1); positive path `//Dave --bond` → bond + bonded `update_feed` → `gpn_effective_feed` reads on-chain median `token=$0.082 / gpu=$3.00/hr / 72% / 15% @block 23` (exit 0) — **exactly matches** client-side `gpn_aggregate_feed` bonded-median (identical values @block 23), proving the runtime API surfaces the same bond-weighted median. 123/123 workspace, 50/50 agent, release clean. **ALL §6 oracle-governance code gaps #1–#5 CLOSED + D3 consumed + Phase-C tooling complete + on-chain median exposed** — remaining are business decisions D1–D5 (escalated) + VPS-blocked public-testnet items (needs $).**
**Last Updated:** 2026-08-02 ~08:10 UTC (2026-08-01 ~01:10 PT)
**Caretaker:** PM07

---

## 🚀 Current Milestone (Cycle 57)

**The block explorer now surfaces the on-chain Phase-C bond-weighted median — the authoritative feed the auto-switch consumes (code, no money).** Cycle 56 exposed `ProfitabilityOracle::effective_feed()` as the `gpn_effectiveFeed` RPC, but `gpn_explorer` (cycle 45) still only showed the latest posted feed. This cycle wires the on-chain median into the explorer:

1. **`chain_summary()`** reads `get_effective_feed()` and includes an `effective_feed` object in `/api/chain` (and `/api/feed`); `null` when no bonded operator has posted.
2. **New `/api/effective-feed` endpoint** — returns just the on-chain median, or `{"error":"no bonded operator has posted a feed yet"}` (HTTP 200) when unset.
3. **Dashboard** — new "On-chain effective feed (Phase-C bond-weighted median)" section (Token $ / GPU $/hr / Utilization / Staking APR / @ block) right under the profitability feed, with a graceful empty-state.

**VERIFIED LIVE on RTX 4060** (`gpn-node --dev`, real GPU Ethash, DAG 1023 MiB): negative path (fresh chain) → `/api/effective-feed` = error object + `/api/chain` `effective_feed:null`; positive path `gpn_operate_oracle … //Dave --bond` → `✅ bond_operator` → `✅ update_feed` ($0.082 / $3.00/hr / 72% / 15%) → `/api/effective-feed` returns exactly that feed `@block 25`, `/api/chain` populated, dashboard section shows `$0.0820 / $3.00/hr / 72% / 15% @ #25`. Node kept mining GPU blocks throughout (head #26). **Agent 50/50, build clean — explorer bin only, no pallet/runtime change.**

**Gotchas:** `gpn_operate_oracle` CLI is positional (`[endpoint] [values_file] [interval_secs] [signer_uri] [--bond]` — `--bond` is the 5th arg); values TOML field is `token_price_usd` (micro-USD), not `token_price_micro_usd`.

**Next:** all oracle-governance code gaps #1–#5 CLOSED + D3 consumed + Phase-C tooling + on-chain median + explorer integration complete. Remaining: business decisions **D1–D5** (`05-oracle-governance.md` §7 — token listing, marketplace go-live, governed sudo replacement, Phase C go-live gating, testnet root) → escalated to Jeremy; Phase-4 public-testnet items (public bootnodes/wss/explorer) remain **VPS-blocked** (escalated, needs $). Candidate next code: README marketplace/fee-economy polish (docs), or a live two-operator bond-weighted-median seed (gated by the 600-block `UpdateTooSoon` throttle).

---

## 🚀 Previous Milestone (Cycle 55)

**The maintained oracle feed loop now supports Phase-C BONDED operators — a permissionless operator can run it without root `add_operator`/committee admission (code, no money).** Cycle 54 made the *agent* consume the bond-weighted median; cycle 53 made the *pallet* accept bonded operators; but the *feed-maintenance service* (`gpn_operate_oracle`) still only worked for committee operators like `//Alice`. This cycle closes that last gap:

1. **New `--bond` flag** (5th CLI arg, truthy `--bond|true|1|yes`) in `gpn_operate_oracle`: on startup, resolve the signer's 32-byte account and call `ChainClient::get_staked_bond(account)`.
2. **Already bonded** → log bond size in GPN (raw/1e12) and skip (idempotent).
3. **Not bonded** → submit `ProfitabilityOracle.bond_operator` (empty args) via the dispatch-outcome-verified `submit_signed`; `AlreadyStakedOperator` on a race is non-fatal, anything else fatal.
4. Loop proceeds unchanged — the pallet's `update_feed` authorizes `is_committee || is_staked`, so a bonded operator posts feeds exactly like a committee operator, weighted by bond size in the on-chain bond-weighted median.

**VERIFIED LIVE on RTX 4060** (`gpn-node --dev`, real GPU Ethash, DAG 1023 MiB): `//Dave` (non-committee, genesis-funded) → `🔐 not bonded — submitting bond_operator (1,000 GPN)` → `✅ bond_operator dispatched successfully` → iteration #1 `✅ update_feed dispatched successfully` (feed $0.082 / $2.75/hr / 68% / 14% posted by a permissionless operator) → `gpn_aggregate_feed` shows `[operator-feeds] 1 operator` (Dave @block16) + `[median]` AND `[bonded-median]` both populated. Idempotency: re-run with `--bond` → `🔒 already a bonded operator (bond = 1000 GPN); skipping bond_operator`.

**🐛 Display bug fixed:** bond printed "1 GPN" (divided raw by 1e15); 1,000 GPN = 1e15 raw at 12 decimals, so corrected divisor to 1e12 → "1000 GPN".

**Agent 50/50 (+0, no new tests needed — tooling only), release build clean, no pallet/runtime change.**

**Next:** all oracle-governance code gaps #1–#5 are CLOSED, D3 aggregation is consumed end-to-end, and Phase-C tooling is complete. Remaining: business decisions **D1–D5** (`05-oracle-governance.md` §7 — token listing, marketplace go-live, governed sudo replacement, Phase C go-live gating, testnet root) → escalated to Jeremy; Phase-4 public-testnet items (public bootnodes/wss/explorer) remain **VPS-blocked** (escalated, needs $). Candidate next code: README marketplace/fee-economy polish (docs), or an `effective_feed()` on-chain read tool for the Phase-C bond-weighted median.

---

## 🚀 Previous Milestone (Cycle 54)

**The agent now consumes the Phase-C bond-weighted median end-to-end — closes the spec §8/D3 code item (the last code work item in the oracle-governance roadmap).** The pallet exposed the on-chain `StakedOperators` bond map in cycle 53; the agent previously only aggregated client-side with a weight-agnostic median (`median`). This cycle added the **bond-weighted** aggregation path:

1. **`ChainClient::get_staked_bonds()`** — enumerates `ProfitabilityOracle::StakedOperators` (subxt `.iter()`, same proven dynamic-storage pattern as `get_operator_feeds`), returns `(hex account, bond GPN-raw)`. The bond-weight side of the aggregation.
2. **Pure `bond_weighted_median_f64(weights, values)`** — sort by value, walk cumulative bond weight, take the value at the 50% cumulative-weight point. Large-bond dominates; tiny-bond outlier excluded; zero/absent bonds + non-finite dropped.
3. **Pure `aggregate_bonded_median_feed(feeds, bonds)`** — per-field bond-weighted median + max `updated_at_block` (parallel arrays; `None` on empty/len-mismatch).
4. **`analyze()` `feed_aggregation="bonded_median"` branch** — reads `get_operator_feeds()` + `get_staked_bonds()`, aligns by account key (keeps only operators present in BOTH maps: bonded AND have posted), aggregates. Warn + fall back to `CurrentFeed` if none.
5. **`gpn_aggregate_feed`** — now also prints a `[bonded-median]` row + verdict.
6. **+6 unit tests** (agent lib 44→50): large-bond-dominates-tiny-bond-outlier (a $9,999/hr outlier cannot move gpu price), single-operator, zero-bond/non-finite exclusion, empty/all-zero none, aggregate weights-by-bond, len-mismatch none.

**Verified LIVE on RTX 4060** (`gpn-node --dev`, real GPU Ethash): seeded Alice feed → `gpn_aggregate_feed` shows `[operator-feeds] 1 op`, `[median]` populated, `[bonded-median] NONE` (correct — no operator has BOTH bonded AND posted; verifies the align logic + fallback). `gpn_phase_c_probe` (//Dave bond, //Charlie challenge) → all Phase C economics pass live (bond 1,000 GPN → bond_more 1,100 → challenge → Dismiss → unbond); the single `❌ bonded update_feed failed: UpdateTooSoon` is the **pre-existing global 600-block throttle** (cycle-50 design finding — only one operator can post per interval; Alice had just posted @block14), NOT a regression.

**Agent 50/50 (+6), workspace 123/123, release build clean — no pallet/runtime change (agent + tooling only).**

**Next:** all §6 oracle-governance code gaps #1–#5 are CLOSED and the D3 aggregation is consumed end-to-end by the agent. Remaining: business decisions **D1–D5** (`05-oracle-governance.md` §7 — token listing, marketplace go-live, governed sudo replacement, Phase C go-live gating, testnet root) → escalated to Jeremy; Phase-4 public-testnet items (public bootnodes/wss/explorer) remain **VPS-blocked** (escalated, needs $). Candidate next code: `gpn_operate_oracle` bonded-operator support (let a bonded operator run the long-lived feed-maintenance loop), or README marketplace/fee-economy polish (docs).

---

## 🚀 Previous Milestone (Cycle 53)

**Implemented + live-verified the biggest remaining build — oracle-governance §6 gap #5: the Phase-C STAKING-BACKED SLASHING ORACLE (code, not docs).** The oracle's operator set is now **permissionless and economically-secured**: anyone bonds GPN to become an operator, and a stale/outlier operator can be slashed via a challenge anyone can file. Implemented **inside `pallet-profitability-oracle`** (NOT a new pallet crate, avoiding this project's sp-core version-conflict history — see cycles 21/25). All §6 oracle-governance code gaps #1–#5 are now **CLOSED**.

1. **Pallet** — new `StakedOperators` (bond map, ValueQuery), `OperatorLastFeedBlock` (per-operator staleness), `OpenChallenges` (challenge struct), `ChallengeVerdict`; new extrinsics `bond_operator` / `bond_more` / `unbond_operator` (blocked while challenged) / `challenge_operator` / `resolve_challenge` (SlashStale | SlashOutlier | Dismiss); `update_feed` now also authorizes **bonded operators** + sets `OperatorLastFeedBlock`; on-chain **bond-weighted median** `effective_feed()` (read-only; a small-bond outlier can't move it, a large bond dominates). Config: `OperatorBond`/`ChallengeBond`/`OutlierThresholdBps`/`StaleOperatorBlocks`/`SlashBps`/`Currency`. **D4 default = BURN** (slashed operator bond + frivolous-challenger bond burned via `Currency::slash`, aligning with gpn-token's 20% burn); `SlashBps`=10%. +13 tests (oracle 26→39).
2. **Runtime** — wired the new oracle Config: `OperatorBond`=1,000 GPN, `ChallengeBond`=100 GPN, `OutlierThresholdBps`=2,000 (20%), `StaleOperatorBlocks`=14,400 (~60h), `SlashBps`=1,000 (10%), `Currency=Balances`.
3. **Agent** — `ChainClient::get_staked_bond()` reader + new `gpn_phase_c_probe` live-verification binary.
4. **VERIFIED LIVE on RTX 4060** (`gpn-node --dev`, real GPU-mined blocks, `gpn_phase_c_probe` exit 0): `bond_operator` by Dave → 1,000 GPN bond on-chain → bonded `update_feed` accepted → `bond_more(+100)` → 1,100 GPN → `challenge_operator` by Charlie → `resolve_challenge(Dismiss)` → challenge closed, operator bond intact (frivolous challenge burned challenger's bond) → `unbond_operator` → removed + bond returned.
5. **Bugs fixed during implementation:** `account_id()` needed `PalletId` + `AccountIdConversion`; `ChallengeVerdict` needs `DecodeWithMemTracking` → extrinsic takes a `u8` verdict discriminant; **unit bug** `OperatorBond` was `1e18` raw = **1,000,000 GPN** (12 decimals) not 1,000 GPN = `1e15` (Dave's 100k GPN couldn't cover it → `Arithmetic(Underflow)` live) — fixed; unbond/bond-return uses `AllowDeath` (pallet account may drain below ED).
6. **Tests:** `cargo test --workspace` (gpn-node) **123/123**; `cargo test --lib --release` (gpn-agent) **44/44** (+8 GPU-ignored); release build clean.

**Next:** all §6 oracle-governance code gaps are CLOSED. Remaining: the **business decisions D1–D5** (`05-oracle-governance.md` §7 — token listing, marketplace go-live, governed sudo replacement, Phase C go-live gating, testnet root) → escalated to Jeremy; and Phase-4 public-testnet items (public bootnodes/wss/explorer) remain **VPS-blocked** (escalated, needs $). Candidate next code: agent `feed_aggregation="bonded_median"` wiring, or `gpn_operate_oracle` bonded-operator support. Prior milestone (cycle 51) — Phase B committee admission:

**Closed oracle-governance §6 gap #4 — Phase-B COMMITTEE ADMISSION for oracle operators (runtime, no money).** §6 gaps #1–#4 are now ALL CLOSED. The oracle's `add_operator`/`remove_operator` were root-only, so a single compromised root key could replace the whole trusted feed-writer set. Phase B replaces that with a **named committee** that votes to admit operators (threshold approvals) — implemented **directly in the pallet** (no new pallet / no new deps, avoiding this project's sp-core version-conflict history):

1. **Pallet** — `Committee` (genesis-seeded `StorageMap`) + `PendingAdmissions` (`StorageMap<AccountId, PendingAdmission { proposer, approvals: BoundedVec }>`); `propose_operator` (committee member, self-approves; `AdmissionThreshold==1` admits immediately, else stores pending) + `approve_operator` (committee member; auto-admits at threshold, clears pending); `remove_operator` = committee member OR root (emergency break-glass; mainnet root is inert → committee-only in practice). +10 tests.
2. **Runtime** — `CommitteeSize=8`, `AdmissionThreshold=2` wired into `pallet_profitability_oracle::Config`.
3. **Chain specs** — dev [Alice, Bob], local/testnet [Alice, Bob, Charlie] committee; **mainnet EMPTY committee** (inert/safe, mirrors empty operators).
4. **`gpn_committee_probe`** — new tool exercising the full dispatch path live.

**Verified LIVE** (`gpn-node --dev`, RTX 4060, real GPU-mined Ethash blocks #24+): committee seeded on-chain (Alice/Bob yes, Dave no) → Alice `propose_operator(//Charlie)` → pending stored → Bob `approve_operator` → threshold met, **Charlie admitted**, pending cleared → Charlie `update_feed` **accepted** (real operator) → non-member Dave `propose_operator` **FAILED dispatch** `ModuleError(ProfitabilityOracle::NotCommitteeMember) [pallet=9 error=6]` (surfaced as Err by the cycle-39 dispatch-outcome-verified `submit_signed`). **Oracle pallet 26/26 (+10), workspace 110/110.**

**Design notes:** removal keeps a root break-glass (mainnet-inert → committee-only in practice); `AdmissionThreshold`/`CommitteeSize` are runtime constants (governance-updatable storage = Phase C). Next §6 gap: #5 → **Phase-C** staking-backed slashing oracle (new pallet, bigger build, gated on Phase A/B economics).

---

## 🚀 Previous Milestone (Cycle 50)

**Closed oracle-governance §6 gap #3 — the agent can now aggregate ALL operators' feeds via a per-field median, so one outlier operator can't move the effective feed that drives the auto-switch.**

The oracle drives the auto-switch, and previously the agent trusted a **single** feed (`CurrentFeed` = last operator to write) — so one **outlier operator** could still move the effective price and force a mass switch. Now each operator's own latest feed is stored on-chain and the agent can median them:

1. **Pallet** — new `OperatorFeeds: StorageMap<AccountId, ProfitabilityFeed>` (latest feed **per operator**, populated on every `update_feed`; `CurrentFeed` unchanged as effective/latest — fully backward-compatible, no existing consumer changes). +2 tests (per-operator storage, same-operator overwrite).
2. **Agent reader** — `get_operator_feeds()` enumerates the map via subxt `.iter()` (decodes each operator AccountId + feed); shared `parse_feed_value()` refactor.
3. **Pure median** — `median_f64` (finite-only, outlier-robust) + `aggregate_median_feed` (per-field median + max `updated_at_block`). Unit-proven: `[2, 3, 9999] → 3` — a $9,999/hr outlier operator cannot move the GPU-hour median. (+6 tests)
4. **Config** — `feed_aggregation = "latest" | "median"` (serde default `"latest"`, no behaviour change for existing nodes).
5. **`analyze()`** — `"median"` branch aggregates all operator feeds (falls back to `CurrentFeed` with a warn if none posted); staleness TTL + reasonableness bounds still apply on top.
6. **`gpn_aggregate_feed`** — new read-only tool: prints every operator's feed + the median.

**Verified LIVE** (`gpn-node --dev`, RTX 4060, real GPU-mined blocks): seeded Alice (token $0.075 / gpu $2.50/hr / util 65% / apr 12%) → `gpn_aggregate_feed` reads the new map (`#1 0xd43593c715… token=$0.075 … @block16`) + median = Alice (1 op); `gpn_check_feed` backward-compat on `CurrentFeed` → compute $1.38/hr → reasonable (exit 0). Multi-operator median is unit-proven (identical code path reads more map entries); a live 2-operator seed is gated by the 600-block `UpdateTooSoon` throttle (~80 min on this node) so wasn't run. **Agent 44/44 (+7), node 100/100 (+2), pallet 16/16 (+2).**

**Design finding:** `UpdateTooSoon` uses a SINGLE global `LastUpdateBlock`, so only one operator can post per 600-block interval — fine for a small named set on rotation; make it **per-operator** in Phase B/C if concurrent independent operators are wanted.

**Next §6 gaps, in order:** #4 → **Phase-B** council/multisig admission (runtime); then Phase-C staking-backed slashing oracle (runtime).

---

## 🚀 Previous Milestone (Cycle 49)

**Closed oracle-governance §6 gap #2 — the agent no longer trusts an implausible profitability feed (code, no money, high safety value).**

The oracle drives the auto-switch; a single bad/outlier feed value (the cycle-31 USD² unit bug class, a garbage GPU-hour price, or NaN/Inf) would otherwise inflate compute revenue and force a mass switch to Compute. Now `analyze()` guards `calc_compute_revenue`:

1. **`compute_revenue_reasonable(revenue, max_usd_per_hour)`** — pure, unit-tested; true iff finite, > 0, ≤ max. Since compute revenue is monotonic in both inputs, one output ceiling bounds every implausible input combination.
2. **Config** — `compute_revenue_max_usd_per_hour: f64` default **50 USD/hr** via `#[serde(default = "default_compute_revenue_max")]` (very generous; never blocks a legitimate single-GPU switch).
3. **`analyze()`** — implausible revenue → warn log + compute=0 (conservative Mining default, mirroring the cycle-48 staleness-TTL fallback).
4. **`gpn_check_feed`** — new read-only diagnostic: prints the live feed, raw compute revenue, and reasonableness verdict (exit 0=plausible, 1=implausible/absent).

**Verified LIVE** (`gpn-node --dev`, RTX 4060, real GPU-mined blocks): seeded `gpu=$10,000/hr, util=100%` → `compute=$8,500/hr` → `reasonable=false` (exit 1, rejected, forces Mining); seeded `gpu=$2.50/hr, util=65%` → `compute=$1.38/hr` → `reasonable=true` (exit 0, accepted). **Agent 37/37 (+4), node 98/98** — no pallet/runtime change.

---

## 🚀 Previous Milestone (Cycle 47)

**Verified mainnet is safe-by-default + drafted the oracle-governance spec — two runbook §9 mainnet open items advanced with no money and no code changes.**

**(a) Mainnet sudo is provably inert (runbook §9 #1, "non-functional" half).** Ran `gpn-node build-spec --chain=mainnet` on the current release binary and decoded the genesis JSON:
- **Sudo key** = `5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM` → decodes to prefix `42` + payload **`0x0000…00` all-zeros** = `AccountId::new([0u8;32])`. Nobody has that private key → `Sudo.sudo` (and every root-only call: oracle `add_operator`, `GPNToken.distribute_fees`, …) is **unreachable on mainnet**.
- **`balances=[]`** (fair launch, genesis supply 0) and **oracle operators `[]`** (feed locked by default) both confirmed.
- So mainnet cannot have a feed written, an operator added, or fees root-dispatched until a governance path exists — the safe-by-default posture. The *governed replacement* path remains a business decision (escalate).

**(b) Oracle governance spec drafted (runbook §9 #5):** new `design/protocols/05-oracle-governance.md` — operator-side accountability for the feed that drives the auto-switch: 3-phase governance (A = root-managed named set, **ZERO runtime change**, testnet-ready, client-side median → B = committee/multisig admission → C = permissionless staking-backed operators with bond + on-chain median + slashing), full threat model (collusion / single-root capture / stale feed / outlier / sybil), trusted data-source table (token price / GPU-hour price / utilization / APR) with no-self-dealing + reasonableness bounds, §6 ordered code gaps (agent feed-staleness TTL, feed reasonableness bounds in `calc_compute_revenue`, client-side median, then Phase-B/C pallets), §7 open decisions D1–D5 → escalate to Jeremy.

**Docs updated:** runbook §9 #1 marked "non-functional half VERIFIED (Cycle 47)" + #5 marked "Spec DRAFTED"; README project-structure pointer; cycle log; HEARTBEAT; memory. No code changed (docs + verification only).

---

## 🚀 Previous Milestone (Cycle 45)

**First concrete piece of HEARTBEAT priority #1 (public-testnet tooling, runbook §7) — `gpn_explorer`: a read-only web block explorer + JSON API built + verified live.**

A lightweight explorer (`gpn-agent/src/bin/gpn_explorer.rs`) that serves a dark-mode HTML dashboard + JSON endpoints over any node's JSON-RPC. Read-only (no signer, no writes) — safe to point at a public node:

1. **Endpoints** — `GET /` (HTML: head block #/hash, live difficulty, profitability feed, account panel, recent blocks, jobs) · `/api/chain` (head+difficulty+feed) · `/api/blocks?count=` (recent hashes) · `/api/account?address=` (ss58 | `//dev` URI | 0x-hex → balance/nonce/staked/attested/tier/GFLOPS) · `/api/jobs` (all marketplace jobs).
2. **Reuses `ChainClient`** for storage reads (difficulty, feed, accounts, attestation, jobs — same proven subxt dynamic-storage + head-block paths) and raw `chain_getHeader`/`chain_getBlockHash` HTTP RPC for block headers (mirrors the faucet's `rpc_call`). No new deps (`tiny_http` already present from the faucet).
3. **🐛 Bug found & fixed** — Substrate's `chain_getHeader` result has **NO `hash` field**; the first cut read `header.hash` so `head_block` returned `None` (dashboard showed head #0 + empty blocks). Fixed by fetching the head hash via `chain_getBlockHash([number])`.

**Verified LIVE (`gpn-node --dev`, RTX 4060, GPU-mined):** `/api/chain` → head #30 with real hash `0x7010bce8…`, difficulty **4096** (real, self-tuned from the pallet target — not a placeholder), feed fallback (no oracle operator seeded on dev — correct); `/api/blocks?count=5` → real hashes 30→26; `/api/account?address=//Alice` → `5GrwvaEF…`, 100000 GPN, unstaked/unattested; dashboard HTML renders head/difficulty/feed + Alice+Bob rows + recent blocks; created a real job (`gpn_create_job //Bob 150 50`) → `/api/jobs` shows **job #1, Pending, 50 GPN, min 150 GFLOPS, requestor Bob `0x8eaf…`**, `next_job_id=2` (live on-chain data). Error paths: missing address → **400**, unknown path → **404**.

**Verified:** `cargo build --release --bin gpn_explorer` clean; agent lib **26/26** (7 GPU-ignored) — unchanged. No pallet/runtime change. Detail: `shared/artifacts/project7/cycle45-explorer-live-log.md`

---

## 🚀 Previous Milestone (Cycle 44)

**Closed HEARTBEAT priority #1 (post-cycle-43) — `gpn_faucet`: the off-chain drip faucet (runbook §5 option 3, ZERO runtime change) built + verified live.**

A small HTTP service (`gpn-agent/src/bin/gpn_faucet.rs`, new `tiny_http` dep) backed by a funded testnet key that drips GPN via a rate-limited signed `Balances.transfer_allow_death`:

1. **Endpoints** — `GET /drip?address=<ss58|0xhex>&amount=<gpn>` (drip, default `--drip-gpn`) and `GET /status` (faucet balance + config + per-address rate-limit ledger).
2. **Rate limiting (per address)** — `--cooldown-secs` (3600), `--daily-cap-gpn` (100k), `--total-cap-gpn` (1M), `--reserve-gpn` (1000; faucet refuses to drain itself below reserve — operator mines GPN into the faucet key first, since testnet genesis has zero supply).
3. **Reuses the shared signer plumbing** (`signer::Sr25519Signer` + `signer::submit_signed`, dispatch-outcome verified) so a drip that fails dispatch is reported as an error, not a false success.
4. **Two live findings** — (a) the call is **`Balances.transfer_allow_death`** (this Substrate version renamed `transfer`; confirmed via metadata probe); (b) **dest is bare account bytes, not `MultiAddress`** (GPN `type Address = AccountId`; first MultiAddress attempt panicked, fixed to match the proven `gpn_distribute_rewards` pattern). Also fixed a tokio pattern: plain `fn main` + one runtime + `handle.block_on` (not `#[tokio::main]` + nested `Runtime`).

**Verified LIVE (`gpn-node --dev`, RTX 4060):** 5000 GPN drip to Bob landed on-chain (authoritative subxt head read: Alice 100000→95000, dispatch-outcome ok); immediate 2nd drip → HTTP **429** cooldown; hex-address drip (Charlie, 2000 GPN) → ok; invalid address → **400**; missing address → **400**; reserve guard (`--reserve-gpn 95000` vs 93000 balance) → **500**.

**Verified:** `gpn_faucet` unit tests **7/7**, agent lib **26/26** (7 GPU-ignored), node workspace **98/98** — all green; release build clean (only `tiny_http` added). Detail: `shared/artifacts/project7/cycle44-faucet-live-log.md`

---

## 🚀 Previous Milestone (Cycle 43)

**Closed HEARTBEAT priority #2 (Phase-4 prep) — `testnet_chain_spec()` implemented + verified live.**

Added `--chain=testnet` as a config-only fork of mainnet (the runbook's #1 remaining checklist item), so a public testnet can be stood up without touching the pristine fair-launch mainnet runtime.

1. **Two-node GPU p2p testnet re-verified** on the current release binary (post cycle-39/40/41 fixes, real OpenCL miner): `gpn_two_node_testnet.sh` → **6/6 passed**. Both nodes mined real Ethash on the RTX 4060 (`GPU Ethash miner online: NVIDIA GeForce RTX 4060`, no CPU fallback), connected as peers over libp2p, converged on the same longest chain (**best: #7**). Same integration the CI `two-node-gpu-testnet` job runs on a self-hosted `gpu` runner (`workflow_dispatch`).
2. **`shared/artifacts/project7/testnet-public-runbook.md`** — public-testnet spec/runbook: chain-spec matrix (dev/local/testnet/mainnet), testnet topology (bootnodes + external GPU miners via RPC), genesis params (config-only fork of the fair-launch mainnet runtime), faucet options (recommends off-chain drip — zero runtime change), operator onboarding (4-step join), launch checklist, and mainnet open items (sudo rotation, token listing → business, marketplace go-live, oracle governance).
3. **README** — added pointer to the runbook in project-structure.

**Verified:** no code changed (docs + live re-verify only); two-node testnet **6/6 passed** on the fixed runtime. Detail: `shared/artifacts/project7/cycle42-testnet-runbook-log.md`

---

## 🚀 Previous Milestone (Cycle 41)

**The live profitability feed is now a maintained service, not a one-shot seed.** Priority #2 (fresh post-cycle-40 list) is closed: new `gpn_operate_oracle` binary in `gpn-agent` — a long-running operator loop that keeps `ProfitabilityOracle.update_feed` fresh, mirroring `gpn_seed_feed` (cycle 22) but built for production operation.

1. **Values from a TOML file, reloaded every iteration** (`oracle_values.toml`, default template written on first run) — an external price feed / operator updates token price, GPU-hour price, utilization, staking APR in the file **without restarting** the loop. Units match the pallet (micro-USD + basis points).
2. **Respects `UpdateTooSoon`** (pallet `OracleUpdateInterval` = 600 blocks ≈ 2.5h) — never spams; logs `⏳ skipped: pallet interval not yet elapsed (UpdateTooSoon)` and waits (the expected back-off, not an error).
3. **Fatal vs. retry error handling** — `NotAuthorized` (operator not in `OracleOperators`, root `add_operator`) exits with a clear message; transient errors log + retry.
4. **Reuses the cycle-39 dispatch-outcome-verified `signer::submit_signed`** + the cycle-27 head-block nonce read.

**Verified LIVE (`gpn-node --dev`, RTX 4060):** iteration #1 `✅ update_feed dispatched successfully` (feed set on-chain); iteration #2 on the same chain `⏳ update_feed skipped … (UpdateTooSoon)` — since `LastUpdateBlock` is only written on a *successful* update, hitting the guard on the second run proves the first feed is genuinely on-chain. Node mined to block #15 during the test, PoW undisturbed.

**Verified:** `cargo build --release --bin gpn_operate_oracle` clean (no pallet/runtime change; agent-only tooling). Node 98/98 + agent 26/26 test suites unaffected. Detail: `shared/artifacts/project7/cycle41-oracle-operator-live-log.md`

---

## 🚀 Previous Milestone (Cycle 40)

**Closed priority #5 — the complete revenue loop now runs end-to-end live on the fixed runtime.** This is the first time the entire economy was exercised in one session on the cycle-39 (dispatch-outcome-verified) node:

1. **Two-account marketplace flow** (`gpn_two_account_demo`, Bob→Alice, 50 GPN): Bob escrowed 50, Alice earned 47.5 (price − 5% fee), distinct accounts, job Completed, all assertions green; every real extrinsic logged `tx dispatched successfully` (no false Err — cycle-39 fix holds on the success path).
2. **Dispatch-outcome failure path** (`gpn_verify_dispatch`): guaranteed-fail `accept_job(999999999)` → `Err: ModuleError(ComputeMarketplace::JobNotFound) [pallet=8 error=2]` (Alice was registered, so the real first-failing guard surfaced). ✅
3. **GPU attest** (`gpn-agent stake_only` on RTX 4060): deposited 1,000 GPN stake (cycle-38 unit fix) + measured 196 GFLOPS → on-chain `staked=true attested=true tier=Bronze @ 196 GFLOPS`; second run correctly surfaced `AlreadyAttested` (idempotency + dispatch path both work).
4. **Fee-economy → tier-weighted payout** (`gpn_distribute_rewards`): `distribute_fees(1000)` → StakingRewardPool 0→400, OracleRewardPool 0→100; `distribute_staking_rewards([Alice])` drained staking 400→0, `distribute_oracle_rewards([Alice])` drained oracle 100→0; Alice +500 GPN (400 staking + 100 oracle − fees), balance 99047.5→99547.5.

**Verified:** `cargo test --workspace --release` (gpn-node) → **98 passed / 0 failed**; `cargo test --lib --release` (gpn-agent) → **26 passed / 0 failed / 7 GPU-gated ignored**. No source changes this cycle (pure live verification + docs). Detail: `shared/artifacts/project7/cycle40-full-loop-live-log.md`

---

## 🚀 Previous Milestone (Cycle 39)

**Closed the cycle-38 robustness finding: `signer::submit_signed` (and every agent caller that uses it) now verifies the DISPATCH OUTCOME, not just inclusion.** The old code broke on `TxStatus::InBestBlock` and returned `Ok` unconditionally — a tx that was *included but failed dispatch* (still lands in a block, with a `System::ExtrinsicFailed` event) was logged as "succeeded". This is exactly what masked the cycle-38 `MinAttestationStake` unit bug: the attestation `deposit_stake` failed dispatch (insufficient balance) but was reported as success.

1. **Dispatch-outcome check in `submit_signed`** — after inclusion, reads the block's `System::Events` via the proven dynamic-storage path (`client.storage().at(hash).fetch`), decodes with `subxt::events::Events::decode_from`, scans for `System::ExtrinsicFailed`, and decodes the module-error **pallet/error indices** from the raw SCALE bytes (variant 3 = `Module`, byte[1]=pallet, byte[2]=error, legacy 2-byte + new 5-byte forms), resolving names from metadata → `ModuleError(ComputeMarketplace::JobNotFound) [pallet=8 error=2]`.
2. **Avoided subxt's broken event path** — `TxInBlock::wait_for_success`/`fetch_events` route through `backend.storage_fetch_value`/`block_body`, which hit a `Serde json error` on the GPN runtime's custom types (they catch the failure but give an unhelpful message). The dynamic-storage path is proven to work (same as `current_nonce`).
3. **New `gpn_verify_dispatch` probe** — submits a guaranteed-fail extrinsic (accept non-existent job) and asserts `Err` + resolvable module error. Exit 0 = verified.
4. **Verified LIVE on RTX 4060** — failure path: `Err: ModuleError(ComputeMarketplace::ProviderNotRegistered) [pallet=8 error=13]` (Alice unregistered), then after registration `ModuleError(ComputeMarketplace::JobNotFound) [pallet=8 error=2]` (real first-failing guard). Success path: `gpn_two_account_demo` (Bob escrows 50, Alice earns 47.5) logged `tx dispatched successfully` for every real tx — the fix correctly distinguishes success from failure.

**Verified:** `cargo test --workspace --release` (gpn-node) → **98 passed / 0 failed**; `cargo test -p gpn-agent --lib --release` → **26 passed / 0 failed / 7 GPU-gated ignored** (was 22; +4 dispatch-error unit tests).

Detail: `shared/artifacts/project7/cycle39-dispatch-outcome-verify-log.md`

---

## 🚀 Previous Milestone (Cycle 38)

**Fixed a latent consensus-affecting unit bug and verified the tier-weighted staking payout live on the RTX 4060.** The cycle-35 faucet cut (1T → 100k GPN) exposed that `MinAttestationStake` was `1e21` raw = **1 billion GPN**, not the intended **1,000 GPN** (= 1e15) — the old 1-trillion faucet masked it (cycle 27's "1,000 GPN" attest was a misread; the value was 1B). With the faucet now 100,000 GPN, the 1B stake was unaffordable → `deposit_stake` failed dispatch → attestation silently broken on dev.

1. **Bug fixed** — `MinAttestationStake` in `runtime/src/lib.rs` and the agent config default corrected `1e21` → `1_000_000_000_000_000` (1,000 GPN, 12 decimals). Consensus-affecting; node + agent + WASM rebuilt.
2. **Secondary gap documented** — `signer::submit_signed` only confirms *inclusion*, not *dispatch success*, so failed extrinsics log "succeeded". This masked the bug; flagged as a follow-up robustness fix.
3. **Verified LIVE** (`gpn-node --dev`, RTX 4060): agent deposit now logs 1e15 raw = 1,000 GPN; `gpn_check_state` → `staked=true attested=true tier=Bronze @194 GFLOPS`, balance −1,000 (100000 → 98999.9997). Then `gpn_distribute_rewards` (seed 3000 → distribute 1000): `distribute_fees(1000)` → StakingRewardPool 0→400, OracleRewardPool 0→100; `distribute_staking_rewards([Alice])` → **pool drained 400→0** (was HELD at 400 in cycle 37 when Alice was unattested); oracle drained; Alice +~500 GPN (400 staking + 100 oracle − fees). **Proof-of-staked-GPU now pays attested stakers live.**

**Verified:** `cargo test --workspace --release` → **98 passed / 0 failed**; node mined blocks #0+ on the new runtime during the attest + distribute runs (PoW path unaffected).

Detail: `shared/artifacts/project7/cycle38-stake-unit-bug-attest-payout-log.md`

---

## 🚀 Previous Milestone (Cycle 37)

**Closed the funding gap from cycle 36 — the fee economy is now self-funding on-chain.** The blocker was that marketplace fees (5%) landed in the `gpn/cmkt` account while `distribute_fees` draws from `gpn/tokn`, so the pools stayed empty in normal operation.

1. **`FeeSinkProvider` trait in `gpn-token`** (mirrors the `GPUAttestationProvider` pattern) exposing `fee_sink()` → returns the token pallet account. Implemented for `Pallet<T>`.
2. **Marketplace routes the fee** — new `Config::FeeSink: FeeSinkProvider`; `complete_job` transfers the fee from `gpn/cmkt` to the token pallet account (AllowDeath, since sweeping may empty the escrow-only marketplace account). Runtime wires `type FeeSink = GPNToken;`.
3. **🐛 Tool panic fixed** — `signer::current_nonce` used `reqwest::blocking::Client` inside tokio → "Cannot drop a runtime in a context where blocking is not allowed". Replaced with async client. `gpn_distribute_rewards` now completes end-to-end.
4. **Verified LIVE** (`gpn-node --dev`, RTX 4060 mining): `gpn/tokn` 0 → **2.5 GPN** after a 50 GPN job completed (5% fee routed in); then `distribute_fees(1000)` → StakingRewardPool 0→400, OracleRewardPool 0→100, oracle drained to Alice (+100), staking pool held for unattested Alice (tier gate intact).

**Verified:** `cargo test --workspace` → **98 passed / 0 failed** (+1 new fee-routing test); node + runtime WASM build clean; chain mined blocks #0–#12 live (PoW path unaffected).

Detail: `shared/artifacts/project7/cycle37-fee-routing-live-log.md`

---

## 🚀 Previous Milestone (Cycle 35)

**Closed the last two Phase-4 tokenomics open questions in code — supply cap decided, mainnet fair-launch spec added, and a critical block-reward bug fixed.** This makes the token economy coherent for mainnet: emission is disinflationary *and* absolutely bounded.

1. **Hard supply cap = 1B GPN.** `MaxSupply` in the runtime gpu-pow config was `10^24` raw = **1 trillion GPN** (comment wrongly said 1B). Corrected to `10^21` raw = **1B GPN**. The cap was already enforced in `distribute_pending_rewards` (guard + `test_supply_cap_halts_mining`); now the value matches the design.
2. **Mainnet fair-launch chain spec.** New `mainnet_chain_spec()` (`--chain=mainnet`, `ChainType::Live`) with **zero endowed accounts and zero oracle operators** — all GPN issued via GPU PoW. Dev/local faucet cut from 1 trillion GPN (equal to the whole old cap) to a sane **100,000 GPN** per account.
3. 🐛 **Block-reward bug fixed (critical).** The reward constant was `5×10^19` raw = **50,000,000 GPN**, but the spec documents **50 GPN/block**. With a 1B cap, a 50M reward would exhaust supply in **~20 blocks**, silently halting mining. Fixed to 50 GPN in runtime + test constant + chain spec.

**Verified:** `cargo test --workspace` → **97 passed / 0 failed** (gpu-pow 16, gpn-token 19, marketplace 21, attestation 20, oracle 14, node 5, runtime 2). `build-spec` confirms mainnet = 0 endowed, dev = 6×100,000 GPN, reward = 50 GPN on all. Live `gpn-node --dev` mined/imported blocks #30–#34 with the new reward. Detail: `shared/artifacts/project7/cycle35-supply-cap-mainnet-log.md`

---

## 🚀 Previous Milestone (Cycle 34)

**Closed the three code-side Phase-4 open questions from the tokenomics spec in `pallet-gpn-token`** — the fee economy now has real deflation, a working oracle payout, and proof-of-staked-GPU incentives. This is the first code delivery toward mainnet tokenomics (not just docs).

1. **Real burn** — `distribute_fees` now `Currency::slash`es the burn share (default 20% of the 5% fee) from the pallet account, actually removing it from `total_issuance` (new `Burned` event). Was: counted in an event only.
2. **Oracle reward distribution** — new `distribute_oracle_rewards(operators)` drains `OracleRewardPool` and splits evenly (new `OracleRewardsPaid` event). Was: pool accumulated forever.
3. **Tier-multiplied staking payout** — `distribute_staking_rewards(stakers)` now pays each staker a share **weighted by attested GPU tier multiplier** (Bronze 1.0× / Silver 1.5× / Gold 2.5×; Unattested excluded), consuming the existing `GPUAttestationProvider` cross-pallet trait (same pattern as the marketplace). Attestation now directly boosts staking yield — the proof-of-staked-GPU incentive is real.
4. **Wiring** — `gpn-token` now depends on `pallet-gpu-attestation`; runtime sets `type GPUAttestation = GPUAttestation`.

**Verified:** `cargo test -p pallet-gpn-token` → **19 passed / 0 failed** (was 16; +3 new: burn-reduces-issuance, oracle split, tier weights). `cargo build -p gpn-node` (node + WASM runtime) clean. New extrinsics `distribute_staking_rewards(stakers)` / `distribute_oracle_rewards(operators)`; new events `Burned` / `OracleRewardsPaid`. Tokenomics spec §7 updated (3 of 5 open items now code-complete).

Detail: `shared/artifacts/project7/cycle34-tokenomics-phase4-log.md`

---

## 🚀 Previous Milestone (Cycle 33)

**Closed the last documentation gap: every `design/` subdir referenced by the README is now non-empty and source-of-truth-aligned to the implemented runtime.** Cycle 32 filled `design/tokenomics/`; this cycle filled the remaining three empty dirs with the same code-tracking approach.

1. **`design/pallets/00-pallet-overview.md`** — 5-pallet inventory (gpu-pow, gpu-attestation, compute-marketplace, profitability-oracle, gpn-token): roles, all extrinsics, storage, events, cross-pallet contracts (mint, `GPUAttestationProvider` gate, fee split), dependency graph, verification state.
2. **`design/protocols/01-gpu-pow-consensus.md`** — Ethash GPU-only PoW, block lifecycle & reward mint, difficulty adjustment, epochs, storage, agent integration (DAG release/rebuild on switch).
3. **`design/protocols/02-gpu-attestation.md`** — Proof-of-Staked-GPU flow, 1,000 GPN stake, GFLOPS measurement (Cycle 24 units fix), `GPUAttestationProvider` cross-pallet contract, open Phase-4 items.
4. **`design/protocols/03-compute-marketplace.md`** — two-party escrow job lifecycle (register→create→accept(gate)→start→submit→complete), pricing & 5% fee, min_gflops gate, storage.
5. **`design/protocols/04-profitability-oracle.md`** — on-chain `ProfitabilityFeed` (token price / GPU $/hr / utilization / APR), operator extrinsics, USD-units convention (Cycle 31 fix), agent consumption.
6. **`design/node-agent/01-agent-architecture.md`** — crate layout, Mode enum & auto-switch (5% hysteresis, min_epoch_hours), ChainClient/signer, config table, standalone tooling (incl. two-account demo), open items.
7. **README** — project-structure section updated: full 5-pallet list + new `design/` tree.

**Verification:** all new docs cross-checked against `gpn-node/runtime/src/lib.rs` (construct_runtime, pallet config) and pallet `lib.rs` (extrinsics/storage/events) and `gpn-agent` source. No code changed. Git tree clean after commit.

Detail: `shared/artifacts/project7/cycle33-design-docs-log.md`

---

## 🚀 Previous Milestone (Cycle 32)

**Filled the empty `design/tokenomics/` dir with a source-of-truth tokenomics spec aligned to the implemented `pallet-gpn-token` code** (fair-launch, 10%→1% inflation, 50 GPN/block, 5% fee split 200/150/100/50 bps, 1,000 GPN attestation stake); reconciled preliminary-paper vs implemented-code tokenomics; documented Phase-4 open questions. Commit `795686c`.

---

**The last integration gap is closed: the live on-chain profitability feed now drives the auto-switch and job claiming end-to-end.** Found & fixed a dimensional bug (`calc_compute_revenue` multiplied an already-USD GPU-hour price by `token_price` again → USD², capping compute revenue ~13× too low), which is why the chain feed had never triggered Compute and the autoclaim demo had to use the scripted feed.

1. **Fix:** extracted `calc_compute_revenue(gpu_price, utilization)` as a free function returning `gpu_price × utilization × 0.85` USD/hr (no token double-conversion); updated `analyze()`; +regression test.
2. **Verified live (RTX 4060, `gpn-node --dev`):** seeded feed `GPU=$12/hr, util=95%` → agent log `Compute=$9.6900/hr` (from live feed) → `🔄 Mining → Compute` (mining stopped, DAG released) → claimed job #1 → real 2048³ GPU GEMM → `✅ job #1 completed & paid out` → auto-attested `tier=Bronze` @ 195 GFLOPS.
3. **On-chain:** Alice `staked=true attested=true`; balance `999,000,000,047.499` GPN (= 1T − 1,000 stake + 47.5 payout). Two-account demo re-run green.
4. **New `config/chain_autoclaim.toml`** (`profitability_feed = "chain"`).

Detail: `shared/artifacts/project7/cycle31-chain-feed-autoswitch-live-log.md`

**The marketplace is now proven with a real multi-party escrow/payment path — a distinct paying customer (Bob) funds a job, and a distinct provider (Alice) is paid from escrow.** The old demo used one account for both sides; new `gpn_two_account_demo` exercises the true two-party flow.

1. **New `gpn_two_account_demo` binary** — requestor `//Bob` creates a paid job (escrow debited from Bob); provider `//Alice` registers (idempotent), accepts, starts, submits a deterministic result hash, completes → payout credited to Alice. No GPU needed (deterministic hash) so it runs on a plain CPU `gpn-node --dev`; still exercises the `min_gflops` attestation gate when set > 0.
2. **Verified live (this host):** job #4 Completed; on-chain `requestor=0x8eaf…` (Bob), `provider=0xd43593…` (Alice); Bob lost exactly 50 GPN escrow; Alice gained ~47.5 GPN (50 − 5% `MarketplaceFeeBps`). All 5 assertions pass.
3. **Fixed 2 demo bugs** — (a) passed signer URI to `get_account_info` (parses ss58 → 0 balance); now pass resolved `AccountId32` ss58. (b) address-assertion format mismatch (`get_job_state` returns `0x{hex}` vs ss58); now compare `0x{hex}` of resolved bytes.
4. **Regression:** marketplace 21 passed, agent lib 21 passed / 7 GPU-ignored, release build clean. No pallet/runtime changes.

Detail: `shared/artifacts/project7/cycle30-two-account-demo-live-log.md`

---

## 🚀 Previous Milestone (Cycle 29)

**The last integration gap is closed — `auto` mode now claims & serves real marketplace jobs on-chain when compute demand is high, and the `provider=0x00` display bug is fixed.**

1. **`ChainClient::list_claimable_jobs(attested_gflops)`** — enumerates `0..next_job_id`, keeps only `Pending` jobs this provider can satisfy (open market `min_gflops=0`, or `attested ≥ min_gflops`); `get_job_min_gflops` reader; pure `is_job_claimable` predicate (+3 tests).
2. **Agent `claim_and_serve_jobs()`** — idempotent provider registration, then per job: `accept_job → start_job → real GPU GEMM → submit_result → complete_job` (payout). Wired into the `auto` profitability tick: when current mode is `Compute`, it claims & serves instead of only running background compute.
3. **🐛 AccountId32 display fix** — subxt dynamic storage decodes an AccountId32 as a *doubly-nested* `Composite::Unnamed` of 32 byte-prims; old code read only the outer level → `provider=0x00`. New `collect_account_bytes` recursively descends composites/variants (+handles U256/U128). Verified live: `provider=0xd43593c715…` (Alice).
4. **New tooling** — `gpn_create_job` (requestor side, leaves job Pending), `config/autoclaim.toml` (auto + scripted demand-spike).

**Verified live (RTX 4060):** `gpn_create_job` → job #3 Pending → `gpn-agent auto` boot Mining → oracle spike → 🔄 Mining→Compute → `1 claimable job(s) found (attested 294 GFLOPS)` → accept/start/real GEMM/submit/complete → ✅ job #3 paid out → demand fades → 🔄 Compute→Mining.

Commit `abe86b1`. Detail: `shared/artifacts/project7/cycle29-autoswitch-claim-jobs-live-log.md`

---

## 🚀 Previous Milestone (Cycle 27)

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
