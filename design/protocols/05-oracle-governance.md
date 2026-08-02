# Profitability Oracle — Governance Spec (runbook §9 #5)

**Status:** Draft proposal for Jeremy's review — *not yet enforced.* Defines who may
run the `ProfitabilityOracle`, what data sources are trusted, and how a bad feed is
held accountable. It is **incrementally enforceable with the pallets we already have** —
Phase A needs **zero** runtime changes; later phases are flagged as new-pallet work.

**Companions:** `04-profitability-oracle.md` (protocol), `testnet-public-runbook.md` §9
(mainnet open items), `marketplace-sla-reputation-proposal.md` (provider/requestor
accountability). This doc is the *operator-side* accountability half.

---

## 1. Why the oracle is the linchpin

The agent's **auto-switch** reads `ProfitabilityOracle::CurrentFeed`
(`token_price_usd`, `avg_gpu_hour_price`, `utilization_rate_bps`, `staking_apr_bps`)
to decide, per GPU-hour, whether to **mine**, **stake**, or **serve AI compute**
(cycle 18/20/31). Whoever controls the feed therefore controls where every node's GPU
is pointed. A manipulated feed can:

- **Inflate compute revenue** → nodes flood the marketplace, undercut real prices,
  and waste GPU-hours on jobs nobody priced correctly.
- **Deflate mining** → nodes abandon PoW, dropping network security.
- **Shift staking APR** → distort the staking pool payout.
- **Front-run the marketplace** — an operator that sees an incoming `update_feed`
  can position a job before the price moves.

So feed integrity is **economic security**, not just data plumbing. The governance
model must answer: *who writes the feed, from what sources, and what happens if they lie.*

---

## 2. Current on-chain surface (verified, `pallet-profitability-oracle`)

| Call | Who may call | Guards |
|------|--------------|--------|
| `update_feed(token_price, gpu_price, utilization_bps, apr_bps)` | any signed account **in `OracleOperators`** | `NotAuthorized` if not an operator; `UpdateTooSoon` if < `OracleUpdateInterval` (600 blocks) since last; `InvalidPrice` if `token_price_usd == 0` |
| `add_operator(account)` | **root only** (Sudo today) | `AlreadyOperator`; `MaxOperatorsReached` (`MaxOracleOperators` cap) |
| `remove_operator(account)` | **root only** (Sudo today) | `NotOperator` |

Storage: `CurrentFeed` (latest feed + `updated_at` block), `LastUpdateBlock`,
`OracleOperators` (set), `OperatorCount`.

Key properties already in place:
- **Signed writes** — an operator must sign `update_feed`; no anonymous writes.
- **Anti-spam** — the 600-block `UpdateTooSoon` guard bounds write frequency
  (≈ one update per ~2.5h at 15s blocks) and smooths the feed.
- **Boundary check** — `token_price_usd > 0` rejects a zeroed token price.
- **Bounded operator set** — `MaxOracleOperators` prevents unbounded growth.

