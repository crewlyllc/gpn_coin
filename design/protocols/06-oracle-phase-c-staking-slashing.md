# Phase C — Staking-Backed Slashing Oracle (Spec)

**Status:** Design spec (Cycle 52). **Not yet implemented.** This is the buildable
blueprint for oracle-governance §6 gap #5 — the permissionless, staking-backed
end-state of the `ProfitabilityOracle`. Phase B (committee admission, cycle 51) is
complete; this spec turns the Phase-C design in `05-oracle-governance.md` §4 into a
concrete pallet design that can be implemented next cycle with minimal sp-core risk.

**Companions:** `05-oracle-governance.md` (governance model, threat model, §6 gaps),
`04-profitability-oracle.md` (current protocol), `testnet-public-runbook.md` §9
(mainnet open items).

---

## 1. Goal

Replace the *committee-trusted* operator set (Phase B) with a **permissionless,
economically-secured** operator set:

- **Anyone** can become an oracle operator by **bonding GPN** (`OperatorBond`).
- Operator **weight in the effective feed = bond size** (bond-weighted median).
- A **stale or outlier** operator can be **slashed** (a portion of the bond is
  burned / routed to the requestor-compensation pool) via a **challenge** anyone
  can submit.
- **Sybil resistance** comes from the bond cost + slashing, mirroring the 1,000 GPN
  attestation-stake pattern already in `pallet-gpu-attestation`.

This is the "trustless" end-state and the biggest build. It is designed to be
**additive** to the existing pallet (Phase A/B behavior stays intact) and gated on
the Phase A/B economics being proven on testnet first (business decision D2).

---

## 2. Design constraints (learned from this project's history)

1. **NO new Substrate pallet crates unless unavoidable.** The project has a painful
   history of `sp-core`/`substrate-compat` version conflicts when wiring a *new*
   pallet crate into the runtime (see cycles 21/25). Phase C should therefore be
   implemented **inside the existing `pallet-profitability-oracle`** (like Phase B
   was), adding storage + extrinsics, NOT as a separate `pallet-oracle-slashing`.
2. **Use the existing `Currency` trait pattern** already proven in
   `pallet-gpn-token` (line 52: `Currency<AccountId>::Balance`; `slash` at line 210;
   `deposit_creating` at line 176; `transfer` + `ExistenceRequirement`). Phase C
   needs `T::Currency: Currency<Self::AccountId>` added to the oracle `Config`.
3. **Backward compatibility.** All existing storage/extrinsics/consumers
   (`CurrentFeed`, `OperatorFeeds`, `LastUpdateBlock`, `OracleOperators`,
   `Committee`, `PendingAdmissions`, `update_feed`, `propose_operator`,
   `approve_operator`, `remove_operator`) must keep working. Phase C is additive:
   a new `StakedOperators` map + new extrinsics; `update_feed` gains an *additional*
   authorization path (bonded), it does not remove the committee path.
