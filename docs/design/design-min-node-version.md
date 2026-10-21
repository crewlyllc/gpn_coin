# Design — minimum node version, as a consensus rule (STAGED proposal)

**Status: DESIGN ONLY. Nothing in the runtime is modified, nothing is built,
nothing is applied.** The node-side cutoff (layer 1) ships now; this document is
the on-chain half, which is review-first and chain-wide.

Related: `docs/updates.md` (the operator-facing view), `wasm/README-pending-upgrade.md`
(the staged-artifact pointer), `docs/GPN-MASTER-REFERENCE.md` §5 (how every
previous runtime change was staged and rolled out).

---

## 1. The problem with a web-served cutoff

Today the cutoff lives in the **signed release manifest**
(`https://gpncoin.ai/releases.json` → `min_version`). That is a good trade:
it is signed with the release key, it can be changed in seconds, and it needs no
chain interaction.

It is, however, not *consensus*:

* the value lives outside the chain, so two nodes can legitimately disagree about
  the cutoff while a manifest propagates (CDN staleness, an offline node);
* a node cannot prove to anyone *why* it stopped mining;
* GPN's release key becomes a de-facto consensus parameter setter — a role we
  would rather keep for signatures on binaries only.

## 2. Proposal: `MinNodeVersion` in the runtime

### 2.1 Where

`pallets/gpn-governance` — it is already the place where the committee decides
network-wide things, and it already owns the proposal → approve → execute flow
(`propose`/`approve`/`execute`). No new pallet, no new origin.

### 2.2 Storage

```rust
/// The oldest node build still allowed to mine and announce, as M.m.p.
/// `0.0.0` (the default) = no cutoff. Set only by a governance proposal whose
/// call is executed by the committee origin.
#[pallet::storage]
#[pallet::getter(fn min_node_version)]
pub type MinNodeVersion<T: Config> = StorageValue<_, NodeVersion, ValueQuery>;

/// Packed M.m.p so a comparison is one u32 comparison and the encoding is
/// stable: major*1_000_000 + minor*1_000 + patch.
#[derive(
    Clone, Copy, PartialEq, Eq, Encode, Decode, MaxEncodedLen,
    RuntimeDebug, TypeInfo, Default,
)]
pub struct NodeVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}
```

Why a packed triple rather than a string: a string is unbounded, unvalidated and
invites "0.10" vs "0.9" lexical bugs. Why `ValueQuery` with a `0.0.0` default:
an existing chain needs **no migration** — the storage item reads as "no cutoff"
until governance sets it, and a missing value can never brick mining.

### 2.3 Call

```rust
/// Set the minimum node version. Committee origin only (same origin as the
/// other governance calls — no new privilege).
#[pallet::call_index(3)]
#[pallet::weight(T::WeightInfo::set_min_node_version())]
pub fn set_min_node_version(
    origin: OriginFor<T>,
    version: NodeVersion,
) -> DispatchResult {
    T::CommitteeOrigin::ensure_origin(origin)?;   // or the existing Members gate
    let old = MinNodeVersion::<T>::get();
    // Monotonic in practice: never LOWER the cutoff outside a proposal that
    // also emits the reason, so a compromised member cannot quietly re-admit
    // an exploited build. Downgrades are allowed but must be explicit.
    MinNodeVersion::<T>::put(version);
    Self::deposit_event(Event::MinNodeVersionChanged { old, new: version });
    Ok(())
}
```

Notes:

* **Monotonicity is a policy, not an assertion.** A hard `>=` check would make a
  mistaken raise unrecoverable; instead the event (`old`, `new`) is the audit
  trail, and any lowering should be justified in the proposal text. If review
  prefers a hard rule, add `ensure!(version >= old)` — decide at review time.
* The call goes through the normal proposal path, so it lands **only** after
  committee approval + `execute`. No single key can move it.

### 2.4 Runtime API (so a node can read it)

```rust
// in sp_api::decl_runtime_apis!
pub trait NodeVersionApi {
    /// (major, minor, patch) — the on-chain cutoff, (0,0,0) = none.
    fn min_node_version() -> (u16, u16, u16);
}
```

