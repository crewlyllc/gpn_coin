# Design (STAGED — not implemented, not applied): hash-addressed governance proposals

**Status:** design only. **Nothing in this document is in the runtime.** No patch is applied, no
artifact is built, and `gpn_gov_upgrade --mode preimage` refuses to run until this ships as a
reviewed spec-42 upgrade in its own right.

## 1. Problem

`pallet-gpn-governance::propose(origin, call: Box<T::RuntimeCall>)` stores the whole call inline:

```rust
pub struct Proposal<T: Config> {
    pub call: T::RuntimeCall,                       // ≤ ~500 KB for set_code
    pub approvals: BoundedVec<T::AccountId, T::MaxMembers>,
    pub created_at: BlockNumberFor<T>,
}
pub type Proposals<T> = StorageMap<_, Blake2_256, T::Hash, Proposal<T>>;  // OptionQuery
```

For a runtime upgrade this means:

1. the **full WASM is visible in the `propose` extrinsic** (a public call carrying a raw WASM —
   exactly what `docs/upgrades-governance.md` §3 invariant 1 forbids);
2. every committee member must download and re-hash the same 500 KB blob to know what they are
   approving (no way to approve "the bytes with hash `0x…`" and have the chain enforce it);
3. ~500 KB of state per proposal, forever (there is no expiry and no cancel);
4. the `propose` extrinsic is ~500 KB — close enough to the 5 MB block limit that a larger
   runtime, or a smaller `MaximumBlockLength`, makes governance unusable.

Standard Substrate solves this with `pallet-preimage`: content is uploaded once (`note_preimage`,
paying the deposit), then referenced by hash, and enactment resolves the hash back to the bytes.

## 2. Shape of the change

Add **`pallet-preimage`** (and, for scheduled enactment, nothing else — the timelock already
exists in `pallet-gpn-governance`), and replace the inline call with polkadot-sdk's own
`Bounded<Call, Hashing>` wrapper, which is what `pallet-scheduler` / `pallet-democracy` use for
exactly this reason (`frame_support::traits::Bounded`, `preimages.rs`):

```rust
pub struct Proposal<T: Config> {
    /// `Bounded::Inline(call)` behaves like today; `Bounded::Lookup { hash, len }`
    /// carries only the hash and resolves through `pallet-preimage` at execution.
    pub call: Bounded<T::RuntimeCall, T::Hashing>,
    pub approvals: BoundedVec<T::AccountId, T::MaxMembers>,
    pub created_at: BlockNumberFor<T>,
}
```

`Bounded` already gives us `hash()`, `len()`, `lookup_needed()` and, via `QueryPreimage`,
`have()` / `peek()` / `realize()`. There is no need to invent a `ProposalKind` enum, and existing
`Inline` proposals keep working unchanged.

### 2.1 Why not `pallet-scheduler` + `pallet-democracy`

GPN has no referenda and no second chamber, and `pallet-scheduler`'s `Scheduled` storage also
holds the call inline — it does not remove the raw-WASM problem, it only moves it. Adding
`pallet-democracy` would change the governance model (stake-weighted votes), which is a
tokenomics decision, not an upgrade-mechanics one. **Keep the 2-of-3 committee + timelock** and
add preimage only.

## 3. Exact wiring

### 3.1 Runtime deps (`gpn-node/runtime/Cargo.toml`)

```toml
pallet-preimage = { workspace = true, default-features = false }
# std feature: "pallet-preimage/std"
# runtime-benchmarks: "pallet-preimage/runtime-benchmarks"
```

### 3.2 `construct_runtime!`

```rust
Preimage: pallet_preimage,
```

with

```rust
parameter_types! {
    /// Base deposit per byte for a noted preimage (2× the byte-length fee is the
    /// standard shape: base + per-byte, refunded on `unnote`/`unrequest`).
    pub const PreimageBaseDeposit: Balance = 5_000_000_000_000;      // 5 GPN
    pub const PreimageByteDeposit: Balance = 1_000_000;              // 0.000001 GPN/byte
    pub const PreimageHoldReason: RuntimeHoldReason =
        RuntimeHoldReason::Preimage(pallet_preimage::HoldReason::Preimage);
}

impl pallet_preimage::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type WeightInfo = pallet_preimage::weights::SubstrateWeight<Runtime>;
    type Currency = Balances;
    type ManagerOrigin = EnsureRoot<AccountId>;
    type Consideration = HoldConsideration<AccountId, Balances, PreimageHoldReason>;
}
```

A 500 KB artifact then costs ≈ 5 GPN + 0.5 GPN ≈ 5.5 GPN held (returned when the preimage is
used or unrequested). At mainnet prices that is negligible, and it stops free state-spam.

### 3.3 `pallet-gpn-governance` changes

```rust
pub trait Config: frame_system::Config {
    // ... existing ...
    /// Preimage store backing hash-addressed proposals (`pallet-preimage`).
    type Preimage: frame_support::traits::QueryPreimage<H = Self::Hashing>
        + frame_support::traits::StorePreimage;
}
```

`propose_preimage(origin, hash, len)` (`#[pallet::call_index(3)]`):

