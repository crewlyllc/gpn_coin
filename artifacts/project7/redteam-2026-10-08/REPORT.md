# GPN Red-Team Report — 2026-10-08

Adversarial audit of `gpn-node/pallets/*`, `gpn-node/runtime`, and `gpn-agent`
(chain client/signer/faucet/scheduler). Method: read adversarially, enumerate
attack surfaces, construct concrete exploits, and **reproduce each finding in a
test before fixing**. Fixes are the smallest correct change + a regression test.

Canonical genesis WASM untouched: `sha256 = 76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97`
(verified before/after the staging build).

---

## FIXED FINDINGS

### F1 — `gpu-attestation::check_expired` permissionless stake drain  (HIGH)
- **Class:** auth/permission + economic DoS (asset destruction).
- **Exploit:** `check_expired(target)` is `ensure_signed` (any account) and
  counted one miss + applied one `10%`-of-*current*-stake slash per **call**.
  An attacker calls it repeatedly in a single block against any provider that is
  past `MaxAttestationAge`; each call re-slashes 10% of the *remaining* stake,
  draining it. Reproduced: a 1000-unit stake → **432** after 10 same-block
  calls (1000→900→810→729→…). No gain to the attacker; total loss to the victim.
- **Fix:** rate-limit miss-counting to at most one per `MaxAttestationAge`
  window via new `LastMissBlock` storage (cleared on (re)attest/unbond), so
  same-window repeats are a no-op.
- **Test:** `test_repeated_check_expired_does_not_drain_stake` (fails: 432,
  passes: 1000). `test_expired_attestation_slashes_after_missed_limit` updated
  to space periods by a full window.
- **Commit:** `fix(gpu-attestation): rate-limit check_expired misses…`

### F2 — `profitability-oracle::resolve_challenge` missing authorization  (HIGH)
- **Class:** auth/permission bypass (bond theft / slashing evasion).
- **Exploit:** `resolve_challenge` was `ensure_signed` with **no** authority
  check, and the caller supplies the verdict. The `Dismiss` (2) verdict burns
  the **challenger's** bond with no on-chain predicate to verify, so:
  (a) any third party can burn an honest challenger's bond at will; and
  (b) a challenged operator can call it on their own challenge to `Dismiss`,
  escaping slashing. (For `SlashStale`/`SlashOutlier` the conditions are
  on-chain-verifiable, so those were benign — `Dismiss` was the vector.)
- **Fix:** gate resolution to a **committee member**, or **root** (break-glass),
  mirroring `remove_operator`.
- **Test:** `test_resolve_challenge_requires_committee_or_root` (fails: non-member
  resolves, passes: rejected + committee still works).
- **Commit:** `fix(profitability-oracle): authorize resolve_challenge…`

---

## REPORTED, NOT FIXED (would require a chain reset / ceremony)

### F3 — Shielded pool: forgeable Groth16 proofs (public deterministic CRS)  (CRITICAL)
- **Class:** forgeable zk proof → **total theft of the shielded pool**.
- **Why:** `groth16::note_spend_key()` / `value_balance_key()` are derived by
  `gpn-shield` setup from a **public, deterministic seed**
  (`testnet-crs-note-spend-v0` via a xorshift PRNG). The toxic waste (τ, α, β, δ)
  is therefore derivable by anyone, so any account can mint a valid proof for
  arbitrary public inputs. In `spend` the public inputs are
  `[value_balance, cm_lo, cm_hi, nf_lo, nf_hi, merkle_root]`; an attacker can
  pick a live pool `commitment`, an arbitrary **unspent** `nullifier`, a known
  `merkle_root` (from `RootHistory`), and `value_balance = PoolValue`, with
  `recipient = attacker` — draining the entire `PoolValue` to themselves.
  The pallet's on-chain guards (nullifier-unspent, root-known, commitment-in-pool,
  `PoolValue` cap) do **not** stop this: every field they check is attacker-chosen
  and mutually consistent under a forged proof.
- **Note:** the doc-comment acknowledges "proofs are forgeable by anyone" but
  wrongly claims the on-chain checks are "the real guards" — they are not, because
  forging lets you satisfy all of them.
- **Fix (out of scope / chain-reset class):** a real Powers-of-Tau / Sapling
  ceremony to replace the deterministic CRS. Replacing the baked-in VK would
  invalidate every existing note/proof (their VK changes), i.e. it is a
  chain-reset-class change → **STOP + report** per the hard rules.