A node reads it with one `state_call` at startup and after each manifest refresh.
It does **not** need a pallet-specific storage key or a metadata dependency,
which keeps the client usable across runtime versions.

### 2.5 What a node does with it

Merge with the manifest cutoff, take the **maximum**:

```text
required = max(manifest.min_version, chain.min_node_version)   # if both are readable
required = manifest.min_version                                 # chain unreachable / not yet on chain
```

Then the existing layer-1 behaviour applies unchanged: below `required` → log
`UPGRADE REQUIRED`, `--no-seed --no-discoverable`, no proposer, no miner, keep
syncing.

Why max(): the manifest is fresher (a release can raise the floor within
seconds), while the chain value is the one everybody agrees on. Taking the max
means a node errs toward the stricter of two signed, reviewable sources.

### 2.6 Failure modes, deliberately chosen

| Situation | Behaviour | Why |
|---|---|---|
| Chain unreachable at startup | use the cached/manifest value | a network hiccup must not stop a healthy node |
| On-chain value unreadable (old runtime) | ignore it, use the manifest | forward-compatible: new nodes tolerate pre-upgrade chains |
| Governance raises the floor by mistake | `set_min_node_version` back down (with a justified proposal) | recoverable, audited |
| Node below the floor but the operator uses `GPN_MIN_VERSION_ENFORCE=0` | mines, and is (in practice) isolated by the protocol-id bump | the escape hatch stays a local, unsupported choice |

## 3. Peer layer: when to bump the protocol id

Independent of any storage item, incompatible peers must not half-talk. Policy
(also in `docs/updates.md` §5.2):

| Change | Runtime-only (`set_code`)? | Bump protocol id? | Raise `min_version`? |
|---|---|---|---|
| Pallet constant / parameter / bug fix in an extrinsic | yes | no | no |
| New extrinsic / new storage item that old nodes can ignore | yes | no | usually no |
| Block or header format change | no (node + runtime) | **yes** | **yes** |
| p2p message set / handshake change | no | **yes** | **yes** |
| RPC the access point or provider loop depends on | no | no (not p2p) | **yes** (AP compatibility) |

Bumping the protocol id is the *enforcement* of the cutoff for nodes that ignore
it: an incompatible node finds no peers, cannot fetch blocks, and cannot
propagate a block it sealed — the safe failure mode. It must ship **in the same
release** that raises `min_version`, never earlier (that would cut off nodes
before they were told to upgrade).

Implementation point for review: the protocol id lives with the network
configuration in `gpn-node/node/src/service.rs` (the `sc_network` config). The
bump should be a named constant (`GPN_PROTOCOL_ID`) with the release version in
its value, so "did we bump it?" is a one-line diff.

## 4. Rollout (when approved — none of this is done)

1. Review this design; decide the monotonicity question (§2.3) and the weight.
2. Implement in `pallets/gpn-governance` + the runtime API; **new**
   `spec_version` (42), and pallet unit tests for: default = no cutoff,
   committee-only set, event carries old/new, lowering is allowed/explicit,
   `MinNodeVersion` read through the API.
3. Build a **separate** WASM artifact (never regenerate the canonical
   `gpn_runtime.compact.compressed.wasm` — the genesis WASM is immutable):
   `wasm/gpn_runtime_spec42_min_node_version.compact.compressed.wasm`.
4. Stage it and record the pointer in `wasm/README-pending-upgrade.md`.
5. Committee proposal → approve → `execute` → `System.set_code`. Watch one
   session of blocks for `spec_version` and the event.
6. Only then may `releases.json`'s `min_version` be raised beyond what the
   on-chain value will say — and the next node release reads the chain value.

Steps 1–4 are what "staged" means in this repo: the change exists as a
reviewable design plus an artifact, and **nothing on chain changes until a human
with the committee keys says so.**

## 5. Why not put `min_version` in genesis?

Because it would be a second, frozen copy of a value that has to move: genesis is
immutable here (a good thing), and a genesis value cannot be raised when a
security fix ships. `0.0.0`-means-no-cutoff keeps the genesis diff empty, which
is also why this proposal needs **no migration**.
