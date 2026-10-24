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

## Roster key custody audit (item #88, 2026-10-23)

Question: *before mainnet, does every authorised feed writer have a key we hold **and** a
balance to pay fees?* Answer on this chain: **no — 1 of 5 genesis roster keys qualifies.**
Measured on chain at head ~180,116 (audit re-verified after a real post at 180,141):

| role | address | key on this host | balance | posts feeds? |
|---|---|---|---|---|
| oracle-1 | `5DUaYAf5…M5Y6` | **yes** (`~/.gpn/oracle.env`, `ORACLE_SIGNER`) | 199.99 GPN | **yes** — bonded 1000 GPN, the only `StakedOperators` entry, i.e. the sole `effective_feed()` contributor |
| oracle-2 | `5Dk6Yv3B…RtwNtV` | no | **0.0000 GPN** | no — cannot pay fees |
| oracle-3 | `5F1eQwdy…MwKwD9` | no | **0.0000 GPN** | no |
| oracle-4 | `5DV1GPah…enSDUa` | no | **0.0000 GPN** | no |
| oracle-5 | `5Cvguz6w…JGASbm` | no | **0.0000 GPN** | no |
| committee-1 | `5HTWBtWr…3PJRteWN` | yes | 99.99 GPN | n/a (admission only) |
| committee-2 | `5Cd5NiY2…uEX3Dx` | yes | 99.99 GPN | n/a |
| committee-3 | `5HeUVTK6…j5h` | yes | **100.00 GPN** (funded this run — was 0) | n/a |

`OracleOperators` = 5 (genesis `testnet_oracle_operators()`, Jeremy's `oracle-roster`
2026-09-16; `OperatorCount = 5`, `MaxOracleOperators = 10`). `Committee` = 3, all held
here, `AdmissionThreshold = 2`. `committee-4/5` from the 2026-10-21 key file are **not**
committee members on this chain (they are mainnet additions).

**Why the four dead entries are not an immediate emergency, and why they still matter:**
`effective_feed()` aggregates **only** `StakedOperators`, so a non-bonded roster account
contributes nothing to pricing — the feed is not being polluted by unreachable keys. What
they do is make the roster *look* redundant when it is not: the network's price feed
currently depends on **one** key in `~/.gpn/oracle.env`. Lose that file (or this host) and
the feed freezes at the last posted value; `StaleOperatorBlocks = 2112` (~9 h at 15 s)
after the last post the bonded operator also becomes slashable (10 % of the 1000 GPN bond).

**Fee runway:** one post per operator per `OracleUpdateInterval` (600 blocks). At ~2.7 h
per post, 199.99 GPN is thousands of posts of runway — fees are not the constraint.

**Remediation done this run (no runtime/genesis/`set_code`):** funded `committee-3`
(0 → 100 GPN) so all three admission-committee keys can pay fees for the only path that
can replace a lost roster key (`propose_operator`/`approve_operator`/`remove_operator`),
and proved a real end-to-end write: `update_feed` from oracle-1 landed at block **180141**
(`LastUpdateBlock` 179539 → 180141, `OperatorFeeds[oracle-1].updated_at` likewise,
`gpn_effective_feed` reads `updated at block = 180141`, `gpn_check_feed` still plausible).

**Open decision for Jeremy (owner-gated — key custody, cannot be resolved from here):**
1. **Import** the four cold `oracle-roster` seeds onto a host that runs a feed loop (or run
them from the cold machine), or
2. **Retire** them with `remove_operator` from a committee key (we hold 2-of-3) so the
   roster states the truth, and instead add a **second bonded operator** with its own
   `gpn-oracle` instance — different bond size so the median is genuinely weighted.

Option 2 plus a second service is the only arrangement that survives losing any single key,
which is what "the feed must not go stale before mainnet" actually requires. **Do not bond
a second operator without a service to keep it fresh:** an unmaintained bond is a slash
waiting to happen.

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

## Operator loop: RPC reconnection (item #105)

The feed loop (`gpn_operate_oracle`, unit `gpn-oracle`) holds one RPC client for the
life of the process. A dropped socket (`The background task closed connection closed;
restart required`) used to freeze the feed silently — 64 consecutive failures, 3 040
blocks stale — so the loop now heals itself: it logs `♻️ RPC socket lost … reconnecting`,
rebuilds the client, re-reads the nonce and resubmits **once**. A chain-level answer
(`UpdateTooSoon`, `NotAuthorized`) is never retried, and if the node is down the loop
keeps its cadence instead of exiting. Watch for the `♻️` line: repeated heals mean the
RPC is unstable, not that the feed is broken.
