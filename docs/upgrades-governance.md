# Runtime upgrades on mainnet — the governance pipeline

**Audience:** the GPN committee (proposers/approvers), the launch operator, and anyone
verifying an upgrade happened correctly. **Status:** tool + runbook implemented and rehearsed;
no runtime change has been applied by this work.

> **Spec 42 — "governance hardening" is STAGED, not applied.** A separately built spec-42
> artifact (committee 5 / 3 approvals, 14-day proposal expiry, 3-of-5 cancel, hash-addressed
> preimages, hand-set weights, storage migration v0→v1) exists at
> `wasm/gpn_runtime_spec42_gov_hardening.compact.compressed.wasm`. The plan, review checklist
> and exact commands are in `shared/artifacts/project7/staged/gov-hardening-v1.md`. Everything
> below describes **both** the live (spec 41) behaviour and, where marked *spec 42*, the staged
> behaviour. Until the upgrade enacts, the live chain is still 2-of-3 with no cancel/expiry.
>
> **Spec 43 — "symmetric retarget + optimistic claim" is STAGED, not applied.** Built on top of
> the spec-42 source: `wasm/gpn_runtime_spec43_retarget_clamp.compact.compressed.wasm`
> (519 286 B, `sha256 5604f140…d10cd`, code hash `0x43b6777f…8361c`, `spec_version = 43`).
> Plan, evidence and rollback: `shared/artifacts/project7/staged/retarget-clamp-v1.md`;
> simulation: `shared/artifacts/project7/analysis/retarget-sim-2026-10-22.md`. See §7.2.

Companion docs:

- `docs/mainnet-launch-runbook.md` — the launch itself, and the post-launch governance actions
- `design/design-gov-preimage-proposals.md` — the *missing* hash-addressed proposal path (staged)
- `wasm/README-pending-upgrade.md` — the staged spec-42 artifact (not built, not applied)
- `docs/incident-response.md` — what to do when an upgrade goes wrong

---

## 1. Why mainnet cannot use the sudo tool

`gpn-agent/src/bin/gpn_upgrade_runtime.rs` is correct for **dev and testnet**: it submits
`Sudo.sudo(System.set_code(WASM))`. On mainnet `Sudo`'s key is the **inert all-zeros
placeholder** `AccountId::new([0u8; 32])` — no private key exists, and the account is not
endowed (decision D2, `chain_spec::mainnet_chain_spec()`).

That leaves exactly one origin able to reach `System.set_code`: **Root**. On mainnet Root is
reachable only through `pallet-gpn-governance`.

> Keep the sudo tool for dev/testnet. It is *not* a mainnet path and must not be documented as
> one.

## 2. The governance surface as it actually is

Read it from a running node at any time:

```bash
gpn-agent/target/debug/gpn_gov_upgrade surface --endpoint ws://<node>:9944
# or: --json for machines
```

### 2.1 What exists

