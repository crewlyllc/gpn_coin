# Proof-of-Work Entry — design note (spec 47)

Status: **implemented in source, not enacted**. Owner decision 2026-11-04.
Supersedes the two rejected "free but unproven" attempts.

## 1. The circle we are removing

Measured 2026-11-04 on a fresh node:

| Requirement | Cost | Fresh node has |
|---|---|---|
| `submit_attestation` fee | ~0.0002 GPN | 0 |
| `register_provider` fee | ~0.0002 GPN | 0 |
| Existential deposit (account must exist) | 1 GPN | 0 |
| Block production | authority-only ⇒ no mining income | — |

So the only way to become a provider is to already be funded, and the only way
to be funded is to already be a provider (or be handed coins by the operator).
That is a closed circle. Bitcoin has no such circle: running a node is free,
mining is free to attempt, and *work* is what earns.

## 2. The gate: pay in computation, not in coins

A new extrinsic pair is **fee-free if and only if the caller supplies a valid
Ethash solution in the same call**:

- `GPUAttestation::attest_entry(pow, …)` — prove work + record identity.
- `ComputeMarketplace::register_provider_entry(pow, …, requirements)` — prove
  work + record identity + register the provider in **one** call.

`pow` is `Option<EntryPow>`. With `Some(proof)` the call is dispatched with
`Pays::No`; with `None` it is dispatched with `Pays::Yes` and behaves like any
other fee-paying call. The existing `submit_attestation` /
`register_provider` are unchanged and remain fee-paying.

Verification reuses `pallet-gpu-pow`'s existing solution check — the same
`blake2_256(encode(nonce, mix_hash, algorithm_version))` binding and the same
`meets_difficulty` comparison against the **live chain `Difficulty` target**.
No new hash function, no new difficulty constant: the entry price is exactly
the price the chain already puts on one block of work.

```
solution_hash == blake2_256(encode(nonce, mix_hash, algorithm_version))
solution_hash <  Difficulty::get()          // same target block miners use
algorithm_version == AlgorithmVersion::get()
```

### Difficulty floor and its measured basis

`Difficulty` is the chain's live retargeted target (spec 46 machinery: ±33 %
per window, 45-block window, 15 s target). On the current testnet that has
settled at the `MinDifficulty` floor `0x1000`; a solution at that target is a
few thousand nonces — sub-second on any discrete GPU, ~seconds on a CPU. On a
chain whose difficulty has risen, the entry costs proportionally more, which is
the intended behaviour: **entry is priced in the same unit as block production**,
so its price tracks the network's actual work.

## 3. Anti-spam bounds (all on-chain)

| Bound | Value | Enforced in |
|---|---|---|
| Fee-free entries per block | **2** default (`FreeEntryLimit`, root-mutable) | `FreeEntryCount` (block-number keyed) |
| Fee-free entries per account, ever | **1** | `FreeEntryAccounts` |
| Fee-free entries per GPU fingerprint, ever | **1** | `FreeEntryGpus` + `GpuOwner` |
| Fingerprint required on the free path | always | zero fingerprint ⇒ `GpuIdentityMissing` |
| Prior attestation must not exist | required | `Attestations::contains_key` |
| PoW validity | chain difficulty | `EntryPowVerifier::verify_entry_pow` |

`FreeEntryCount` stores `(block, count)` and resets when the block number
changes, so a limit of 2 means at most two *successful* free onboardings per
block regardless of how many blocks apart the attempts are.

### Cost per attestation vs block budget

One verification is a single `blake2_256` over 44 bytes plus four `u64`
comparisons — on the order of **1 µs** of execution, against a 6 s block
budget. The *expensive* part is the miner's GPU time, which the chain never
pays for: at `MinDifficulty` a solution takes ≲1 s of GPU time; at a mainnet-like
target it takes the same ~15 s a block does. Per block the chain will accept at
most 2 entries ⇒ ≤2 attestation writes + 2 registration writes of weight,
bounded and negligible against the block weight limit.

### What an attacker gains per unit of spend