**Verified this cycle (cycle 47):** the **mainnet** genesis seeds **zero** oracle
operators and the mainnet Sudo key is the **all-zeros inert account** (ss58
`5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM` = `AccountId::new([0u8;32])`,
no endowed balance). So on mainnet **no operator can be added and no feed can be
written** until a governance path exists — the oracle is *locked by default*, which is
the safe posture. (Runbook §9 #1 "non-functional" half: **verified**.)

---

## 3. Threat model

1. **Colluding operators** — 2+ operators coordinate to write a fake feed.
2. **Single-point capture** — if `add_operator` is controlled by one root key that is
   compromised, the whole set is replaced.
3. **Stale feed** — operators stop updating; `CurrentFeed` goes stale and auto-switch
   bases decisions on outdated prices (the agent currently has no staleness TTL —
   gap, see §6).
4. **Outlier feed** — one operator posts an extreme value that the consumer trusts
   wholesale (no aggregation today).
5. **Sybil operators** — with free `add_operator`, an attacker floods the set.

---

## 4. Governance model — three phases

### Phase A — Root-managed operator set (current, testnet-ready, ZERO runtime change)

- Operator admission/removal is **root-only** (Sudo). On **testnet** the ops key is
  controlled by the operator team (functional by design: faucet + emergency feed
  seeding). On **mainnet** root is **inert** (§2), so the set stays empty — safe.
- **Trust model:** a small, named set of independent operators (target 3–5,
  `MaxOracleOperators`), each running its own data feed (exchange price API +
  GPU-rental marketplace scan + utilization estimator) and signing `update_feed`.
- **Feed consumer rule (recommend, docs/agent):** take the **median** of recent
  operator feeds, not the last writer — so one outlier cannot move the price.
  (Median across operators is client-side today; on-chain aggregation is Phase C.)
- **Admission bar (docs, not enforced):** an operator must run a public node, publish
  its data-source methodology, and commit to a minimum update cadence.
- **Exit:** root removes a non-performing/malicious operator; operators can be
  replaced without touching the runtime.

**Why Phase A first:** it needs no new pallet, matches the already-built
`gpn_operate_oracle` loop (cycle 41), and lets us run a real multi-operator testnet
to measure feed behavior before committing to on-chain governance.

### Phase B — Committee / multi-sig admission (small runtime addition)

- Replace **root-only** `add_operator`/`remove_operator` with a **named committee**
  (e.g. a fixed multisig or a `pallet-collective` council) that votes on operator
  changes. Removes the single-root capture point without going fully permissionless.
- Implementation: add a council pallet; `add_operator` origin becomes
  `EnsureOrigin` = council majority (or a fixed `MultiSig` account).
- **When:** before real mainnet operator onboarding, so no one key can hijack the set.

### Phase C — Permissionless, staking-backed operators with slashing (new pallet)

- Anyone can become an operator by **bonding GPN** (e.g. `OperatorBond`). Operator
  weight in feed aggregation ∝ bond.
- **Feed aggregation on-chain:** each operator submits to `CurrentFeed`; the pallet
  stores per-operator submissions and exposes the **bond-weighted median** (or
  trimmed mean) as the effective feed. `LastUpdateBlock` staleness per operator.
- **Slashing:** if an operator's submission deviates from the aggregate by > a
  threshold, or the operator goes stale past `StaleOperatorBlocks`, a **challenge**
  (anyone can submit) slashes a portion of the bond into the treasury/burn.
- **Sybil resistance:** bond cost + slashing makes fake-operator flooding
  economically irrational (mirrors the 1,000 GPN attestation-stake pattern).
- **This is the "trustless" end-state** and the biggest build; defer until Phase A/B
  prove the operator economics on testnet.

---

## 5. Trusted data sources (per-operator methodology, docs)

For each feed field, operators SHOULD source from independent public data:

| Field | Recommended source(s) | Unit / encoding |
|-------|----------------------|-----------------|
| `token_price_usd` | CEX/DEX price API (aggregate ≥2 venues); must be > 0 | micro-USD (u64) |
| `avg_gpu_hour_price` | GPU-rental market scan (io.net/Akash/Vast-style) for the target SKU (e.g. RTX 4060 class) | micro-USD/hr |
| `utilization_rate_bps` | Marketplace job fill-rate + external demand signal | basis points (0–10_000) |
| `staking_apr_bps` | Staking pool payout rate / staked supply | basis points |

**Integrity conventions:**
- **No self-dealing:** an operator must not create/accept marketplace jobs whose
  price it also set in the feed.
- **Reasonableness bounds (agent-side today, pallet in Phase C):** reject feeds where
  `gpu_price` or `utilization` imply an implausible `Compute` revenue (e.g. > some
  multiple of the mining revenue floor) — the cycle-31 USD² bug taught us the agent
  must sanity-check units, not blindly trust.
- **Public methodology:** each operator publishes its source list + update cadence in
  the onboarding doc so consumers can judge independence.

---

## 6. Gaps to close (code, ordered)

1. **Agent staleness TTL** — if `CurrentFeed`'s `updated_at` is older than N blocks,
   the agent should fall back to a conservative default (mine) rather than act on a
   stale feed. *Small agent change; high safety value.*
2. **Feed reasonableness bounds in the agent** (`calc_compute_revenue` guard) — reject
   implausible USD/hr so a single bad feed can't force a mass switch.
3. **Median aggregation client-side** (Phase A): poll all operators (or a public
   aggregator), take the median. *Tooling, no runtime.* — **DONE (cycle 50):**
   added a per-operator `OperatorFeeds` storage map to `pallet-profitability-oracle`
   (populated on every `update_feed`; `CurrentFeed` unchanged as the effective/latest
   value), agent `get_operator_feeds()` reader, pure `median_f64` /
   `aggregate_median_feed` (+ unit tests proving one outlier operator cannot move a
   field), a `feed_aggregation = "latest" | "median"` agent config, a
   `gpn_aggregate_feed` tool (prints every operator's feed + the median), and a
   `median` branch in `analyze()` (falls back to `CurrentFeed` if no operator has
   posted). Verified live on the RTX 4060 dev node: 1-operator read of `OperatorFeeds`
   + backward-compatible `CurrentFeed`; 2-operator median via a Bob seed after the
   600-block interval.
   **Design finding:** the `UpdateTooSoon` throttle uses a **single global**
   `LastUpdateBlock`, so only one operator can post per 600-block interval — the
   per-operator map accumulates over time but writes are serialized. Fine for a
   small named set on rotation; if Phase A wants concurrent independent operators,
   make the throttle per-operator (Phase B/C consideration).
4. **Phase B council/multisig admission** — runtime work, gated on Phase A testnet.
5. **Phase C staking-backed slashing oracle** — new pallet, gated on Phase B.

---

## 7. Open decisions (escalate to Jeremy)

- **D1 — Operator set size & identity on mainnet:** 3–5 named entities vs. open set?
  Who qualifies (independent infra, public methodology)?
- **D2 — Phase target:** is a root-managed/committee model acceptable for mainnet
  launch, or must the oracle be permissionless (Phase C) before go-live?
- **D3 — Aggregation:** median of operator feeds (client-side) acceptable at launch,
  or must aggregation be on-chain (Phase C) first?
- **D4 — Slashing severity:** for a bad/stale feed in Phase C, what bond % is slashed,
  and does it burn or route to the treasury/requestor-compensation pool?
- **D5 — Who holds testnet root:** confirm the ops team controls the testnet Sudo key
  (faucet + emergency feed) and that it is NOT reused anywhere on mainnet.

These are business/governance yes/no decisions → escalate to Jeremy; the Phase-A
operator runbook and the §6 code gaps (#1–#3) are safe to proceed with regardless.
