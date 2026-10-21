# Runtime upgrades on mainnet — the governance pipeline

**Audience:** the GPN committee (proposers/approvers), the launch operator, and anyone
verifying an upgrade happened correctly. **Status:** tool + runbook implemented and rehearsed;
no runtime change has been applied by this work.

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

| Piece | Value on mainnet | Where it comes from |
|---|---|---|
| Pallet | `GpnGovernance` (`pallet-gpn-governance`) | `gpn-node/runtime/src/lib.rs` |
| Members | the **committee-3** (`testnet_committee()`), seeded at genesis | `node/src/chain_spec.rs` |
| Approvals required | **2** (`ApprovalsRequired`) | `GovernanceApprovalsRequired` |
| Timelock | **17 280 blocks** ≈ 3 days at 15 s (`TimelockPeriod`) | `GovernanceTimelockPeriod = 3 * DAYS` (`DAYS = 5 760`) |
| Max members | 8 (`MaxMembers`) | `GovernanceMaxMembers` |
| Proposal id | `blake2_256(SCALE(RuntimeCall))` | `T::Hashing::hash_of(call)` |
| Executor | **anyone** (`execute` takes a signed origin, not a member origin) | pallet call 2 |
| Root dispatch | `call.dispatch(RawOrigin::Root)` | pallet call 2 |

Live check (testnet, 2026-10-21):

```
committee  3 member(s), 2 approval(s) required, max 8
members    5HeUVTK6zd9...5JqYej5h  5Cd5NiY2qkG...pGUvEX3Dx  5HTWBtWro5A...3PJRteWN
timelock   17280 blocks (~3d 0h at 15 s/block)
```

The 25-address roster with the **13-of-25** threshold is `pallet-gpn-treasury`. It governs
*spending*, **not** consensus. A treasury majority cannot change the runtime, and the committee
cannot spend treasury funds.

### 2.2 What does **not** exist (and therefore cannot be used)

The runtime contains **no** `pallet-democracy`, `pallet-collective`, `pallet-preimage`,
`pallet-scheduler`, or FRAME `pallet-treasury`. Consequences, stated plainly:

- **No referenda, no conviction voting, no tracks.** There is no stake-weighted vote and no
  "aye/nay" phase. The committee decides; the chain only enforces the 2-of-N + timelock rule.
- **No technical-committee fast-track** and **no emergency track**. The timelock is a runtime
  constant; it cannot be shortened by any call, including a root call, without a runtime
  upgrade (chicken and egg).
- **No preimage indirection.** `propose(call: Box<RuntimeCall>)` stores the **whole call
  inline** in `GpnGovernance::Proposals`, so a `set_code` proposal puts the **full WASM in the
  `propose` extrinsic** and keeps ~500 KB (compressed artifact) of state until execution. See §7
  for the staged fix.
- **No veto / cancel.** There is no extrinsic that removes a pending proposal. It disappears
  only when `execute` succeeds (removed *before* dispatch, so a failing dispatch also consumes
  it) or by a root `System.set_storage`/`kill_storage` intervention — which itself needs
  governance. Operationally: **approve deliberately, because you cannot take it back** beyond
  refusing to reach 2 approvals for 3 days and watching the timelock.
- **No proposal expiry.** An approval-less proposal sits in state forever (state bloat; cleanup
  needs a runtime change).

### 2.3 The upgrade origin, in one line

> `GpnGovernance.execute(hash)` → `RawOrigin::Root` → `System.set_code { code }`.
> 2 distinct committee approvals + 17 280 blocks of public notice, and no single key can act.

## 3. Security invariants (non-negotiable)

1. **Never a raw WASM in a public call.** With today's runtime this invariant *cannot* be
   satisfied for `set_code` (no preimage pallet, §7) — so it is enforced socially: the committee
   **must** verify the artifact hash out-of-band and compare it with `stage`'s output before
   proposing. `stage` prints `sha256` (release identity) and `blake2_256` (the code hash).
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
| `propose` | committee member A | A's key (proposer self-approves → 1/2) |
| `approve` | committee member B | B's key (→ 2/2) |
| wait | everybody | — (public timelock, 17 280 blocks) |
| `execute` | **anyone** (an ops bot is fine) | any funded key (execution is permissionless) |
| `verify` | whoever executed + a second, independent operator | — |
| rollback decision | committee (2 of 3) | A + B again |

Nothing here requires the members to be online at the same time except that both must sign
before the timelock-starting proposal is useful (the clock starts at `propose`, not at 2/2 —
so **secure approval 2 promptly**, otherwise the 3-day window is partly wasted).