| Piece | Live (spec 41) | Staged (spec 42) | Where it comes from |
|---|---|---|---|
| Pallet | `GpnGovernance` (`pallet-gpn-governance`) | same | `gpn-node/runtime/src/lib.rs` |
| Members | **3** (`testnet_committee()`), genesis-seeded | **5** (committee-1…5), seeded by the storage migration and replaceable by `set_members` | `node/src/chain_spec.rs`, `GovernanceMigrationMembers` |
| Approvals required | **2** | **3** | `GovernanceApprovalsRequired` |
| Timelock | **17 280 blocks** ≈ 3 days at 15 s | unchanged | `GovernanceTimelockPeriod = 3 * DAYS` |
| Proposal expiry | **none** | **80 640 blocks** (14 days); `execute` → `ProposalExpired`, pruned in `on_initialize` | `GovernanceProposalExpiry` |
| Cancel / veto | **none** | `cancel(hash)` requires **3 of 5** approvals | pallet call 3 |
| Proposal encoding | whole call inline | `ProposalCall::{Inline, Preimage}`; `propose_preimage(hash, bytes)` stores bytes once under `blake2_256` | pallet calls 0 and 5/6 |
| Max members | 8 (`MaxMembers`) | 8 | `GovernanceMaxMembers` |
| Max call/preimage | — | 4 MiB (`MaxProposalCallLen`) | `GovernanceMaxProposalCallLen` |
| Open proposals | unbounded | 64 (`MaxOpenProposals`, index + prune bound) | `GovernanceMaxOpenProposals` |
| Proposal id | `blake2_256(SCALE(RuntimeCall))` | same for inline; `blake2_256("gpn/gov/preimage/v1" ++ hash ++ len)` for preimage | `T::Hashing::hash_of`, `preimage_proposal_id` |
| Executor | anyone (signed, not member-gated) | unchanged | pallet call 2 |
| Root dispatch | `call.dispatch(RawOrigin::Root)` | unchanged | pallet call 2 |
| Weights | `WeightInfo = ()` (**zero**) | hand-set, conservative (`weights::GpnWeight`) | `pallets/gpn-governance/src/weights.rs` |
| Storage version | 0 | 1 (migration `on_runtime_upgrade`) | `STORAGE_VERSION` |

Live check (testnet, 2026-10-21, before the upgrade):

```
committee  3 member(s), 2 approval(s) required, max 8
members    5HeUVTK6zd9...5JqYej5h  5Cd5NiY2qkG...pGUvEX3Dx  5HTWBtWro5A...3PJRteWN
timelock   17280 blocks (~3d 0h at 15 s/block)
```

After spec 42: `committee 5 member(s), 3 approval(s) required, max 8`, expiry
`80640 blocks (~14d)`, and the committee addresses are exactly those in
`governance-keys.txt` / `governance-addrs.txt` (committee-1 … committee-5).

The 25-address roster with the **13-of-25** threshold is `pallet-gpn-treasury`. It governs
*spending*, **not** consensus. A treasury majority cannot change the runtime, and the committee
cannot spend treasury funds.

### 2.2 What does **not** exist (and therefore cannot be used)

The runtime contains **no** `pallet-democracy`, `pallet-collective`, `pallet-preimage`, or
`pallet-scheduler` (spec 42 keeps the local `Preimages` map instead of adding
`pallet-preimage`, so no new pallet index or deposit machinery is introduced). Consequences:

- **No referenda, no conviction voting, no tracks.** There is no stake-weighted vote and no
  "aye/nay" phase. The committee decides; the chain only enforces the 3-of-N + timelock rule.
- **No technical-committee fast-track** and **no emergency track**. The timelock is a runtime
  constant; it cannot be shortened by any call, including a root call, without a runtime
  upgrade (chicken and egg).
- **Preimage indirection exists from spec 42** (`propose_preimage`, `note_preimage`,
  `Pallet::peek`). Before it, `propose(call: Box<RuntimeCall>)` stored the whole call inline, so
  a `set_code` proposal put the full WASM in the `propose` extrinsic and kept ~500 KB of state
  until execution. `--mode preimage` refuses to run against a runtime without
  `propose_preimage` in its metadata (exit 4).
- **Veto/cancel exists from spec 42** and is deliberately *symmetric* with execution: it needs
  the same 3 approvals, so one member can neither veto nor unblock alone. Expiry (14 days) is
  the backstop that keeps abandoned proposals from leaking state.

### 2.3 The upgrade origin, in one line

> `GpnGovernance.execute(hash)` → `RawOrigin::Root` → `System.set_code { code }`.
> 3 distinct committee approvals (of 5) + 17 280 blocks of public notice, and no single key can
> act.

## 3. Security invariants (non-negotiable)