- To register *k* providers, an attacker must produce *k* valid solutions at the
  live difficulty **and** present *k* distinct GPU fingerprints. Cost ≈ *k* ×
  (one block's GPU work) plus the hardware to hold *k* real GPUs.
- Registering more accounts does not help: the per-account and per-fingerprint
  bounds are permanent for the free path, and a second free entry for the same
  account fails with `FreeEntryAccountUsed`.
- Filling a block with free entries costs 2 × 15 s of GPU work and yields 2
  registrations — worse than simply earning a block reward for the same work.
  The attack has negative expected value.

## 4. Why "measured GFLOPS" alone is not proof

The current attestation challenge is a SHA-256 over public data plus a
self-reported `compute_gflops`. Both are free to fabricate: a CPU can hash a
public constant, and a number in a struct costs nothing. That is why the two
earlier "free" attempts were rejected. PoW is different because the *only* way
to produce a hash below the target is to actually iterate nonces — the cost is
paid to physics, not to the chain, and cannot be faked by a liar.

Note the honest limit: at `MinDifficulty` the work is small (≈10³ hashes). That
is deliberate for testnet onboarding; the bound that matters is the per-block
and per-identity cap, not the raw hash count. On mainnet the same call inherits
whatever difficulty the chain has retargeted to, which is the correct price.

## 5. Existential deposit

`testnet-zero-ed` (runtime feature) sets `ExistentialDeposit = 0` so a
zero-balance account can exist to receive an attestation. The mainnet build
keeps 1 GPN — this is a **chain-parameter decision, reversible without a code
change**, and the feature is off by default. `MinAttestationStake` is already 0
on every network (owner decision 2026-11-04), so no stake transfer occurs on
the free path.

## 6. Explicit non-goals

- **Not** a new consensus rule. Block production stays authority-only; this
  only prices *entry*.
- **Not** a new hash or a new difficulty schedule. The existing
  `pallet-gpu-pow` target is reused verbatim.
- **Not** a reward. A fee-free entry earns nothing; it only removes the
  bootstrap cost.
- **Not** a permanent relaxation of fees: the plain calls keep their fees, and
  the free path is one-shot per account and per GPU.
- **Not** an anonymity set: the fingerprint is still public and unique.
- **Does not** make invalid proofs free to *keep* — an invalid proof aborts the
  call. It remains possible to *submit* an invalid proof at zero fee (block
  weight only, ~1 µs verification); block authors do not include failing
  extrinsics, and the accepted-per-block cap bounds state growth. This residual
  is documented rather than hidden: closing it would need a signed-extension
  pre-check, which is out of scope for spec 47.

## 7. Rollout

Source only. Not enacted: no `set_code`, no genesis change, `spec_version`
46 → 47. Live rehearsal still required: build the `testnet-zero-ed` +
`testnet-no-stake` WASM, verify the canonical committed WASM is unchanged, run
a two-node testnet where a fresh key attests and registers with no faucet, and
confirm the second attempt by the same key is rejected.

## AMENDMENT 2026-11-21
> **Origin, recorded 2026-11-21 at the owner's correction:** the fee was not inherited from the
> chain's economics — it was proposed by the assistant in an earlier conversation, then codified
> here as "unchanged and remain fee-paying". A design document written by the assistant is not an
> owner decision, and this one should never have settled a cost.
 — registration is free on EVERY path (owner)

The paragraph *"The existing `submit_attestation` / `register_provider` are unchanged and remain
fee-paying"* is **withdrawn**. It recorded a fee structure nobody asked for: the fees (~0.0002 GPN)
were what the chain happened to charge on 2026-11-04, and this document preserved them instead of
asking whether they should exist. Owner intent, stated plainly on 2026-11-21:

> **Joining the network must never cost money on any path.** Not the entry path, not the
> re-registration path, not a fallback.

Consequences (rows 231/232):

- `ComputeMarketplace::register_provider` (call_index 0) gains `Pays::No` on its **weight
  annotation**, exactly as `accept_job` did in #205, so the transaction pool and the dispatch agree.
- `GPUAttestation::submit_attestation` is reviewed under the same rule.
- Any client text that says "paid path", "registration fee" or "deposit GPN" is wrong and is
  removed, including the spent-free-entry message, which must point at
  `register_provider_entry_unsigned` (no fee, no account needed, proof of work only).
- The only remaining costs to a new operator are **compute** (the proof of work) and, for AI work,
  escrow for jobs they request — never permission to exist.