## 5. The runbook

All commands are `gpn-agent/target/release/gpn_gov_upgrade` (build with
`cargo build --release --bin gpn_gov_upgrade`), `E=ws://<unsafe-rpc-node>:9944`.

### 5.1 T-7d — build and stage the artifact

Build the runtime **alone** (never the canonical bundled artifact):

```bash
cd gpn-node
cargo build -p gpn-runtime --release --features std
cp target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm \
   ../wasm/gpn_runtime_spec42.compact.compressed.wasm
sha256sum ../wasm/gpn_runtime_spec42.compact.compressed.wasm
```

`build.rs` embeds the committed canonical WASM when it is present. To produce a **new** runtime
you must build in a tree without it (or with `GPN_WASM_BUILD=1`-style override in a scratch
copy). Verify what you built, not what you intended:

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

```bash
gpn_gov_upgrade propose --plan .state/gov-upgrade/spec42_staged<…>.plan.json --signer <A-uri>
```

Prints the **proposal hash** = `blake2_256(SCALE(System.set_code{code}))`. That hash is the id
everything else uses; publish it (with `sha256` and `code hash`) on the ops channel.

Re-running `propose` with identical bytes is a **no-op** (the pallet rejects duplicates; the
tool detects the existing entry and exits 0) — safe to retry after a failed submission.

### 5.3 T-3d — approve (member B)

```bash
gpn_gov_upgrade approve 0x<proposal-hash> --signer <B-uri>
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

- a competing proposal that reaches 2/2 first (it would upgrade first — `verify` detects the
  code-hash mismatch and tells you to re-stage against the new version);
- any sign that members' keys are compromised (there is **no cancel**; the only defense is not
  reaching the threshold, then re-proposing after rotating the committee via a *separate*
  governed upgrade).

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
| `execute` → `ExecutionFailed` | Root call reverted (bad WASM, `SpecVersionNeedsToIncrease`) | proposal is **consumed**; fix the artifact and re-propose |
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

**What you cannot do:** cancel a pending proposal, shorten the timelock, or "undo" an execution.
Everything else is a new governance action.

## 7. What is still missing for mainnet (staged, not applied)

1. **Hash-addressed proposals (`pallet-preimage`).** Today `set_code` cannot be proposed by code
   hash, so ~500 KB of WASM sits in a public extrinsic and in state. Design + exact wiring:
   `design/design-gov-preimage-proposals.md`. Requires a spec-42 build and a vote — *do not*
   hand-apply it.
2. **A veto/cancel path and proposal expiry** in `pallet-gpn-governance` (today a proposal can
   only disappear by executing it). Same staged design.
3. **`WeightInfo` for the governance pallet is `()` (all weights are zero)** — `propose` with a
   500 KB call is unmetered. Acceptable on a PoW solochain with no block-weight PoV, but it
   should be benchmarked before the committee gets busy.
4. **Committee composition.** Mainnet inherits `testnet_committee()` — three ss58 addresses.
   Decide whether the production committee differs (that is itself a genesis decision, so
   settle it **before** the genesis is cut).

## 8. Rehearsal (what was actually run)

Two halves, because the timelock is a runtime constant:

```bash
# (a) live chain: stage → propose → approve → status → execute-refused → verify-pending
bash scripts/gov-upgrade-rehearsal.sh

# (b) enactment: propose → approve → timelock → execute(Root) → :code + CodeUpdated
cd gpn-node && cargo test -p gpn-runtime --test gov_upgrade_rehearsal
```

(a) runs on a throwaway `--chain dev` node (own base path/ports, genesis hash unrelated to
testnet). Result: a 502 597-byte compressed artifact proposed on chain, 2/2 approvals, timelock
pending, `execute` refused, `verify` reporting the on-chain `:code` hash (spec 6) and timing out
with a diagnosis.

(b) runs the real `Runtime` in test externalities and fast-forwards the block number to the
exact timelock boundary: at `created_at + 17 279` execution is refused and the proposal
survives; at `+ 17 280` any account's `execute` writes `:code == proposed bytes` and emits
`System::CodeUpdated { hash: blake2_256(:code) }`; a second proposal replaces the first (the
rollback mechanism).

Why not on a live chain end-to-end? `TimelockPeriod` is 3 days of **real** blocks and cannot be
shortened without a runtime build. Faking `System.Number` via `System.set_storage` trips
frame_system's `Block number must be strictly increasing` assertion and halts the chain — tried,
observed, documented here so nobody repeats it.