1. **Never a raw WASM in a public call.** From spec 42 this is enforced *by the chain*: use
   `propose --mode preimage`, which submits `propose_preimage(hash, bytes)` — the proposal
   carries `{ hash, len }` (36 bytes) and anyone can read `GpnGovernance.Preimages[hash]` and
   re-hash it before the timelock elapses. On a spec-41 chain the invariant cannot be satisfied
   for `set_code`, so it is enforced socially: the committee **must** verify the artifact hash
   out-of-band and compare it with `stage`'s output before proposing. `stage` prints `sha256`
   (release identity) and `blake2_256` (the code hash).
2. **Always the code hash.** Every step is keyed by hashes, never by "the file we think we
   built": plan → `code_hash`, proposal → `proposal hash`, verification → `:code` hash.
3. **Always verify both `specVersion` *and* `:code`.** A version bump alone can come from a
   different artifact; a code hash alone does not prove the runtime is functional.
4. **Never propose an artifact the chain would reject.** `System.set_code` enforces
   `spec_name == chain.specName` and `spec_version > chain.specVersion`. `gpn_gov_upgrade stage`
   refuses to write a plan unless both hold (this is how the stale committed WASM — spec 6 — is
   caught before it reaches the committee).
5. **Never run the proposer against a `--rpc-methods=safe` node.** `author_submitExtrinsic` is
   an *unsafe* RPC. The launch runbook (§4) correctly tells *operators* to use `safe`; the
   committee must submit through a node run with `--rpc-methods=unsafe` (or polkadot.js), and
   must not expose that node publicly.
6. **Rollback is a new release, not a code revert.** `set_code` refuses a non-increasing
   `spec_version` (§6).
7. **No genesis/canonical-WASM edits.** The bundled
   `gpn-node/runtime/gpn_runtime.compact.compressed.wasm` is the genesis artifact and is
   immutable; upgrades ship as separately built artifacts.

## 4. Who does what

| Step | Who | Signed by |
|---|---|---|
| build + stage the artifact | release engineer | nobody (offline) |
| `propose` | committee member A | A's key (proposer self-approves → 1/3) |
| `approve` | committee members B and C | B's and C's keys (→ 3/3) |
| wait | everybody | — (public timelock, 17 280 blocks) |
| `execute` | **anyone** (an ops bot is fine) | any funded key (execution is permissionless) |
| `verify` | whoever executed + a second, independent operator | — |
| `cancel` (spec 42) | any member **after 3 approvals** | that member's key |
| rollback decision | committee (3 of 5) | A + B + C again |

Nothing here requires the members to be online at the same time except that all three must sign
before the timelock-starting proposal is useful (the clock starts at `propose`, not at 3/3 —
so **secure the remaining approvals promptly**, otherwise the 3-day window is partly wasted).
After spec 42 an unexecuted proposal also dies on its own after **14 days**; re-propose rather
than letting a stale approval set linger.

## 5. The runbook

All commands are `gpn-agent/target/release/gpn_gov_upgrade` (build with
`cargo build --release --bin gpn_gov_upgrade`), `E=ws://<unsafe-rpc-node>:9944`.

### 5.1 T-7d — build and stage the artifact

Build the runtime **alone** (never the canonical bundled artifact):

```bash
cd gpn-node
# build.rs embeds the committed canonical WASM when it is present, so the
# convenience script moves it aside for the duration of the build and restores
# it hash-verified afterwards (it never regenerates or overwrites the canonical
# genesis artifact).
bash scripts/build-spec42-wasm.sh
# -> target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm
# -> prints size + sha256, and re-prints the canonical hash to prove it is intact
cp target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm \
   ../wasm/gpn_runtime_spec42_gov_hardening.compact.compressed.wasm
```

The staged spec-42 artifact is already built and checked in:
`wasm/gpn_runtime_spec42_gov_hardening.compact.compressed.wasm`
(**512 888 bytes**, `sha256 7694f7f6c547df93b7c22d53863acae5b83532721a28c10375d2a233333d30f1`,
code hash `0xf0c6a5ea7891836c15364ac9a029a05821d69aef2b38a36130472de414245e48`,
`spec_version = 42`). Any rebuild must reproduce these numbers or the plan must be re-staged.

