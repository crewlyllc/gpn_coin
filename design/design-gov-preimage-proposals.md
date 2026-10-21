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
pub type Proposals<T> = StorageMap<_, Blake2_256, T::Hash, Proposal<T>, OptionSwapQuery>;
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
exists in `pallet-gpn-governance`), and teach `pallet-gpn-governance` to accept a
**`ProposalKind`**:

```rust
pub enum ProposalKind<T: Config> {
    /// Today's behaviour: the call travels inline.
    Inline(T::RuntimeCall),
    /// New: only the hash travels; the bytes live in `pallet-preimage`.
    Preimage { hash: T::Hash, len: u32 },
}
```

Proposal becomes:

```rust
pub struct Proposal<T: Config> {
    pub kind: ProposalKind<T>,
    pub approvals: BoundedVec<T::AccountId, T::MaxMembers>,
    pub created_at: BlockNumberFor<T>,
}
```

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
    /// Resolve a noted preimage to its bytes. `None` if absent (not yet noted,
    /// or already consumed).
    type Preimage: PreimageProvider<Self::Hash>;
}
```

```rust
pub trait PreimageProvider<Hash> {
    fn get(hash: &Hash) -> Option<Vec<u8>>;
}
```

`propose_preimage(origin, hash, len)`:

- `ensure_signed` + `Members` check (identical to `propose`);
- `ensure!(len as usize <= MaxProposalCallLen::get())` — a new constant, e.g. 5 MB, so a
  proposal can never exceed `MaximumBlockLength`;
- **do not** verify the preimage exists at propose time (it may be noted later), but **do**
  verify it at `execute` time, where a missing/stale preimage is a hard error
  (`PreimageMissing`) — the failure must be *before* `Proposals::remove`, so the committee can
  still fix it (note the preimage, then execute);
- store `ProposalKind::Preimage { hash, len }`; the proposal id stays
  `blake2_256(SCALE(ProposalKind::Preimage))`, so ids are still content-addressed and two
  different preimage hashes give two different proposals.

`execute`:

```rust
let call = match proposal.kind {
    ProposalKind::Inline(c) => c,
    ProposalKind::Preimage { hash, len } => {
        let bytes = T::Preimage::get(&hash).ok_or(Error::<T>::PreimageMissing)?;
        ensure!(bytes.len() as u32 == len, Error::<T>::PreimageLengthMismatch);
        T::RuntimeCall::decode(&mut &bytes[..]).map_err(|_| Error::<T>::BadPreimage)?;
    }
};
```

`T::Preimage` is implemented for `pallet_preimage::Pallet<T>`:

```rust
impl<T: pallet_preimage::Config> PreimageProvider<T::Hash> for pallet_preimage::Pallet<T> {
    fn get(hash: &T::Hash) -> Option<Vec<u8>> {
        pallet_preimage::Pallet::<T>::get::<T::RuntimeCall>(hash)  // decode-checked
    }
}
```

(`pallet_preimage::Pallet::get::<Call>` already length-checks and decodes, which is why the
`len` field is belt-and-braces and also lets the UI show the size before the preimage is even
noted.)

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
5. `execute` must be preceded by a check that the preimage still exists (it can be unrequested);
   if missing, `execute` fails with `PreimageMissing` **without consuming the proposal** — the
   fix is to re-note the same bytes (same hash) and execute again.

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