4. **Bounded storage.** Use `BoundedVec` for any unbounded collection (e.g. a
   challenge's approval/evidence list). Cap the operator set via the existing
   `MaxOracleOperators`.

---

## 3. Storage additions (in `pallet-profitability-oracle`)

```rust
/// GPN bonded by an oracle operator (Phase C). Presence here = a permissionless
/// operator. Weight in feed aggregation = bond size.
#[pallet::storage]
pub type StakedOperators<T: Config> =
    StorageMap<_, Blake2_128Concat, T::AccountId, BalanceOf<T>, ValueQuery>;

/// Per-operator staleness: the last block each operator submitted a feed.
/// Used to detect stale operators for slashing (independent of the global
/// `LastUpdateBlock` which Phase A/B uses for the anti-spam throttle).
#[pallet::storage]
pub type OperatorLastFeedBlock<T: Config> =
    StorageMap<_, Blake2_128Concat, T::AccountId, BlockNumberFor<T>, OptionQuery>;

/// A pending slashing challenge against an operator (Phase C).
#[derive(Clone, Encode, Decode, PartialEq, Eq, Debug, TypeInfo, MaxEncodedLen)]
#[scale_info(skip_type_params(T))]
pub struct Challenge<T: Config> {
    /// Who submitted the challenge (must post `ChallengeBond`).
    pub challenger: T::AccountId,
    /// The operator being challenged.
    pub target: T::AccountId,
    /// Block at which the challenge was opened.
    pub opened_at: BlockNumberFor<T>,
    /// Bond posted by the challenger (returned if the challenge succeeds).
    pub bond: BalanceOf<T>,
}

/// Open challenges awaiting resolution (`StorageMap<AccountId /*target*/, Challenge<T>>`).
#[pallet::storage]
pub type OpenChallenges<T: Config> =
    StorageMap<_, Blake2_128Concat, T::AccountId, Challenge<T>, OptionQuery>;
```

**Config additions:**

```rust
// In Config trait:
/// Bond required to become a permissionless operator (Phase C).
#[pallet::constant]
type OperatorBond: Get<BalanceOf<Self>>;
/// Bond required to file a slashing challenge (Phase C).
#[pallet::constant]
type ChallengeBond: Get<BalanceOf<Self>>;
/// Deviation (in basis points) above which an operator's feed is "outlier" and slashable.
#[pallet::constant]
type OutlierThresholdBps: Get<u32>;
/// Blocks without a feed after which an operator is "stale" and slashable.
#[pallet::constant]
type StaleOperatorBlocks: Get<BlockNumberFor<Self>>;
/// Portion (in basis points) of the operator bond slashed on a successful challenge.
#[pallet::constant]
type SlashBps: Get<u32>;
/// The currency used to hold operator/challenge bonds.
type Currency: Currency<Self::AccountId>;
```

---

## 4. New extrinsics

### `bond_operator()`
- **Who:** any signed account.
- **Effect:** transfer `OperatorBond` from the caller into the pallet account;
  insert into `StakedOperators`. The account is now a permissionless operator and
  may call `update_feed`.
- **Guards:** `AlreadyOperator` if already bonded; `InsufficientBalance` if the
  caller can't fund the bond; `MaxOperatorsReached` if at `MaxOracleOperators`.
- **Idempotency:** a bonded operator can top up later (`bond_more`) or exit
  (`unbond_operator`).

### `bond_more(additional)`
- **Who:** an existing bonded operator.
- **Effect:** transfer `additional` into the pallet; increase `StakedOperators[who]`.
  Raises the operator's aggregation weight.

### `unbond_operator()`
- **Who:** a bonded operator.
- **Effect:** transfer the full bond back to the caller (reserved exit). Removes from
  `StakedOperators`. **Guard:** no `OpenChallenges` may be pending against the caller
  (prevents slashing evasion — D4 refinement).

### `update_feed(...)` — extended authorization
- **Existing:** committee-approved operators (Phase A/B) still authorized.
- **New:** any `StakedOperators` member authorized (Phase C).
- **New effect:** also set `OperatorLastFeedBlock[who] = now`. Everything else
  unchanged (writes `CurrentFeed` + `OperatorFeeds[who]`).

### `challenge_operator(target)`
- **Who:** any signed account, posting `ChallengeBond`.
- **Effect:** if `target` is a `StakedOperators` member and has no open challenge,
  open an `OpenChallenges[target]` (with the challenger's bond). The target is now
  under challenge and may NOT `unbond_operator` until resolved.
- **Guards:** `NotOperator` if target isn't bonded; `AlreadyChallenged` if one is open.

### `resolve_challenge(target, verdict)` — where the economics bite
Two paths:

- **`SlashStale`:** anyone may `resolve_challenge(target, SlashStale)` when
  `now - OperatorLastFeedBlock[target] > StaleOperatorBlocks`. Slashes `SlashBps` of
  the operator's bond (burn via `Currency::slash`, or route to the requestor pool —
  **decision D4**); returns the challenger's `ChallengeBond`; removes the challenge.
- **`SlashOutlier`:** anyone may `resolve_challenge(target, SlashOutlier)` when the
  *aggregate* median deviates from `target`'s own latest feed by > `OutlierThresholdBps`
  on any field. Same slash + bond-return + challenge-close.
- **`Dismiss`:** if the challenge is unfounded (target feed within threshold AND not
  stale), anyone may dismiss it: the challenger's `ChallengeBond` is **slashed** (to
  the same sink) as a frivolous-challenge deterrent; target's bond untouched.

