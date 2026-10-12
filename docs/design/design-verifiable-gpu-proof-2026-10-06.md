# GPN — Verifiable GPU Proof (stake-free attestation) — Design
_2026-10-06 · decided by Jeremy: "go with C, keep staking possible for the future"_

## Objective
Change the GPU attestation gate so that:
1. **No mandatory stake.** You do NOT need to lock GPN to become an attested miner.
2. **Real verification.** "GPU-only" is *enforced*, not self-reported (today `submit_attestation`
   stores a `challenge_response` it never checks → trivially fakeable, which is the *only*
   reason the stake existed).
3. **Staking becomes the carrot, not the stick.** Staking is OPTIONAL and **increases your
   reward** via the existing tier multiplier — this is the originally-designed intent
   (see Reconciliation below).

## Reconciliation with the original tokenomics design (2026-10-06 review)
Reviewing `design-tokenomics-spec.md` + `spec-one-gpu-one-reward.md` + `gpu-attestation`:
- The original intent WAS "staking increases your reward": `AttestationTier::staking_multiplier_bps()`
  = **Bronze 1.0× / Silver 1.5× / Gold 2.5×**, paid from the **staking-reward pool**
  (40% of the fee pool) by `distribute_staking_rewards`, weighted **staking × GPU tier multiplier**.
  Separate from the **block/mining reward** (50 GPN/block, halving ~2 yrs) paid to the miner.
- TWO gaps between design and on-chain reality:
  1. The stake is a **hard gate** (`MinAttestationStake = 1,000 GPN` enforced at attest) — so it is
     a *requirement*, not merely a boost. The "gate" and the "boost" were conflated.
  2. The multiplier payout is a **STUB**: the spec itself flags "`distribute_staking_rewards`
     currently just drains the pool and emits an event — needs a real per-staker payout keyed by
     GPU tier multiplier." So "staking → higher reward" is **not implemented yet**.
- **Resolution (this design):** make staking the *optional boost* (not the gate). No stake to mine;
  staking raises your tier ⇒ raises your reward multiplier. Thus BOTH of Jeremy's recollections hold:
  "no mandatory stake" AND "staking increases your reward."

## Work items (revised)
1. Remove the stake as a hard attestation gate (+ real verification).
2. **Implement the real tier-weighted `distribute_staking_rewards`** (finish the stub) — this is the
   "staking increases your reward" mechanism.
3. Keep `deposit_stake`/`withdraw_stake` (optional).

## Why the current gate is weak
- `gpu_attestation::submit_attestation` requires `ProviderStake::contains_key` (≥1,000 GPN).
- The `challenge_response`, `gpu_info_hash`, `gpu_count`, `compute_gflops` are **all
  self-reported and unverified on-chain**.
- `gpu_pow::distribute_block_rewards` pays the block author iff `attested_gflops(author) > 0`.
- Net: the *only* real cost is the stake. Remove the stake naively ⇒ free fake GPUs.

## Verification mechanism options

| # | Mechanism | Real? | On-chain verify cost | Effort | Notes |
|---|-----------|-------|----------------------|--------|-------|
| 1 | **NVTrust / GPU TEE hardware attestation** | ✅ strongest | signature chain (cheap) | high | NVIDIA GPUs already targeted; needs cert chain + oracle/native verify |
| 2 | **Ethash challenge–response verified natively** | ✅ | node-native (has epoch cache), result surfaced on-chain | med | reuses the consensus Ethash verifier; runtime can't do Ethash (needs 16 MB cache) |
| 3 | **PoW-derived label ("mined ≥K blocks in window")** | ✅ | free | low | proof = blocks the node already Ethash-verified on import |
| 4 | **Self-reported + random committee spot-checks** | partial | low | med | weak; social/oracle trust |

### Recommendation: (3) for mining eligibility + (2) for marketplace capacity, staking optional

**(3) PoW-derived mining eligibility — the clean, Bitcoin-like answer.**
The node already verifies every block's **Ethash seal** on import (that's how consensus works).
So "this account produced a valid block" *is* proof of real GPU work — no extra verification
needed. Define:
- `BlocksAuthored[a]` = rolling count of blocks authored in the last `AttestationWindow` blocks
  (the author is already stamped in the pre-runtime digest).
- **Attested-for-mining** iff `BlocksAuthored[a] ≥ K` (e.g. K = 3 in a 100-block window).
- `distribute_block_rewards` pays the author iff attested-for-mining, else beneficiary (bootstrap).
- Result: **GPU-only enforced by PoW itself**, no stake, no self-report, stake-optional.
- Bootstrap note: the first K blocks an account produces go to the beneficiary; after that the
  author is paid. (Or grandfather-in already-attested accounts at upgrade.)

**(2) Marketplace capacity challenge (for serving AI jobs).** Keep `submit_attestation`, but
replace the unverified `challenge_response` with a **node-native challenge**:
- Chain issues a challenge (derived from a recent block hash → unpredictable);
- provider returns a valid **Ethash solution** at a target;
- the **node** verifies it natively (it has the epoch cache) and the result is committed
  on-chain (off-chain-worker / signed result), exactly like a block seal.
- This proves GPU-class work for the advertised throughput tier.

**Staking (optional, kept):** `deposit_stake`/`withdraw_stake` remain. Repurposed to confer
future *benefits* (Silver/Gold exposure, marketplace priority, staking-reward share) — never a
hard gate.