Verify what you built, not what you intended:

```bash
gpn_gov_upgrade stage ../wasm/gpn_runtime_spec42.compact.compressed.wasm --endpoint $E
```

Expected output: `spec_name=gpn spec_version=42`, `code hash 0x…`, and
`plan written: .state/gov-upgrade/spec42_staged<codehash8>.plan.json`.

`stage` **fails (exit 3)** if the artifact is not WASM/compressed-WASM, has no
`runtime_version` section, has a different `spec_name`, or does not increase `spec_version`.
Distribute the artifact **and** the plan file; every committee member re-runs `stage` on their
own copy and compares `sha256`, `code hash` and `spec_version` before approving anything.

### 5.2 T-3d — propose (member A)

**On spec 42+ (recommended):** hash-addressed, so the public call carries 36 bytes, not 500 KB.

```bash
gpn_gov_upgrade propose --mode preimage \
    --plan .state/gov-upgrade/spec42_staged<…>.plan.json --signer <A-uri>
```

**On spec 41 (the only path until the upgrade enacts):** the call carries the raw WASM inline.

```bash
gpn_gov_upgrade propose --mode inline \
    --plan .state/gov-upgrade/spec42_staged<…>.plan.json --signer <A-uri>
```

Both print a **proposal hash**: `blake2_256(SCALE(System.set_code{code}))` for inline, and
`blake2_256("gpn/gov/preimage/v1" ++ code_hash ++ len_le)` for preimage. That hash is the id
everything else uses; publish it (with `sha256` and `code hash`) on the ops channel.

`--mode preimage` exits **4** if the runtime metadata has no `GpnGovernance.propose_preimage`,
so it cannot be used by accident against the pre-hardening pallet.

Re-running `propose` with identical bytes is a **no-op** (the pallet rejects duplicates; the
tool detects the existing entry and exits 0) — safe to retry after a failed submission.

### 5.3 T-3d — approve (members B and C)

```bash
gpn_gov_upgrade approve 0x<proposal-hash> --signer <B-uri>
gpn_gov_upgrade approve 0x<proposal-hash> --signer <C-uri>
```

Before signing, B checks the **`status`** output:

```bash
gpn_gov_upgrade status 0x<proposal-hash> --plan .state/gov-upgrade/spec42_staged<…>.plan.json
```

and must see `code … blake2 0x<code hash>` and `matches plan code hash ✓`, plus the decoded
`spec_version` from the **on-chain** bytes. If either differs, stop: someone proposed different
bytes.

### 5.4 The timelock (public notice window)

17 280 blocks. `status` prints the exact executable block and an ETA. Watch for:

- a competing proposal that reaches 3/5 first (it would upgrade first — `verify` detects the
  code-hash mismatch and tells you to re-stage against the new version);
- any sign that members' keys are compromised. On spec 41 there is **no cancel**: the only
  defense is not reaching the threshold, then re-proposing after rotating the committee via a
  *separate* governed upgrade. On spec 42, `cancel` (3 of 5) removes the proposal, and an
  unexecuted proposal expires on its own after 14 days (that includes one whose approvals were
  collected during a key-compromise scare).

### 5.5 T-0 — execute (anyone)

```bash
gpn_gov_upgrade execute 0x<proposal-hash> --signer <ops-key>
```

The tool pre-flights the threshold and timelock (and exits 2 with the remaining block count if
not ready). The pallet re-checks both. On success the proposal is removed and `execute`'s Root
dispatch runs `System.set_code`.

### 5.6 T+0 — verify

```bash
gpn_gov_upgrade verify --plan .state/gov-upgrade/spec42_staged<…>.plan.json --timeout 900
```

Success requires **all** of:

- `state_getRuntimeVersion.specVersion == 42`;
- `:code` (raw storage `0x3a636f6465`) hashes to the plan's `code hash`;
- block production is still advancing (a bad runtime stalls the chain);
- optionally `--ap-url http://<access-point>:8092/health`.