- Secondary issue in the same `spend` (moot while F3 stands): the shielded→shielded
  **output** commitment is passed as raw bytes and is *not* one of the proof's
  public inputs, so the circuit cannot bind it. Transparent extraction is still
  capped by `PoolValue`, so it is not an independent inflation vector.

---

## PROBED, COULD NOT BREAK (attack surfaces tested)

- **compute-marketplace escrow/settlement:** single `JobEscrow::take` guards
  double-pay; `complete`/`cancel`/`fail`/`expire` are mutually exclusive on state;
  fee/refund dust folding keeps the pallet at 0; per-requestor + global caps bound
  slot DoS; expiry sweep is bounded and idempotent.
- **Off-chain escrow channels:** `close` needs both sigs; `force_close` needs the
  counterparty sig + `now >= deadline` + caller-is-a-party + strictly-increasing
  nonce; `settle` enforces balance conservation against the deposit; one live
  channel per (requestor, provider). No replay/forgery found.
- **gpn-token:** every mint funnels through `mint_within_cap` (canonical counter,
  saturating add, 1B cap); `distribute_*` are root-only (a permissionless caller
  draining pools was already fixed); real burn via slash; `total_bps` cannot be 0
  (config validated at genesis/`set_fee_distribution`).
- **gpn-treasury:** 13-of-25 roster approvals, no double-approve, no re-execute,
  `amount <= unlocked - released`; `set_roster`/`set_threshold` are root-only;
  empty-roster error after `put` is rolled back on dispatch error.
- **gpu-pow:** block-reward payout is gated on the seal-verified author read from
  the pre-runtime digest (bound via `pre_hash`) + canonical supply cap; halving
  and windowed-difficulty math bounded. (See note below.)
- **gpn-liveness:** self/duplicate-witness guards, bounded uptime bitmask, one
  finalization per window, saturating slashing.
- **gpn-governance:** 2-of-3 members + timelock, call-hash-keyed proposals,
  removed-before-dispatch.
- **gpn-bridge:** trusted-relayer gating, content-hash replay dedupe, `TotalLocked`
  solvency bound, per-window rate limit, pause breaker; root-only admin.
- **node seal verification:** `verify_seal` recomputes Ethash over the block
  pre-hash and checks mix + target; author bytes in the seal are redundant (the
  authoritative author is the pre-runtime digest, authenticated by `pre_hash`).
- **agent signer:** `Sr25519Signer` uses real sp-core keys; channel signatures
  verified on-chain; no hardcoded secrets beyond dev URIs in tests/config defaults.

## NOTED, NOT FIXED (low / latent)

- **`gpu-pow::submit_solution` uses a caller-supplied `difficulty`** for the PoW
  check instead of the canonical stored `Difficulty` (a classic difficulty
  bypass). Impact is currently **low**: `PendingBlockRewards` /
  `PendingRewardBeneficiary` are write-only (never consumed for payout — the real
  reward path is `on_initialize` + `FindAuthor`), so no funds move. Left as-is to
  avoid changing the test/benchmark semantics of a vestigial extrinsic; flagged
  for removal or hardening when the pending path is wired.
- **`gpu-attestation::attested_gflops` returns self-reported `compute_gflops`
  regardless of tier**, so the marketplace `min_gflops` "proof-of-compute" gate is
  bypassable by a self-reported number. It gates matching (not funds), and the
  honest fix (binding gflops to a node-verified measurement) is a design change,
  so it is reported rather than patched.
- **Slash proceeds** in `gpu-attestation` and `gpn-liveness` reduce the tracked
  stake/pool but leave the currency in the pallet account (stranded, not burned) —
  an accounting nit, not exploitable.

---

## STAGED RUNTIME UPGRADE (not applied)

F1/F2 are runtime (WASM) fixes. A spec_version-16 runtime artifact is staged for
forkless `set_code`:

- `artifacts/project7/redteam-2026-10-08/gpn_runtime.compact.compressed.v16.wasm`
- sha256: `7a8800b1afc90202bfbb5f1a325baa73053dbcc967caaf3eb36f4eb0db9141ec`
- `spec_version` bumped 15 → 16 in `gpn-node/runtime/src/lib.rs`.
- Built with the canonical genesis WASM moved aside then **restored**
  (post-build sha re-verified `76f37b23…`).
- **NOT submitted** — no `set_code` was applied.