> **Note on deviation verification:** computing the aggregate median on-chain requires
> reading every `OperatorFeeds` entry. For a bounded operator set (≤ `MaxOracleOperators`)
> this is fine. A challenge may specify the offending field to bound the check.

---

## 5. Bond-weighted effective feed (aggregation)

Expose a read helper `effective_feed()` (storage-level, not an extrinsic) that returns
the **bond-weighted median** across all `StakedOperators`:

1. Enumerate `OperatorFeeds` (each entry's `updated_at`).
2. Drop stale entries (`now - OperatorLastFeedBlock > StaleOperatorBlocks`).
3. For each field, compute the **bond-weighted median**: sort operators by field value,
   walk cumulative bond weight, take the value at the 50% cumulative-weight point.
4. Return that as the effective feed.

This replaces the client-side plain median (cycle 50) with an **on-chain**,
bond-weighted version for Phase C. The agent can opt in via a new
`feed_aggregation = "bonded_median"` config (additive; `latest`/`median` unchanged).

---

## 6. Unit-test plan (mirror existing pallet test conventions)

1. `bond_operator_transfers_bond_and_registers` — balance moved, `StakedOperators`
   set, `UpdateFeed` accepted by a non-committee bonded operator.
2. `bond_operator_insufficient_balance` — `InsufficientBalance`.
3. `bond_more_increases_weight` — aggregation weight reflects top-up.
4. `unbond_operator_returns_bond` — balance restored, removed from `StakedOperators`.
5. `unbond_blocked_while_challenged` — `OpenChallenges` guard.
6. `challenge_opens_with_bond` — `OpenChallenges[target]` stored, bond held.
7. `challenge_not_operator` / `already_challenged` guards.
8. `resolve_slash_stale` — operator stale past `StaleOperatorBlocks` → slashed,
   challenger bond returned, challenge closed.
9. `resolve_slash_outlier` — operator feed deviates > `OutlierThresholdBps` from
   median → slashed.
10. `resolve_dismiss_slashes_challenger` — unfounded challenge → challenger bond
    slashed, target untouched.
11. `effective_feed_bond_weighted_median` — a small-bond outlier cannot move the
    median; a large-bond operator dominates.
12. `update_feed_sets_operator_last_block` — staleness bookkeeping.
13. Backward-compat: committee path `update_feed` still works when no bonds exist.

---

## 7. Open decisions (escalate to Jeremy — D4, plus D2/D3 dependency)

- **D2 (phase target):** is committee-only acceptable for mainnet launch, or must
  Phase C be live first? (Gates whether we *implement* or *spec-only* this cycle's
  build.)
- **D3 (aggregation):** client-side median acceptable at launch, or must on-chain
  bond-weighted aggregation land first?
- **D4 (slashing sink & severity):** what `SlashBps` (e.g. 10% of bond)? Does the
  slashed bond **burn** (deflation, aligns with the 20% burn in `gpn-token`) or route
  to a requestor-compensation pool? Same for the frivolous-challenge bond.
- **D5 (testnet root):** confirm ops holds testnet Sudo; unrelated to Phase C but a
  precondition for testing Phase C economics live.

> **Recommendation for next cycle:** if D2/D3 allow it, implement Phase C **inside
> `pallet-profitability-oracle`** (per §2.1) with the 13-test plan above; otherwise
> keep this spec as the authoritative design and pursue docs/mainnet-prep instead.
