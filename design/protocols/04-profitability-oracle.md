# Profitability Oracle Protocol — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches `pallet_profitability_oracle` (Cycle 32 audit)
**Date:** 2026-08-01
**Author:** PM07

> The on-chain feed that tells each GPU agent what each mode earns right now, so the agent
> can auto-switch between Mining / Staking / Compute. Source of truth =
> `gpn-node/pallets/profitability-oracle/src/lib.rs`.

---

## 1. Role

The oracle publishes a `ProfitabilityFeed` on-chain. Every `gpn-agent` reads it and runs
its local `ProfitabilityCalculator` to convert Mining / Staking / Compute to USD/hr, then
`decide_switch()` picks the highest-revenue mode. The agent is the decision-maker
(agent-based auto-switch); the chain feed is the trusted input.

## 2. Feed Content (`ProfitabilityFeed`)

| Field | Meaning |
|-------|---------|
| `token_price` | GPN price in USD |
| `gpu_price` | Market GPU-hour price in **USD/hr** (NOT token) |
| `utilization` | Expected GPU utilization (0–1) for compute demand |
| `staking_apr` | Staking APR for the staking-revenue leg |
| (timestamps) | Update/validity metadata |

> **Units warning (Cycle 31 fix):** `gpu_price` is already USD/hr. Compute revenue =
> `gpu_price × utilization × 0.85`. Do **not** multiply by `token_price` again — that
> `USD²` bug deflated compute revenue ~13× and hid the chain-feed path.

## 3. Extrinsics

| Call | Actor | Effect |
|------|-------|--------|
| `update_feed(feed)` | operator | Publish a new `ProfitabilityFeed`; updates `CurrentFeed` + `LastUpdateBlock` |
| `add_operator(who)` | root | Grant operator role |
| `remove_operator(who)` | root | Revoke operator role |

Only `OracleOperators` may `update_feed`.

## 4. Storage (source of truth)

`CurrentFeed`, `LastUpdateBlock`, `OracleOperators`, `OperatorCount`.

## 5. Agent Consumption

- `config/profitability_feed = "chain"` selects the chain feed (default in newer configs);
  `"scripted"` is used for offline/testing.
- `ProfitabilityCalculator::analyze()` reads `CurrentFeed` via `ChainClient`, computes:
  - `calc_mining_revenue(hashrate, difficulty, token_price)`
  - `calc_staking_revenue(staked_amount, apr, token_price)`
  - `calc_compute_revenue(gpu_price, utilization)` = `gpu_price × utilization × 0.85`
- `decide_switch()` with **5% hysteresis** (`hysteresis_percent`) prevents flapping.

## 6. Live Verification (Cycle 31)

- Seeded feed `GPU=$12/hr, util=95%` on-chain → agent log `Compute=$9.6900/hr` (from live
  feed) → `🔄 Mining → Compute` (mining stopped, DAG released) → claimed job #1 → real
  2048³ GPU GEMM → `✅ job #1 completed & paid out` → auto-attested `tier=Bronze` @
  195 GFLOPS. Then demand faded → `🔄 Compute → Mining`.

## 7. Open Items (Phase 4)

- Oracle operator reward distribution: `gpn-token` accumulates `OracleRewardPool` (10% of
  fees) but no payout call exists yet — wire operators to be paid from it.
- Oracle decentralization / aggregation (currently a simple operator-set feed, not
  multi-source median).
- Feed staleness slashing.

## 8. Related

- Agent decision logic: `design/node-agent/01-agent-architecture.md`
- Fee split / oracle pool: `design/tokenomics/01-gpn-tokenomics.md`
- Implementation: `gpn-node/pallets/profitability-oracle/src/lib.rs`