Exit codes: `0` enacted and matching, `2` timed out (still pending / chain stalled), `3`
**mismatch** (someone else's code, or the wrong artifact), `1` operational error.

Independent verification (do not skip — this is the whole point of hashes):

```bash
curl -s -H 'Content-Type: application/json' \
  -d '{"id":1,"jsonrpc":"2.0","method":"state_getStorage","params":["0x3a636f6465"]}' \
  http://127.0.0.1:9944 | python3 -c 'import json,sys,hashlib;b=bytes.fromhex(json.load(sys.stdin)["result"][2:]);print(len(b), hashlib.blake2b(b,digest_size=32).hexdigest())'
```

### 5.7 Failure paths

| Symptom | Meaning | Action |
|---|---|---|
| `execute` → `NotEnoughApprovals` | threshold not reached | get the missing approval |
| `execute` → `TimelockNotElapsed` | too early | wait; proposal is intact (a failed check does not consume it) |
| `execute` → `ProposalExpired` (spec 42) | older than `created_at + 14 d` | re-propose; `on_initialize` will prune the stale entry |
| `execute` → `PreimageMissing` (spec 42) | the bytes behind a hash-addressed proposal are gone | re-note the same bytes with `note_preimage` (same hash ⇒ same proposal id) and execute again — the proposal and its approvals survive |
| `cancel` → `NotEnoughApprovals` | fewer than 3 approvals | collect the missing approvals; there is no single-member veto |
| `execute` → `ExecutionFailed` | Root call reverted (bad WASM, `SpecVersionNeedsToIncrease`) | spec 41 consumes the proposal; **spec 42 keeps it** (removal happens only after a successful dispatch), so the artifact can be fixed and the same proposal retried while its approvals are still valid |
| `verify` exit 3 `CodeMismatch` | a different upgrade landed first | re-stage against the new `specVersion` |
| `verify` exit 3 `VersionMismatch` | `:code` matches but the version did not move | the artifact's version section is wrong — rebuild |
| `verify` exit 2, head stalled | runtime upgrade broke block production | incident response; committee rollback (§6) |

## 6. Rollback

`System.set_code` **refuses** `spec_version <= current` (`SpecVersionNeedsToIncrease`), so "put
the old WASM back" is not a valid call. Two honest paths:

**A. Re-release the old code (recommended).** Check out the previous runtime source, set
`spec_version = current + 1`, build a fresh artifact, and run the *normal* pipeline
(`stage` → `propose` → `approve` → timelock → `execute` → `verify`). Behaviour goes back; the
version only moves forward. `gpn_gov_upgrade rollback --to <old-source-wasm>` prints exactly
this sequence.

**B. Emergency `System.set_code_without_checks` (root, skips the version and `spec_name`
checks).** Governance *can* dispatch it because it is just another root call:

```bash
gpn_gov_upgrade rollback --to <artifact> --call set_code_without_checks \
    --i-understand-risk --submit --signer <A-uri>
```

Caveats, all real: it still takes the **full 3-day timelock** (there is no fast track), it
bypasses the only automatic sanity check the chain has, and a wrong blob bricks the chain. Use
it only when `verify` says the chain is broken and the committee has jointly checked the
artifact hash out-of-band.

**What you cannot do:** shorten the timelock or "undo" an execution. On spec 41 you also cannot
cancel a pending proposal; from spec 42 `cancel` (3 of 5) removes it, and expiry removes it after
14 days. Everything else is a new governance action.

## 7. Spec 42 — governance hardening (staged, not applied)

Built, tested and staged; **not** enacted. Full plan, review checklist and rollback path:
`shared/artifacts/project7/staged/gov-hardening-v1.md`.

| Change | Effect |
|---|---|
| Committee 3 → 5, approvals 2 → 3 | `GovernanceApprovalsRequired = 3`; `Members` seeded to committee-1…5 by the storage migration (union with any existing member, so nobody is silently dropped) |
| `set_members(Vec<AccountId>)` | Root-only (i.e. only through propose → 3/5 → timelock → execute); bounded by `MaxMembers = 8`, rejects empty/oversized/duplicate sets |
| `ProposalExpiry = 14 days` | `execute` → `ProposalExpired` past `created_at + expiry`; `on_initialize` prunes expired entries (bounded by `MaxOpenProposals = 64`) |
| `cancel(hash)` | same threshold as execute (3 of 5) — no single-member veto, no single-member block |
| `propose_preimage(hash, bytes)` + `note_preimage(hash, bytes)` + `Pallet::peek` | the proposal carries `{ hash, len }`; the bytes live in `Preimages` under `blake2_256`; `execute` verifies hash/length/SCALE-decode before dispatch |
| Hand-set weights | `WeightInfo = GpnWeight<Runtime>`; `propose` is metered per byte (500 KB ≈ 5 ms ref-time), `propose_preimage` likewise; TODO(bench) to replace with `frame-omni-bencher` output |
| Storage migration v0 → v1 | `Proposals` values re-encoded from the old struct into `ProposalCall::Inline(..)`, ids and approvals preserved; `ProposalIds` index rebuilt; `Members` seeded; `MemberCount` recomputed; `StorageVersion` 0 → 1 |

### 7.1 Still missing after spec 42

1. **Benchmarked weights.** The numbers in `weights.rs` are documented bounds, not measurements.
2. **Genesis committee.** Mainnet genesis still seeds `testnet_committee()` (three addresses).
   The five committee accounts must be in the genesis chain spec **before** mainnet is cut —
   that is a genesis decision and is *not* part of this staged upgrade (the migration only fixes
   a chain that is already running).
3. **Committee key custody.** committee-4 and committee-5 were generated 2026-10-21 and stored
   in `governance-keys.txt` (mode 600) on the Linux box. **Move them to cold storage before
   mainnet.**

## 7.2 Spec 43 — symmetric difficulty retarget + pallet-side optimistic claim (staged, not applied)

Built, tested and staged on top of the spec-42 source; **not** enacted. Plan, review checklist,
rollback and the exact propose → approve → execute commands:
`shared/artifacts/project7/staged/retarget-clamp-v1.md`. Evidence:
`shared/artifacts/project7/analysis/retarget-sim-2026-10-22.md` (replay of 2 500 real blocks
through the old and the new controller, plus adversarial burst/pause/hashrate-step patterns).

| Change | Effect |
|---|---|
| Retarget law `2·actual/(actual+target)` → `clamp(actual/target, 100/133, 133/100)` | the old law was **asymmetric** (could relax <2× per window but tighten without bound), so a fast burst collapsed the target (−50.6 % measured) and recovery took several windows (+37 % each). The new bounds are exact reciprocals: one reciprocal window undoes one window of error. |
| `DifficultyAdjustmentWindow` 10 → 45 blocks | window mean noise CV = 1/√n: 32 % → 15 %; a single 90 s stall moves the target 1.11× instead of ~1.5× |
| `Config::MaxRetargetStepPercent = 133` | one constant controls the slew limit; `100` pins the target (a clean stop-retargeting rollback) |
| `apply_difficulty_ratio` saturates on limb overflow | previously wrapped to a *smaller* target — an accidental hardening the max-bound check could not catch |
| `compute-marketplace::reserve_optimistic_claim` / `release_optimistic_claim` (call 15/16), `OptimisticClaims` storage, `claim_reap` in `on_initialize` | a provider that sees a create in the pool pre-books `NextJobId`; `create_job` attaches it in the **inclusion block**, removing one full block wait. `ttl` ≤ `MaxOptimisticClaimTtl` (10), reaped every block, double-claim rejected, eligibility re-checked at inclusion, and a claim never fails or delays a create |
| `submit_result` accepts `Accepted` (assigned provider only) | an attached job needs no separate `start_job`; a third party still cannot deliver and the requestor still cannot fail a `Running` job (red-team #12) |
| `spec_version = 43` | required by `set_code` |

Artifact: `wasm/gpn_runtime_spec43_retarget_clamp.compact.compressed.wasm` — 519 286 B,
`sha256 5604f140a935c3758c0a4d56a754deae2c255661468a66002f7161d74d3d10cd`,
code hash `0x43b6777f7b31988827493b8ea6813f7b938ef3c52b2918c0bcadf2dd3ae8361c`,
`spec_version = 43`, built twice byte-identically. Rebuild: `cd gpn-node && bash
scripts/build-retarget-clamp-wasm.sh` (refuses to run unless the canonical genesis WASM is
`76f37b23…16fa97`, and restores it hash-verified).

**Honest SLO.** Measured block time with the current controller: p50 11.4 s, p95 51.7 s, p99
90.7 s, max 154 s. Simulated with the proposed controller: p50 9.6 s, p95 51.8 s, p99 97.9 s,
max 239 s under the same draws — and the *floor* (target pinned perfectly) is p95 48 s. A
create still pays one block wait, so the achievable e2e p50 is ~12–15 s. **5 s e2e is not
reachable at a 15 s target**; it needs a shorter target block time and/or more sealer capacity.

**Ordering vs spec 42.** This artifact contains the spec-42 code, so enacting it delivers both.
Before spec 42 enacts, `propose --mode inline` is the only path (the preimage path does not
exist on the live runtime). If the committee prefers one change at a time, enact spec 42 first
and rebuild this artifact afterwards with `spec_version = 44` — never circulate two artifacts
claiming the same `spec_version`.

## 8. Rehearsal (what was actually run)

Two halves, because the timelock is a runtime constant:

```bash
# (a) live chain: stage → propose → approve → status → execute-refused → verify-pending
bash scripts/gov-upgrade-rehearsal.sh

# (b) enactment on the real Runtime (spec 42):
#     propose → approve → timelock → execute(Root) → :code + CodeUpdated,
#     plus expiry, cancel, preimage enactment and the v0→v1 migration.
cd gpn-node && cargo test -p gpn-runtime --test gov_upgrade_rehearsal     # 10 tests
cd gpn-node && cargo test -p pallet-gpn-governance                        # 22 tests (incl. weights)
```

(a) runs on a throwaway `--chain dev` node (own base path/ports, genesis hash unrelated to
testnet). Result: a 502 597-byte compressed artifact proposed on chain, 2/2 approvals, timelock
pending, `execute` refused, `verify` reporting the on-chain `:code` hash (spec 6) and timing out
with a diagnosis.

(b) runs the real `Runtime` in test externalities and fast-forwards the block number to the
exact boundaries. Spec 42 coverage:

- 3 of 5 approvals; at `created_at + 17 279` execution is refused and the proposal survives; at
  `+ 17 280` any account's `execute` writes `:code == proposed bytes` and emits
  `System::CodeUpdated { hash: blake2_256(:code) }`;
- expiry: execution succeeds at exactly `created_at + 14 d` and fails with `ProposalExpired` one
  block later, then `on_initialize` prunes the entry;
- `cancel` refuses at 2 approvals, succeeds at 3, and keeps `ProposalIds` in sync;
- preimage: a real ~512 KB `set_code` call is noted by hash, `peek` returns the exact bytes
  before the timelock, and execution installs that artifact; a missing preimage is recoverable
  (`PreimageMissing`, proposal intact, re-note + execute);
- migration: a hand-written v0 proposal is preserved across `on_runtime_upgrade` with its
  approvals, the `ProposalIds` index is rebuilt, membership becomes 5, and a second run is a
  no-op.

Why not on a live chain end-to-end? `TimelockPeriod` is 3 days of **real** blocks and cannot be
shortened without a runtime build. Faking `System.Number` via `System.set_storage` trips
frame_system's `Block number must be strictly increasing` assertion and halts the chain — tried,
observed, documented here so nobody repeats it.