- `ensure_signed` + `Members` check (identical to `propose`);
- `ensure!(len <= MaxProposalCallLen::get())` — a new constant (e.g. 4 MiB, matching
  `pallet_preimage::MAX_SIZE = 4 * 1024 * 1024`) so a proposal can never exceed
  `MaximumBlockLength`;
- **do not** verify the preimage exists at propose time (it may be noted later); `execute` is
  where it must exist;
- store `Bounded::Lookup { hash, len }` — a 36-byte payload; the proposal id stays
  `blake2_256(SCALE(Proposal))`, so it is still content-addressed and two different preimage
  hashes are two different proposals.

`execute`:

```rust
let proposal = Proposals::<T>::get(proposal_hash).ok_or(Error::<T>::ProposalNotFound)?;
// ... approvals + timelock checks (unchanged) ...
if !T::Preimage::have(&proposal.call) {
    // NOT consumed: the committee can note the preimage (same hash) and retry.
    return Err(Error::<T>::PreimageMissing.into());
}
let (call, _len) = T::Preimage::peek::<T::RuntimeCall>(&proposal.call)?;  // fetch + decode
Proposals::<T>::remove(proposal_hash);                                    // only now
T::Preimage::realize::<T::RuntimeCall>(&proposal.call)?;                  // release the preimage/deposit
let res = call.dispatch(frame_system::RawOrigin::<T::AccountId>::Root.into());
res.map_err(|_| Error::<T>::ExecutionFailed)?;
```

`QueryPreimage::peek` already fetches, length-checks and SCALE-decodes (`DispatchError::Corruption`
otherwise), and `realize` releases the preimage and returns the depositor's consideration. Note
the ordering: `PreimageMissing` must be returned **before** `Proposals::remove` (today's code
removes first, which would silently burn a proposal whose preimage is missing).

### 3.4 Migration

`Proposal<T>` changes shape → the storage map needs a migration at enactment. Existing mainnet
proposals must be preserved (a pending upgrade must not be silently dropped):

```rust
// spec 42 -> 43 migration (in the upgrade block)
frame_support::storage::migration::translate_values::<OldProposal<Runtime>, Proposal<Runtime>>(
    |old| Some(Proposal { kind: ProposalKind::Inline(old.call), approvals: old.approvals, created_at: old.created_at }),
);
```

Run it as an `on_runtime_upgrade` hook with `#[storage_alias]` old types, and ship
`try-runtime` checks (`cargo run --release --features try-runtime -- try-runtime on-runtime-upgrade`).

### 3.5 Client side (`gpn_gov_upgrade --mode preimage`)

1. `stage` — unchanged (hashes the artifact, writes the plan).
2. `preimage-note` — submit `Preimage.note_preimage { bytes }` from **any** funded account (the
   depositor does not need to be a member). Idempotent: `note_preimage` errors
   `AlreadyNoted` if present; the tool treats that as success. Prints the preimage hash; assert
   it equals the plan's `code_hash` (they are the same hash function, which is a nice
   cross-check).
3. `propose --mode preimage` — submits `GpnGovernance.propose_preimage { hash, len }`. The
   extrinsic is ~40 bytes instead of ~500 KB, and the proposal is now approvable **by hash**
   alone.
4. `approve` / `status` / `execute` / `verify` — unchanged, except `status` resolves the
   preimage to display the code size/hash/version (it already does so for inline proposals).
5. `execute` must be preceded by a check that the preimage still exists (anyone may
   `Preimage.unnote_preimage` an *unrequested* preimage, and the depositor's consideration can
   lapse); if missing, `execute` fails with `PreimageMissing` **without consuming the
   proposal** — the fix is to re-note the same bytes (same hash, so the proposal id is
   unchanged) and execute again. `propose_preimage` should therefore `T::Preimage::request()`
   the hash (`QueryPreimage::hold`) so a missing preimage is a recoverable, visible condition
   rather than a silent one.

## 4. Test plan (must land with the implementation)

- pallet unit tests: propose-by-preimage, threshold, timelock, missing preimage at execute
  (`PreimageMissing`, proposal intact), length mismatch, non-member propose, duplicate hash.
- runtime integration test mirroring `gpn-node/runtime/tests/gov_upgrade_rehearsal.rs`: note a
  real 500 KB artifact as a preimage, propose by hash, fast-forward the timelock, execute,
  assert `:code` and `CodeUpdated`.
- end-to-end rehearsal script: `scripts/gov-upgrade-rehearsal.sh --mode preimage`.
- A failing-first test that proves the *current* runtime has no `Preimage` pallet (the tool's
  capability check), so a future re-introduction of `--mode inline` defaults cannot silently
  regress the invariant.

## 5. Why this is staged

Applying it is itself a runtime upgrade, and it changes the pallet that performs upgrades — the
one change you do **not** want to rush. Sequence:

1. review this design;
2. implement + test on the spec-42 branch (alongside the staged `MinNodeVersion` item in
   `wasm/README-pending-upgrade.md`);
3. build the artifact separately, `stage` it, and run the *normal* governance pipeline;
4. after it enacts, `gpn_gov_upgrade --mode preimage` becomes available and subsequent upgrades
   stop carrying raw WASM in a public call.
