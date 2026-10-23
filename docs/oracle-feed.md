# Profitability oracle feed — how GPN prices are set (item #68)

The scheduler turns a USD rate into a GPN charge with `gpn/tok = usd/tok ÷ token_price_usd`
and (under `GPN_PRICING_MODEL=market`) anchors the GPU-hour price on
`ProfitabilityOracle::avg_gpu_hour_price`. Both come from the on-chain feed
`ProfitabilityOracle::CurrentFeed`.

**Status: posted and maintained since 2026-10-22** (block 176294) by
`gpn-oracle.service`, a bonded Phase-C operator. Before that the feed had **never been
posted** and every consumer silently used the compiled placeholders
(`token $0.15`, `market $3.00/hr`, `util 65%`).

## Why it had never been posted (root cause, 2026-10-22)

Three independent reasons, all fixed:

1. **The authorized operators were unfunded.** Genesis admits 5 committee operators
   (`OracleOperators`, from Jeremy's `oracle-roster`). All five held **0 GPN**, so every
   `update_feed` would fail on fee payment. Nothing funded them, and nothing noticed.
2. **No bonded operator existed.** `ProfitabilityOracle::effective_feed()` — the
   bond-weighted median that `feed_aggregation="bonded_median"` consumes — only reads
   `StakedOperators`, i.e. Phase-C bonded operators. `StakedOperators` was **empty**, so
   even a committee post would not have produced an effective feed.
3. **No service existed.** `gpn_seed_feed` is a one-shot bootstrap; the pallet only
   accepts a new feed per operator every `OracleUpdateInterval` (600 blocks ≈ 2.5 h at
   15 s), so an unmaintained feed goes stale. Nobody ran the loop.

And the consumer made all of this invisible: the scheduler's feed refresher fell back to
the compiled defaults **silently**. It now logs one line on every transition:

```
[pricing] feed source: ON-CHAIN ProfitabilityOracle::CurrentFeed (token $0.1500, market $0.3000/gpu-hr, util 65.0%)
[pricing] feed source: FALLBACK defaults (…) — no on-chain oracle feed; run the operator loop …
```

## What is posted, and where each number comes from

`oracle_values.toml` (repo root) is the operator's input file:

| field | value | provenance |
|---|---|---|
| `token_price_usd` | `150000` ($0.15) | **Policy anchor**, owner decision D1 (2026-09-15). GPN is not listed on any exchange, so there is no market price to observe. Changes only on an owner decision. |
| `avg_gpu_hour_price` | `300000` ($0.30/hr) | **Market observation.** Oct-2026 RTX 4090 rental band: floor $0.20, median $0.45 (daily scrape), Vast.ai $0.29–0.59. The network's cards are 8–16 GB consumer parts, i.e. below a 4090, so the lower band is the honest anchor. The compiled placeholder was $3.00/hr — ~10× the market — which made a 1B model look 22× gpt-5-nano instead of ~2×. |
| `utilization_rate_bps` | `6500` (65%) | **Not a measurement — do not read it as network utilisation.** The pricing formula uses this field as the steady-state efficiency divisor (`usd/tok = rental / (tok_s × 3600 × util)`). Feeding a *measured* 0–5% network utilisation here would multiply every price ~30×. See "Known gaps". |
| `staking_apr_bps` | `0` | **Measured.** Staking rewards are fee-funded (`distribute_fees`: 40% of a 5% fee) and both `GPNToken::StakingRewardPool` and `OracleRewardPool` read 0 on chain, so the realized APR is 0. The old `1500` (15%) was aspirational. |

Posting these reproduces the *prices* the network was already charging via the fallback
(`token $0.15`, `util 65%` are identical), so nothing repriced silently — what changed is
that the numbers are now on chain, attributable to an operator, and updatable without a
release. `market $0.30/hr` only affects the legacy `GPN_PRICING_MODEL=market` path and
image pricing, not the default throughput model.

## Operator runbook

```bash
# 1. fund the operator (fees) + bond it (Phase C, permissionless, 1000 GPN)
~/.local/bin/gpn_wallet send ws://127.0.0.1:9944 //Alice <OPERATOR_SS58> 1200

# 2. first post (also bonds if --bond is passed)
~/.local/bin/gpn_operate_oracle ws://127.0.0.1:9944 oracle_values.toml 1800 <SEED_HEX> --bond

# 3. keep it fresh (installed unit; seed lives in ~/.gpn/oracle.env, mode 600)
systemctl --user enable --now gpn-oracle.service
```

Verify:

```bash
~/.local/bin/gpn_effective_feed      # bond-weighted median (what bonded_median consumes)
~/.local/bin/gpn_aggregate_feed      # every operator's latest post + plain/bonded median
journalctl --user -u gpn-accesspoint | grep 'feed source'
```

`UpdateTooSoon` in the operator log is **expected** between posts: each operator may post
once per `OracleUpdateInterval` (600 blocks). The loop logs it and waits.

## Known gaps (not fixed here — each needs its own decision)

- **`utilization_rate_bps` is overloaded.** The pallet documents it as "network GPU
  utilisation"; the pricing code uses it as an efficiency divisor. A real measurement
  (busy GPU-seconds ÷ available GPU-seconds over a window) is ~0–5% today and would blow
  prices up ~30× if posted. Either add a separate `efficiency_bps` to the feed (runtime
  change) or rename the field's meaning. **Do not "fix" this by posting a measured
  utilisation into the current field.**
- **`effective_feed()` ignores committee operators and staleness.** It aggregates only
  `StakedOperators`, and despite its doc comment it does **not** drop stale entries — a
  bonded operator that stops posting keeps its weight forever. Today: one bonded operator,
  so the median is that operator's post; if it dies the feed freezes at the last value
  (better than the placeholder, still not live). A staleness filter + a second bonded
  operator (different bond size, so the median is genuinely weighted) is the next step.
- **APR has nothing to measure.** Until fee revenue exists, 0 is correct but useless as a
  signal; revisit when the marketplace has volume.