## Migration
- Runtime change ⇒ forkless `set_code`, spec_version > current on-chain (10). Canonical bundled
  wasm must NOT be regenerated (hard rule) — build a separate WASM.
- Pallet changes:
  - `gpu-attestation`: drop the `ProviderStake::contains_key` requirement from
    `submit_attestation`; keep stake extrinsics; add native challenge verify hook.
  - `gpu-pow`: add `BlocksAuthored` ring + `is_mining_attested()`; switch the payout gate to it
    (grandfather existing attested accounts at first upgrade).
- Off-chain: node already verifies Ethash seals; expose the verified author for the counter
  (already via pre-runtime digest).

## Open decisions for Jeremy
- K / window size for PoW-derived eligibility (proposed K=3 / 100 blocks).
- Whether to grandfather current attested accounts (Alice/Bob) so they aren't dropped for K blocks.
- Order of work: do (3) first (unblocks no-stake mining fast), then (2).

## Status
- [x] design review (this doc)
- [x] implement (3) PoW-derived eligibility  — spec 11 (live)
- [~] implement (2) native challenge verify
  - [x] **node-native Ethash verifier primitive** (`gpn-node/src/challenge.rs`,
        `verify_challenge_solution(challenge, seal, target)` + `parse_seal_hex`;
        reuses the node's dev-epoch light cache; GPU-miner-compatible seal layout
        `nonce(8 LE) ++ mix_hash(32)`; 5 unit tests green incl. a real light-cache
        mine→verify roundtrip, wrong-challenge, above-target, bad-length). 2026-10-08.
  - [x] **runtime verifier-only `confirm_challenge` extrinsic + `ChallengeVerified` map**
        (`gpu-attestation`, call_index 4): new `Config::ChallengeVerifier` (testnet ops key
        `5HHSuwZ…` = `e6e50d8f…`), `ChallengeVerified: StorageMap<AccountId,[u8;32]>` (stores the
        solved challenge; **no `AttestationRecord` layout change** ⇒ live records safe),
        `challenge_verified()` helper, and `apply_challenge_gate` capping **new** Silver/Gold
        submissions at Bronze unless node-verified (Bronze open; expiry sweep does NOT gate ⇒
        legacy records never demoted). 30/30 pallet tests green; `gpn-runtime` checks clean;
        canonical genesis WASM untouched. 2026-10-08.
  - [x] node service task that reads `AttestationChallenge`, calls the verifier, and
        submits the committed result (`confirm_challenge` signed by the verifier key)
        — **node RPC `gpn_confirmChallenge(provider, seal)`** (`gpn-node/node/src/rpc.rs`):
        reads the live `GPUAttestation::AttestationChallenge` from state, verifies the
        seal natively via `challenge::verify_challenge_solution` (target from
        `GPN_CHALLENGE_TARGET`, default 2^240), and on success builds + signs
        (verifier key from `GPN_CHALLENGE_VERIFIER_URI`) + submits the verifier-only
        `confirm_challenge` extrinsic through the tx pool. Runs the node's own signing
        (no externalities needed — spec/tx version + genesis read from the client runtime
        API). 2 unit tests green (target parse, signed-extrinsic encode/decode round-trip);
        `gpn-node` builds clean; canonical genesis WASM untouched. 2026-10-08.
- [ ] runtime upgrade via set_code (Part 1 + Part 2 ship in ONE spec bump; not applied —
      awaiting go-ahead; see GPN-TASKS.md item 2/6)

### Relationship to the minimum-GPU-capability gate (item #19)

The minimum-GPU-capability gate (item #19) is the **eligibility** half of this
proof story: a runtime const `MinEligibilityGflops` (default 3,000 GFLOPS = 3 TFLOPS)
rejects a submission whose `compute_gflops` is below the floor, so a CPU/APU
integrated GPU (≈1–2 TFLOPS) cannot attest ⇒ cannot earn. **But `compute_gflops`
is self-reported at attestation**, so on its own the floor is *spoofable* — a box
can simply claim ≥ the floor. That is precisely why the two mechanisms must ship
together: the **capacity challenge (this doc)** is what turns a *claimed* TFLOPS
figure into a *proven* one (a provider must actually mine a seal over the live
`AttestationChallenge`). Until the challenge-backed figure is what the floor is
compared against, treat the floor as a first-line filter, not a guarantee. The
off-chain scheduler gate (`gpn_scheduler` `min_tflops` + the `gpu_class` flag on
the heartbeat) is the same idea one hop earlier — advert + route, still advisory.

### Part-2 rollout notes (for review)
- Runtime can't run Ethash (no `ethash` dep, no 16 MB cache in wasm) ⇒ verification is
  **node-native**; only the boolean result is committed on-chain by a **trusted verifier
  account** (new `Config::ChallengeVerifier`), mirroring how block seals are node-verified
  at import. Testnet-acceptable; a production design would use a verifier committee.
- Because live attestation records exist on-chain, adding a `verified` field requires either
  a storage migration or a *separate* `ChallengeVerified: StorageMap` (preferred — no record
  layout change). Keep legacy/unverified providers non-disruptive: only newly-gated tiers
  require a verified challenge, so the live marketplace isn't zeroed on upgrade.
