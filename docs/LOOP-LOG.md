# LOOP-LOG

Newest first. Written by the release loop; read by the assistant, who relays anything
that needs Jeremy. `[TRIAGE]` = a decision waiting on the assistant.

## 05:26 PT — 0.3.65 box 8 (row 228): eighth rig ran the whole path and still no import decision; wrapper instrumented, ninth rig dispatched [no triage]

- A0: no CI verdict read this run for the pushed fix `8b5b718` (build in flight/queued); not blocked on it — this box is a rig proof, not a build.
- Eighth rig (BIN `-v8`, 13:01:14Z–13:20:18Z), A.log verbatim: `05:17:15.717 fork-choice: … reorg=accepted depth=49`; `05:17:15.721 block-serving: asking … 180 blocks behind our tip #229 — fork choice accepted its heavier branch`; `05:17:15.742 downloading the accepted heavier branch … 180 blocks … 2 chunk(s)`; then 180 `🆕 Imported #91..#270` in 0.3 s with `best: #229` unchanged. **Zero `import: the branch at` lines.**
- The guard is silent, so two causes remain and cannot be separated from it: (a) `WorkFirstImport` is not in the sync import path, (b) `branch_work`/`chain_work` returned `None`. Neither is a guessable fact — instrumented instead.
- **Instrumented 05:25 PT**: one `import: fork block #N (parent …, best #M) — branch_work=… chain_work=… (#228)` line per fork block. fmt clean; release build 05:25 PT; `strings` count 1. The next log answers (a) vs (b) vs (c) "measured lighter" directly.
- **Ninth rig dispatched 13:20:43Z** — `BIN=/tmp/row228/gpn-node-ea10413-v9`, phase 3 ~05:36 PT. Not waited on. Box 228 stays unticked.
- No blocker, no decision needed.

## 05:02 PT — 0.3.65 box 8 (row 228): seventh rig proved the gate + download; the import wrapper never ran because the SDK had already set fork_choice; fixed, eighth rig dispatched [no triage]

- A0: no CI run existed for `origin/main` HEAD `ea104134…f949` at run start (`total_count: 0`); nothing waited on. Code fix pushed as `71d60b3`, which started a build for the next run to read.
- Seventh rig (BIN `-v7`, 12:37:21Z–12:57:53Z) **construction valid**: `B is shorter by 47 and heavier by 833637`. A.log verbatim:
  - `04:54:50.644 fork-choice: … reorg=accepted depth=47`
  - `04:54:50.644 block-serving: asking 12D3KooWJWNx… for blocks 169 behind our tip #216 — fork choice accepted its heavier branch … (#228)`
  - `04:54:50.649 block-serving: downloading the accepted heavier branch … — 169 blocks … in 2 chunk(s) of at most 128 (#228)`
  - 78 `🆕 Imported #92..#169` in 0.3 s, **all non-best**; `best: #216 (0xa49f…)` never moved; A mined `🏆 #217..#238` on its own chain; B followed A. Zero `import: the branch at … carries work` lines.
- Cause (SDK, quoted): `sc-consensus-pow/src/lib.rs:378` — `PowBlockImport::import_block` sets `fork_choice = Some(Custom(target-sum comparison))` **before** calling the inner importer. Our `WorkFirstImport` is inner, so `if block.fork_choice.is_none()` was never true. Our bug (wrong assumption about wrapper position).
- Fix (compiled, dispatched): decide on the fork case unconditionally — parent ≠ our best ⇒ measure `branch_work` and force `Custom(true)` only when heavier (`service::force_branch_best`); extensions and lighter forks unchanged. `cargo test --release --bin gpn-node force_branch_best` → **3 passed**; fmt clean; release build 05:00 PT; `strings` count 1 for *importing it as best*.
- **Eighth rig dispatched 13:01:14Z** — `BIN=/tmp/row228/gpn-node-ea10413-v8`, log `/tmp/row228/rig/rig.log`, phase 3 ~05:15 PT. Not waited on. Box 228 stays unticked until `import: the branch at #… carries work … vs our best` + `🏆 Imported #` off B's branch + matching `chain_getBlockHash`.
- No blocker, no decision needed.

## 04:38 PT — 0.3.65 box 8 (row 228): sixth rig proved the accept is cleared by the peer's next claim; verdict lifetime fixed, seventh rig dispatched [no triage]

- A0: still `total_count: 0` for `origin/main` HEAD `ea104134…f949` (docs-only commit, `paths-ignore`). Nothing to read, nothing waited on.
- Sixth rig (BIN `-v6`, 12:13:03Z, ended 12:32:00Z) **construction valid**: `A #218 work=342,490 vs B #169 work=9,184,522` (depth 49). A.log verbatim:
  - `04:28:57.790 block-serving: not asking … its claim (tip #169) is not on our canonical chain … it is a fork and never a source of truth (#158)`
  - `04:28:57.791 fork-choice: work ours=342490 theirs=9184522 …; reorg=accepted depth=49`
  - then **no** `asking … fork choice accepted its heavier branch` and **no** `downloading the accepted heavier branch`; A stayed `best: #218 (0xd7d1…)`.
- Cause (read from the code, not guessed): `set_accepted_fork(peer, matches!(decision, Reorg))` cleared the verdict on B's *next* claim. B runs `GPN_STANDBY=1` so it never mines, but it still announces; once it synced A's `#218` its claim evaluated to `using as sync source` at 04:29:26, closing the gate before the sync engine's next request. The 03:17 race was lifetime, not a one-tick ordering bug.
- Fix (compiled, dispatched): `service::next_accepted_fork(current, reorg, peer_tip_held)` — opens on Reorg, stays open while the peer's announced tip is not a block we hold, closes only when that tip is ours (imported) or the link closes. `cargo test --release --bin gpn-node accepted_fork` → **4 passed**; `cargo fmt --all -- --check` clean; release build 12:37Z; `strings` count 1 for *downloading the accepted heavier branch*.
- **Seventh rig dispatched 12:37:21Z** — `BIN=/tmp/row228/gpn-node-ea10413-v7`, log `/tmp/row228/rig/rig.log`, phase 3 ~12:50Z. Not waited on. Box 228 stays unticked until A.log shows `downloading the accepted heavier branch` + `🏆 Imported #` off B's branch + matching `chain_getBlockHash`.
- No blocker, no decision needed.

## 04:13 PT — 0.3.65 box 8 (row 228): fifth rig proved the download and exposed the real remaining bug — the SDK's target-sum fork choice imports a heavier branch as non-best; fixed, sixth rig dispatched [no triage]

- A0: no CI run for `origin/main` HEAD `ea104134…f949` (`total_count: 0` — docs-only, `paths-ignore`). Nothing read, nothing waited on.
- Fifth rig (BIN `-v5`, 11:45:22Z) **construction valid**: `--- CONSTRUCTION OK: B is shorter by 55 and heavier by 782849` (A #224 work=348,877 vs B #169 work=1,171,326, depth 55). A.log, verbatim:
  - `04:01:21.269 fork-choice: work ours=371112 theirs=10785120 peer=12D3KooWMVsz…; reorg=accepted depth=55`
  - `04:01:21.274 block-serving: downloading the accepted heavier branch from 12D3KooWMVsz… — 169 blocks from the branch tip in 2 chunk(s) of at most 128 (#228)`
  - `04:01:22.510 … 🆕 Imported #91 (0x183c…cb35 → 0xa243…4e9c)` … `04:01:22.884 … 🆕 Imported #169 (… → 0x6f6c…2ae3)`
- **The download half is proven** (79 new blocks in 0.4 s, exactly the paginated walk). **The import half fails**: every block landed 🆕 (non-best), A kept `best #224 (0x4026…e352)` and mined its own `🏆 #225` on top of it, then B (standby, never mined) followed A — `chain-state: peer … is ahead at #224 (ours #169, same genesis) — syncing from it`. Both tips SAME at #256 on **A's** chain. No reorg; the lighter chain won. Box 228 stays unticked.
- Cause, read in the SDK (`client/consensus/pow/src/lib.rs:376-390`): `PowBlockImport` sets `fork_choice = Custom(aux.total_difficulty > best_aux.total_difficulty)`, where `total_difficulty` is the **sum of `PowAlgorithm::Difficulty`**. Our `GPNPowAlgorithm::Difficulty` is the pallet **target**, and a target is an inverse difficulty — B's harder branch sums *lower* (10.7M vs 371k here is the pallet target, and the SDK's own sum inverts), so the SDK says “not heavier”. Our choice of `Difficulty` meaning, so our bug.
- Fix (local, compiled, not pushed): `service::branch_work` — cumulative work over an arbitrary branch's last `WORK_WINDOW`=512 blocks walking parent hashes (same accounting as `chain_work`; 288 `MAX_REORG_DEPTH` inside the window); `service::WorkFirstImport` — wraps the PoW importer and, only when `fork_choice.is_none()` and the block's parent is not our best (a true fork), sets `Custom(true)` when our work accounting says the branch is heavier. Normal extensions and lighter forks untouched.
- Build: `cargo fmt --all --check` clean; `cargo build --release --bin gpn-node` → `Finished … in 55.82s`, binary copied to `/tmp/row228/gpn-node-ea10413-v6`, `strings` count 1 for *"importing it as best, not as a side block"*.
- **Sixth rig dispatched 12:13:03Z** (`/tmp/row228/rig/rig.log`), not waited on. Next run: phase-3 verdict is `import: the branch at #… carries work … vs our best` + `🏆 Imported #` rows descending from B's tip + matching `chain_getBlockHash`. Only then is 228 ticked.
- No blocker, no decision needed.

## 03:47 PT — 0.3.65 box 8 (row 228): fifth rig run dispatched with B in standby, debug gate logging and a 5-minute window [no triage]

- A0: no CI run exists for `origin/main` HEAD `ea1041340912ce3bd250628d4155e2f47fc0f949`
  (`total_count 0` — docs-only HEAD, `paths-ignore` working). Nothing to read, nothing waited on.
- Two causes were eliminated by reading the code, not by guessing:
  - the fourth run's accept verdict was overwritten 29 s later because **B kept mining after the
    phase-3 restart** (`03:18:04 ahead at #271 → using as sync source`) which clears
    `accepted_fork`. The plan recorded in the box (SIGSTOP B through phase 3) was wrong: a stopped
    node cannot serve the branch it is the only source for. B now starts with `GPN_STANDBY=1`
    (`service::standby_requested` → full node: serves + syncs, never mines), so the branch is fixed.
  - the log could not have shown the download even if it happened: `should_ask` logged once per
    peer per **process**, so a peer refused a tick before the accept never printed the accepted
    line (exactly the 03:17:35 pair). `explained` now holds the last `Verdict` per peer and logs
    every transition, and `download_accepted_fork` logs the paginated walk it starts.
- Evidence for the change: `cargo test --release --bin gpn-node block_serving::tests` → **21
  passed** (20 before; +`the_verdict_names_the_accepted_branch_first`), `cargo fmt --all -- --check`
  clean; release binary rebuilt into `/tmp/row228/gpn-node-ea10413-v5`, `strings` confirms both
  lines are in it.
- Rig dispatched 11:45:22Z, 300 s observation window, phase 3 expected ~04:03 PT. **Not waited on.**
  Next run reads `/tmp/row228/rig/rig.log` for `downloading the accepted heavier branch` + `Imported
  #` rows descending from B's tip + matching `chain_getBlockHash`.
- Box 228 stays unticked. Script + checklist edits local-only until the release commit.

## 03:28 PT — 0.3.65 box 8 (row 228): construction now valid (depth 47, B 3x heavier), `reorg=accepted depth=47` logged, but the branch is still never imported — box NOT ticked [no triage]

- Root cause of the three invalid rig runs, found and fixed: **the node mines on the GPU**, so
  `taskset` never changed either branch's hashrate. A is now forced onto the CPU light-cache miner
  (`OCL_ICD_VENDORS=/tmp/row228/no-icd`), B keeps the RTX 4060, and B is frozen with `SIGSTOP` after
  74 blocks past the split.
- `--- CONSTRUCTION OK: B is shorter by 47 and heavier by 761832` (A #217 work=381,100;
  B #170 work=1,142,932). Depth 47 is inside the 33–288 band the box is about.
- A.log 03:17:35.486, verbatim:
  `fork-choice: work ours=387832 theirs=11543559 peer=12D3KooWJ5i…; reorg=accepted depth=47`
  and `fork-choice: requesting heavier branch from 12D3KooWJ5i… at #170 (provable depth 47)`.
- **The verdict is logged; the download is not.** A's next 21 imports (#218..#238) all descend from
  A's own #217 `0x299a…22ff`; the rig's hashtable shows #232..#238 **DIFF** at every height. No
  `block-serving: asking … fork choice accepted its heavier branch` line exists; the *same
  millisecond* carries the old `not asking … it is a fork and never a source of truth` refusal.
- Two mechanical suspects: (1) `should_ask` reads `accepted_fork` before the chain-state task
  publishes the Reorg verdict, and its one-explanation-per-peer latch then hides every later ask;
  (2) verdict churn — B kept mining through phase 3 and by 03:18:04 was `ahead at #271 … using as
  sync source`, which clears `accepted_fork`.
- Next run: hold B stopped through phase 3 (stable branch, no churn), `block_serving=debug` on A,
  observe 5+ minutes. If the refusal still precedes the accept within one tick, the fix belongs in
  `block_serving::should_ask` (read the verdict after it is published / clear the latch when
  `accepted_fork` flips) — code, not rig.
- Rig script carried the same fix to `scripts/row228-fork-rig.sh` (local only, not pushed).
- Box 228 unticked. No blocker, no decision needed.

## 02:29 PT — 0.3.65 box 8 (row 228): rig rebuilt (SIGSTOP freeze, disjoint cores) and re-dispatched; run ends, log is the state [no triage]

- A0: no CI run exists for main's HEAD `ea104134…f949` — that commit is docs-only (`paths-ignore`), so
  nothing to read and nothing to wait on. Confirmed with the runs API (`total_count: 0`, full sha).
- Root cause of the 01:59 invalid test is in the rig, and it is mechanical: the "pause B until A pulls
  ahead" branch **killed and restarted** B, and every restart gave B a window of slow blocks that the
  retarget read as *too slow* and eased — so B could never end heavier. Fixed: `SIGSTOP`/`SIGCONT`
  (chain and difficulty frozen exactly), A on cores 0-1 and B on 2-11 (disjoint, 12-core box), and a
  hard validity gate (`CONSTRUCTION FAILED`, exit 3) so an invalid test can no longer be staged as
  evidence. Script edit is local only (not pushed while 0.3.65 is in flight).
- Handle: `BIN=/tmp/row228/gpn-node-a45d77c` (fix string present, `strings` count 1), log
  `/tmp/row228/rig/rig.log`, started 10:21:32Z, expected phase 3 ~10:55Z. **Not waited on.**
- Rig env note for row 229: `/tmp/row228/raw.json` embeds an old runtime wasm — `ADJUST` lines say
  `blocks=10 target=4`, not the repo's `45`/`15`. Node-code fix is unaffected; future rigs wanting the
  shipping constants must `build-spec --raw` from the released binary.
- Known cosmetic rig bug (next run): `jid` is sampled 10 s after launch, before RPC is up, so the
  witness connect fails with `invalid multihash` and phase 1 logs `NO PEER on A`. Mining is unaffected
  (A reached #45 with 0 peers); phase 3 reads ids from the logs and is unaffected. Fix = poll
  `system_localPeerId` until non-empty.
- Box 228 still unticked. No blocker, no decision needed.

## 02:20 PT — 0.3.65 box 8 (row 228): rig finished, its own numbers say the test was INVALID; box not ticked [no triage]

- The 01:59 rig completed (09:59Z–10:11Z). Connection snapshot:
  `A=#227 work=1,374,645,396  B=#177 work=288,645,810  depth=50` — **B was lighter, not heavier**
  (0.29e9 vs 1.37e9), so fork choice correctly kept A's chain. `A.log` has **no `fork-choice:` line**;
  it has `not using as sync source (FORK…)` at 02:09:56 and `fork-tolerance: … their tip 0x7a71e30d is
  behind on a branch we do not have` at 02:10:25. A's 20 imports (#228..#247) form a continuous parent
  chain from A's own `#227 0x8eee…78fe` — A's own mining, not a download.
- The SAME hashes at #241..#247 are **B following A**: B imported `#227..#248`, and its last
  `Imported #248` (02:11:53) has parent `0x613c…a2d3` = A's `#247`. A depth-71 adoption did happen —
  on the taller/heavier side, i.e. the ordinary sync path. That is not the 33–288 band the box is about.
- **Cause is the rig's economics, not the node**: both branches start at the same difficulty at the
  split, so block count dominates; A on 2 cores still accumulated 3.3x B's work per block. A work gate
  cannot fix that (B needs ~5x the work while staying 40 blocks shorter) — next attempt needs B on a
  genuinely harder chain spec, not retarget drift.
- Added a **validity gate** to `scripts/row228-fork-rig.sh`: if `work(B) <= work(A)` at connect it aborts
  (`exit 6`) instead of producing a false pass. `bash -n` clean. Unpushed.
- Box 228 **stays unticked** — there is no `reorg=accepted` line and none is claimed.

## 01:59 PT — 0.3.65 box 8 (row 228): the 01:45 rig's failure diagnosed (our rig, not the node); live proof re-dispatched and the run ended [no triage]

- **A0**: HEAD/origin-main `a45d77c`; `actions/runs?head_sha=<full>` → **1 run (`CI` 44891227910), in_progress**
  → noted, continued (box 228 is code, not a build/publish step). That run is currently rebuilding
  `.ci-cache/gpn-node-target`, which is why the rig binary was frozen for the re-run.
- **Box worked: §3b 228** — unit evidence stayed green (commit `74f6d98`); the 01:45 live rig failed and was
  **diagnosed from its own logs, not re-run blind**. `A.log`/`B.log` show both nodes talking to a
  **different genesis** peer (`0xb8e4636a`, live chain best #338k) and A with no peer at all: phase 2's
  "B too tall" branch restarts B, the main loop broke on A's `#221` during B's boot window, so the
  phase-3 snapshot read `B=#-1`, `B.log` had no `Local node identity is` line, `PEERB` was empty, and both
  nodes were restarted with an **empty reserved peer id** → the documented public-oracle-seed fallback
  (`rig.log` 01:56:22, `A id=12D3KooWRMd…  B id=`).
- **Fix (rig, not node)**: `wait_height 9983 1 120` after the pause restart; `grep -a` for identities
  (the logs contain non-UTF8); phase 3 **exits 3** on a blank id instead of starting blind. `bash -n` clean.
- **Re-dispatched** 01:59 PT under `setsid`, `BIN=/tmp/row228/gpn-node-a45d77c` (a frozen copy of the HEAD
  build; `strings` shows the fix's own line *"fork choice accepted its heavier branch"*, so the fix is
  in the binary that is running). Handle: `/tmp/row228/rig/rig.log` (rig start 2026-11-21T09:59:19Z).
  **Run ended without waiting** (the rig takes ~11 min; waiting is what timed out three earlier runs).
- **Box 228 stays unticked** — needs `reorg=accepted depth=<40..250>` + `Imported #` in that log.
- Pushed `b8ec8f7` (docs only, no CI). The rig-script edit is local-only and goes with the release commit;
  pushing a `scripts/*.sh` change now would cancel the in-flight CI run for this SHA.

## 02:45 PT — 0.3.65 box 8 (row 228): the gate and the 128-block window are fixed, unit-proven; live rig running [no triage]

- **A0**: `GET /commits/main` + `rev-parse HEAD` both `5f77f56` (clean); `actions/runs?head_sha=<full>` →
  **1 run, `CI` in_progress** (08:26Z) → per the revised rule, noted and continued (this box is code,
  not a build/publish step).
- **Box worked: §3b 228** (reorg band 33–288 dead). Found and fixed **two** causes in our own code,
  commit **`74f6d98`**:
  1. **the gate** — `PeerTips` gains `accepted_fork`, written by the chain-state task on
     `Decision::Reorg` and cleared on any other decision / disconnect;
     `may_ask_for_blocks_anchored()` takes it as its one exception. Before this, the peer holding the
     branch fork choice had just accepted was refused twice over (anchor mismatch set for every
     `Use::Fork` peer, plus `PEER_BEHIND_MARGIN` = 32).
  2. **the 128-block response window** — `chain_sync` asks for an accepted fork as ONE descending
     request from the branch tip to our last finalized block, and a peer answers at most
     `MAX_BLOCKS_IN_RESPONSE` (128). Beyond that the batch never reaches our chain, nothing imports
     and the engine re-asks the *identical* request until the repeat cap refuses it — the exact line
     the 2026-11-20 run captured at depth 151. `PoliteDownloader::download_accepted_fork` now walks
     the branch down 128 blocks at a time and returns one response that reaches our chain.
- **Evidence (unit)**: `cargo test --bin gpn-node block_serving::tests` → **20 passed** (two new:
  200-block branch fetched as 128+72 chunks into one response spanning #200..#1; an unaccepted branch
  still refused with nothing on the wire). fmt clean; clippy with CI's `CLIPPY_ALLOWS -D warnings`
  **exit 0**.
- **Live rig (running, this is the box's outstanding half)**: `scripts/row228-fork-rig.sh` — shared
  prefix → data-dir copy → A throttled to 2 cores (difficulty *eases*) vs B on every core (difficulty
  *hardens*) so B is **shorter but heavier** → connect and watch A. Log: `/tmp/row228/rig/rig.log`.
  The box stays **unticked** until that log shows `reorg=accepted depth=40..250`, `Imported #…` and
  A's `chain_getBlockHash` equal to B's at the shared heights.
- **Two hours went into rig plumbing, both lessons recorded in the checklist note and worth carrying**:
  `--node-key-file` wants **raw 32 bytes** (hex text fails `failed to parse Ed25519 secret key`);
  a loopback `--reserved-nodes` peer connects but the peer-watch pauses sealing at
  `sync_num_connected()==0`, and a node with an **empty** reserved set adopts the public oracle seeds,
  whose `different chain` error poisons the rig's sync — so both ends need a reserved entry.
- **Bonus measurement for row 229** (while building the rig): the chain spec generated by the
  published 0.3.62 binary, run on this host, logs
  `ADJUST now=21 last_adj=11 elapsed_s=10 blocks=10 avg_s=1 target=4 ... 288230376151711744 ->
  115292150460684697` — i.e. **window 10, target 4 s, and a 2.5x step**, exactly the shipped
  constants row 229 describes, while repo HEAD's `runtime/src/lib.rs` says 45 / 15 s / 1.33. The
  constants are *runtime* state carried by the chain's genesis WASM, so this is consistent with
  "the artifact embedded a different WASM than HEAD's source" and with the node never replacing the
  runtime (no `set_code`). Not concluded here — row 229 stays open, but the next run has a
  reproducible witness: build a spec with the published artifact and read the first `ADJUST` line.
- No blockers, no `[TRIAGE]` items. Next run: read the rig log; if the reorg is there, tick 228 with
  the lines; if B never out-works A, raise B's height target (`A_TARGET`/`B_MARGIN` are env knobs).

## 00:40 PT — 0.3.65 box 7 (row 227) DONE: deterministic airdrop export [no triage]

- **A0**: `GET /commits/main` → **`be56bd0`** (loop-log) → `actions/runs?head_sha=be56bd0…` →
  **0 runs** (docs-only, `paths-ignore`) as expected. Checkout was 2 commits behind again;
  fast-forwarded to `be56bd0` before reading the checklist.
- **Box ticked — §3b 227** (`gpn-referrals --export [<file>]`, schema `gpn-airdrop-input-v1`).
  - Document carries `registry_sha256`, `totals`, `participants[]` (every entry, with `credited`),
    `referrers[]` and `self_referrals[]`; contract written in `docs/design-referral-codes.md` §7c.
  - Three storage changes were needed to make the contract honest: `Entry.last_seen` (serde-default,
    refreshed in place on a repeat sighting; a pre-227 registry reads it as `first_seen`),
    self-referrals recorded in a **sidecar** (`referral-self-referrals.json`) instead of being
    silently dropped — so they are evidence yet can never consume the address's attribution slot —
    and `invalid_for_address` rows kept in `participants[]` with `credited: false`.
  - Determinism: no clock, no absolute path, ordered collections, stage-then-rename write.
- **Tests**: `cargo test --bin gpn-referrals` 5 passed; `--lib referral::` 29 passed; `--bin
  gpn_scheduler` 103 passed; fmt clean; clippy with CI's `CLIPPY_ALLOWS -D warnings` exit 0.
- **Live**: exported `/tmp/ref227` twice → `cmp` **BYTE-IDENTICAL**; 4 participants (3 credited),
  1 self-referral excluded; `registry_sha256` == `sha256sum` of the registry file.
- No blockers, no triage items. Next box: 228 (reorg band 33–288).

## 00:10 PT — 0.3.65 box 6 (row 226) DONE: referrals are operator-only, read with a wallet signature [no triage]

- **A0**: `GET /commits/main` → **`151072c`** (loop-log) → `actions/runs?head_sha=151072c` → **0 runs**
  (docs-only, `paths-ignore`), expected. **Checkout was 5 commits behind** again — `git status -sb`
  said `behind 5` and HEAD was a duplicate local `5595a78`. Fast-forwarded to `151072c` *before*
  reading the checklist, per the TOOLS.md lesson from 23:55; no duplicate work this time.
- **Box ticked — §3b 226** (operator-only referral visibility, design `docs/design-referral-codes.md`
  §4b, now carrying the exact signed message).
  - `GET /api/referrals?code=&sig=[&addr=]` on the AP: sr25519 signature over
    `gpn-referrals-v1|<code>` (compact or displayed spelling), verified against the wallet the code
    derives from; **same 401 for every failure** so the route cannot probe for a code's existence;
    response is `{code, referred, first_seen, recent[{first_seen}]}` and has **no address field**.
  - `addr` is optional (registry `derived_from` candidates are tried first); needed only for a code
    with zero referrals, where no registry entry names its owner.
  - New admin binary `gpn-referrals` (`--all`, `--code`, `--json`, `--registry`) reads the registry
    off disk on the AP host. No HTTP route for the admin view.
  - **`GET /providers` no longer publishes `"referral"`** — that public node → code map *was* the
    third-party attribution this row forbids. It was the only surface doing so; nothing consumed it.
- **Evidence**: `--lib referral::` **23 passed** (4 new), `--bin gpn-referrals` **3**, `--bin gpn_scheduler`
  **103**; fmt clean; clippy with CI's `CLIPPY_ALLOWS -D warnings` exit 0.
- **Evidence, live** (real binaries, AP :8196, temp `GPN_PREFIX`, registry with 2 credited Alice
  entries + 1 replayed + 1 Bob): Alice's code+sig → **200** `referred 2` (the replayed entry is not
  counted) in both code spellings; Bob's sig → 401; Alice's sig on Bob's code → 401; no sig → 401;
  legacy code → 401; wrong `addr` → 401 with the derivation sentence; `/providers` → **0**
  `"referral"`; `/referrals`, `/api/referrals/all`, `/referrers` → 404.
- **Wrong binary first**: the first live run answered 404 for every path. Cause: I ran
  `~/.ci-cache/gpn-agent-target/debug/gpn_scheduler` (a 23:35 copy from another worktree) instead of
  this checkout's `gpn-agent/target/debug/` build. `cargo test --bin` builds the test harness, not
  the binary — `cargo build --bin …` is required. After rebuilding, the same requests answered as above.
- **Mistake, fixed**: I ran `pkill -x gpn_scheduler` to stop the test AP and **killed the production
  access point on :8092** (same process name). systemd (`gpn-accesspoint.service`) auto-restarted it;
  verified `active` + `/health` **200** afterwards. Written into TOOLS.md: never kill by process name
  on this host — kill the pid whose `/proc/<pid>/cmdline` contains the test port.- **A0 tooling defect found**: `GET /actions/runs?head_sha=<short sha>` returns **`total_count: 0`** —
  it only matches a **full 40-char** sha. Run `cf706e7` (this push) reads `0` short and **1 (`CI`,
  pending)** full. Every earlier "no run for this SHA (docs-only), expected" entry in this log used a
  short sha, so those conclusions were reached by an unsound check (they happened to be right:
  `151072c`/`8e6a2b5` are docs-only). Recorded in TOOLS.md; the next run should add a tiny
  `scripts/ci-status.sh <ref>` that resolves the full sha and prints status+conclusion, so the loop
  stops hand-rolling this query.
- **CI handle**: `CI` run **pending** for `cf706e7` (full sha `cf706e70fdceb09278036977d03ee7b92cdccaf6`).
  Not waited on.
- **Next run**: box 7 = **227** (`gpn-referrals --export` deterministic airdrop input, schema-versioned).
  No decision needed; no [TRIAGE].

## 23:55 PT — no box worked: my checkout was one commit stale and the box was already live [no triage]

- **A0**: `GET /commits/main` → **`8e6a2b5`**; `actions/runs?head_sha=8e6a2b5` → **0 runs** (docs-only).
  The previous run's CI (run **44879998162**, `a13512d`) was still **queued** at 07:23Z → not waited on.
- **What went wrong, in mechanism terms**: I read the checklist from **local HEAD `dd580c4`**, where
  box 5 (row 225) was still unticked, and spent this run building a `/join` page, a `/r/:code`
  `_redirects` rule and a JS BLAKE2b for the checksum — all of which **already existed on `main`** in
  `bb6d8a3`/`a13512d` (`website/join.html`, `worker/worker.js`, `website/tests/join.test.js`). I had
  asked the API for main's sha at the start and **did not compare it with my own HEAD**; the API answer
  and the working tree disagreed by one commit and I never reconciled them. Same failure class as the
  `rev-parse origin/main` trap in TOOLS.md, one layer closer: the stale thing was the checkout itself.
- **Repair**: fetched, `git reset --hard 8e6a2b5`; the duplicate work is parked on local branch
  `dup-225` (never pushed, so the live page is untouched). Verified the row is genuinely done rather
  than claimed: `curl https://gpncoin.ai/join?ref=GPN-ZYCU-4W49-6R` → **200**, 4 `data-ref-arg`
  placeholders; `/r/GPN-ZYCU-4W49-6R` → **301** → `/join?ref=…`. Box 5 stays ticked; its evidence is real.
- **Cost of the mistake**: one run's work discarded. **Lesson written into TOOLS.md**: before working a
  box, `git fetch` and compare `git rev-parse HEAD` against `GET /commits/main` — a docs-only commit is
  invisible in `git log --oneline -1` if you never fetched, and the box you are about to "fix" may
  already be ticked and live.
- **Next run, box 6 = row 226** (referral visibility is operator-only). Plan, with the one design call
  already made so it is work and not a question: the count endpoint belongs on the **access point
  (`:8092`)**, because that is the only process that holds `referral-registry.json`; `GET
  /api/referrals?code=&sig=` verifies an **sr25519 signature over `gpn-referrals-v1‖code`** from the
  address the code derives from (`verify_for_address` already recomputes that), answers
  `{code, referred, first_seen, recent[]}` with **no addresses**, and `401` otherwise; admin view is
  `gpn-referrals --all` reading the registry file on the AP host. Tests: valid sig → count, wrong sig →
  401, someone else's code → 401, response has no address strings, and a grep proving no other route
  returns third-party attributions.

## 23:40 PT — 0.3.65 box 5 (row 225) DONE: the share link is /join, and /r works on the live site [no triage]

- **A0**: `GET /commits/main` → `dd580c4` (box 4 ticked) → `actions/runs?head_sha=…` → **no run at all**
  for that SHA (a docs-only commit; `paths-ignore` working as intended). Not blocked on CI.
- **Box ticked — §3b 225** (share link is `/join?ref=`, never a download URL), commits `bb6d8a3` +
  `a13512d`, pushed twice (second push is a one-line config fix, explained below).
  - `website/join.html` — landing page: what GPN is, what the code means, the code in plain text
    (screenshot keeps the attribution), and four commands with the code baked in (Windows
    quickstart `install.ps1`, Linux quickstart `./install.sh`, Windows manual `gpn_node --setup`,
    from-clone `./install.sh`). A malformed `?ref=` is **named** on the page, not silently dropped.
  - `worker.js` — `GET /r/<code>` → `301 /join?ref=<code>` (`_redirects` is dead config in advanced
    mode, so the route lives in the Worker; `wrangler.jsonc` gained `/r/*` in `run_worker_first`).
  - `assets/referral.js` — `rawFromSearch()`, so a page can tell "no code" from "invalid code".
  - `download.html` + `docs/run-a-node` — no surface asks for a download URL any more.
- **Evidence, tests**: `website/tests/join.test.js` **10 passed** (incl. the page's own script run
  against a stub DOM), `worker/worker.test.mjs` **62 passed**, all other website suites 0 failures;
  the join test is now a CI step.
- **Evidence, live** (polled): `/r/GPN-ZYCU-4W49-6R` → **301** → `/join?ref=GPN-ZYCU-4W49-6R`
  → **200**, 9,170 bytes, 4 `data-ref-arg` placeholders + `data-ref-code` + `id="ref-invalid"`;
  `/download` says "send a join link, not this page's URL"; `/docs/run-a-node` shows the /join form.
- **Failure found and fixed, not parked**: the first deploy left `/r/…` at **404** while `/join` was
  200. Not a routing bug — `assets.run_worker_first` did not list `/r/*`, so the asset layer answered
  before the Worker saw the request. Added the path, asserted it in `join.test.js`, redeployed: 301.
- **CI**: run **44879998162** (CI, `a13512d`) pending at the end of this run — recorded, not waited on.
  `bb6d8a3`'s pending run was superseded (same concurrency group) — a queue cancel, not a red.
- **Next run**: box 6 = **226** (operator-only referral visibility).

## 23:20 PT — 0.3.65 box 4 (row 224) DONE: the operator can find and copy their own code, and the site explains it [no triage]

- **A0**: `GET /commits/main` → `8784012` (box 3) → `actions/runs?head_sha=8784012` → run **44877406357**
  **queued** since 06:50 UTC. Not waited on (this box is code + tests + a website page). `7924b35`
  returns `total_count: 0` now, so that earlier "pending" run was superseded, not failed.
- **Box ticked — §3b 224** (the code must be findable and copyable), commit `5595a78`, pushed once.
  One definition per fact: `referral::share_link` for the link, `derive_code` for the code.
  - `gpn_node --referral [--json]` — code, owning address, source, link; exit 0 with or without a
    wallet; answered before `Config::load()`.
  - startup summary — one line with code + link, silent when there is no account.
  - dashboard :8099 — "Your referral code" panel with a Copy button, fed by `GET /api/referral`;
    a machine with no wallet gets the sentence naming `gpn_node --ensure-wallet`, not a fake code.
  - gpncoin.ai/referrals — the derivation in full, a Python snippet that reproduces the page's own
    example, the four places to find your code, an is/is-not table, and why the link is `/join?ref=`.
- **Evidence**: `--lib gui::` **17**, `--lib referral::` **19**, `--bin gpn_node` **32**,
  `website/tests/referral.test.js` **13**, `node-gui/test_referral_endpoint.py` **5 checks**;
  fmt clean; clippy with CI's full `CLIPPY_ALLOWS -D warnings` clean.
- **Live**: real `--gui` node on an isolated prefix printed the referral line and answered
  `curl :18099/api/referral` with `GPN-AX3R-YC7C-KV` + `/join?ref=…`; the card's DOM was in the
  served page. `--referral` under `strace` made **0** socket syscalls. Test tree stopped afterwards.
- **Cross-check**: the page's Python snippet was run here and produces `GPNAX3RYC7CKV`, matching the
  Rust binary for the same address — the alphabet is Crockford-style (`23456789ABCDEFGHJKLMNPQRSTUVWXYZ`),
  which is the one detail an independent reimplementation gets wrong first (it did, five minutes).
- **Incident, self-inflicted, no lasting damage**: a commit message written in double quotes contained
  backticks, so bash executed `gpn_node --referral [--json]` — which resolved to the *fleet* binary in
  `~/.local/bin` (0.3.62, no such flag) and started a node against `~/.gpn/node` for ~2 minutes before
  being killed. Fleet processes (started 2h earlier) are untouched and still running; nothing was
  written to the fleet base. Rule for this workspace: **never put backticks in a double-quoted shell
  argument** — use `-F <file>` for commit messages.
- **Next run**: box 5 = **225** (`/join?ref=` landing page + `/r/<code>` alias). Code + a website page,
  no CI gate. Ask GitHub once for 44877406357 (and 5595a78's own run) first — a CI failure outranks the box.

## 22:55 PT — 0.3.65 box 3 (row 223) DONE: referral codes derive from the wallet and the AP checks them [no triage]

- **A0**: `GET /commits/main` → `e883ac6` (loop-log docs commit) → `actions/runs?head_sha=e883ac6` →
  **`total_count: 0`** (docs-only, `paths-ignore`), expected. The two earlier code SHAs:
  `c03600a` run 44874120688 **cancelled** (superseded by the next push, not a failure of our code),
  `7924b35` run 44876315200 **pending** at start of this run — not waited on (this box is code+tests).
- **Box ticked — §3b 223** (wallet-derived referral codes, design `docs/design-referral-codes.md`).
  `GPN` + 9 base32 chars (45 bits of `blake2_256("gpn-referral-v1" || account id)`) + a checksum char
  = 13 chars, displayed `GPN-7K2X-9QMD-4T`; `parse_code`/`verify_for_address` accept both forms and
  name the expected checksum char on a typo. Registry entries gain `code`/`derived_from`/`status`
  (`verified`|`unverified`|`legacy`|`invalid_for_address`) with serde defaults, so a pre-0.3.65
  registry still loads, as `legacy`. Node: `--ref CODE@ADDRESS` stores the referrer address
  write-once; the heartbeat publishes `ref` (display form) + `ref_address`; the AP's
  `record_once_checked` recomputes and classifies.
- **Evidence**: `--lib referral::` **18 passed**; `--bin gpn_node` **32**; `--bin gpn_provider_status`
  **69**; `--bin gpn_scheduler` **103**; fmt clean; clippy `-D warnings` clean.
- **Live**: real binary in isolated prefixes — same wallet → same code on "reinstall", Bob's wallet →
  a different code, checksum typo → `checksum is 'T' but this code's checksum is 'V'` and **no files
  written**. Independent Python implementation produced the identical two codes. Real scheduler on
  :8096 + two fake providers publishing the same code with different addresses →
  `referral-registry.json`: `5VerifiedNode status: "verified"`, `5ReplayNode status:
  "invalid_for_address"` (recorded, not dropped), AP log names the replay. Both stayed routable.
- **Design conflict resolved, not escalated**: the doc says "11 characters" in one place and
  "~2⁻⁴⁸ at 12" in another while its own example is `GPN-7K2X-9QMD-4T`; the example won
  (9 payload + checksum, ~2⁻⁴⁵ collisions). Noted in the box.
- **Next run**: box 4 = **224** (show the code to the operator: dashboard card + Copy, startup
  summary, `gpn_node --referral`) — UI/CLI work, no CI gate. Ask GitHub once for 44876315200 (and
  this commit's own run) first.

## 23:05 PT — 0.3.65 box 2 (row 222) DONE: provider advertisement is a task matrix, and the AP holds boxes to it [no triage]

- **A0**: `GET /commits/main` → `0a7e7fb` (loop-log docs commit) → `actions/runs?head_sha=0a7e7fb` →
  **`total_count: 0`** (docs-only, `paths-ignore`), expected; not waited on. Run **44874120688** on
  `c03600a` was still in flight — per the rule, not blocked on: this box is code+tests.
- **Box ticked — §3b 222** (task matrix), commit `7924b35`, pushed once. `tasks::TaskMatrix` parses both
  the new shape (`tasks`/`task_models`/`limits`) and the legacy one (`capabilities` + flat `models[]`);
  the provider heartbeat now emits the matrix (derived from the live model list via `jobkind::model_kind`);
  `/providers` publishes it and `POST /v1/jobs` enforces it.
- **Evidence**: `cargo test --lib tasks::` **15 passed**; `cargo test --bin gpn_scheduler` **102 passed**;
  fmt clean; clippy with CI's `-D warnings` + `CLIPPY_ALLOWS` clean.
- **Live** (new scheduler on :8096, fake provider on :8097 declaring `chat`+`generate-image`):
  `chat`+`sd-turbo` → **400** `model 'sd-turbo' serves task=generate-image, not task=chat`;
  `chat`+`llama3.2:1b` → **503** `provider does not list model … for task=chat`; `speak` → **503**
  `no provider advertises task=speak`; `caption` → **501**; `chat`+`qwen2.5:0.5b` passed the gate and
  logged `[v1/jobs] task=chat model=qwen2.5:0.5b -> /v1/chat/completions` (then 403 "hot-wallet
  inference is disabled" — the test AP had no signer; that is the gate working, not routing failing).
  Legacy compat seen live: the 0.3.64 box on :8093 (no `tasks` field) parsed as a legacy matrix and
  stayed routable.
- **Method note for future runs**: work happens in `~/gpn-work-222` (a fresh clone), NOT in the
  runner checkout — that directory is the linux runner's work dir and a dispatched build wipes it.
- **Site gap found, not fixed here**: `website/ai.html` renders its word table from `/api/v1/tasks`,
  which is fine, but the *provider matrix* (`tasks`/`task_models`/`limits`) is not documented anywhere on
  the site yet. That belongs with the release notes (§7b), not this row.
- **Next run**: box 3 = **223** (wallet-derived referral codes, design `docs/design-referral-codes.md`) —
  code only, no CI gate. Ask GitHub once for 44874120688 (and 7924b35's own run) first.

## 22:10 PT — 0.3.65 box 1 (row 221) DONE: task vocabulary + `POST /v1/jobs` live on :8092, proven end to end [no triage]

- **A0**: `GET /commits/main` → HEAD `97b257c` (the 21:37 checklist commit) → `GET /actions/runs?head_sha=97b257c`
  → **`total_count: 0`** — docs-only commit, `ci.yml` paths-ignore, expected. No failure to diagnose; not waited on.
- **Box ticked — §3b 221** (task vocabulary + generic dispatcher, design `docs/design-task-modality-registry.md`).
  New `gpn-agent/src/tasks.rs`: 14 canonical words + aliases, `resolve()` in the design's order
  (explicit task → model family → `chat`), `serves()` as the AP gate, `adapt_v1_jobs()` as the whole dispatcher;
  `gpn_scheduler.rs` gains `POST /v1/jobs` (one parser → the word picks the endpoint → that endpoint keeps its
  own validation/moderation/escrow/routing, so there is still exactly one job-creation path) and `GET /v1/tasks`
  (the word list with `served` measured from the live registry).
- **Evidence:** `cargo test --lib tasks::` → **11 passed**; `cargo fmt -- --check` clean; clippy silent on the new
  code. Live on :8092 with this binary (AP swapped for ~4 min, then restored — `is-active` = active, `/health` 200,
  `/v1/tasks` 404 again, i.e. the 0.3.64 release binary is back): `{"task":"chat","model":"qwen2.5:0.5b",…}` →
  **200**, job `1795241259305`, answer `"Final!"`, log `[v1/jobs] task=chat model=qwen2.5:0.5b -> /v1/chat/completions`;
  no task/no model → chat, job `1795241038936`; `{"task":"teleport"}` → **400** + `valid_tasks[]`;
  `{"task":"caption"}` → **501** `no vision executor in this build`; `/v1/chat/completions` still 200 (adapter
  regression). `website/ai.html` gains §`#tasks` "One endpoint, every task" with the table **rendered from
  `/api/v1/tasks`**; `node website/tests/chat.test.js` → 45 passed.
- **Not claimed:** acceptance 2 (`speak`→MP3) was not re-run — the audio handler is unchanged by this row and needs a
  consented reference clip; acceptance 5's 503 branch has no live case on this fleet (one box advertises all four live
  capabilities) and is covered by unit test `the_serving_gate_needs_an_advertised_capability_and_an_executor`.
- **Note for the next box (222):** the provider still advertises a flat `capabilities[]`; `tasks.rs` reads only that,
  so the matrix (`task → models`) is row 222's work. Also worth a look: default routing picked `tuned-6542:latest`
  (a toy fine-tune) for an unspecified chat call and produced garbage — a catalogue/allowlist question, not this row.
- **Push**: `c03600a` → `main` (97b257c..c03600a). CI run **44874120688** is `pending` on that sha —
  a build is expected (the commit carries `.rs`), not a `paths-ignore` break. Not waited on: box 2 is code.
- **Next run**: box 2 = **222** (provider task matrix), code only — no CI gate; read 44874120688's conclusion first.

## 21:37 PT — §C: 0.3.65 opened, all 9 NEW rows bundled (221–229), checklist generated, one push [no triage]

- **A0**: HEAD `163b927` → `GET /actions/runs?head_sha=163b927` → **1 run, `pending`** (44870322382).
  That commit carries `scripts/release-timing.py` (the `--follow` fix), so a build is *expected* here —
  `paths-ignore` did not swallow it. Not waited on. Two earlier runs on `75a3460`/`07ba323`
  (44867740901, 44868181586) show `completed cancelled` ~6 min after creation = concurrency-group
  supersede, not a failure. Runner state: `DESKTOP-USBUADL online busy=true`, `openclawassist online
  busy=false` — contention, not a fault.
- **State check**: `awk` over the state column → **0 IN-PROGRESS**, 190 + 2 COMPLETE, 7 BLOCKED,
  **9 NEW (221–229)**. 0.3.64 archived with 0 unticked boxes, so §B was already satisfied and the
  loop is at §C.
- **§C done**: rows 221–229 `NEW → IN-PROGRESS` (9 flipped) · `bump-version.py patch` → **0.3.65**
  (15 manifests + `network.json` + 14 lockfile entries) · `bump-version.py --check` →
  `lockstep: 15 manifest(s) and 14 lockfile entr(ies) agree`, `gpn version: 0.3.65` · CHANGELOG `[0.3.65]`
  written from the nine row titles (no `_TODO_`) · `make-release-checklist.py 0.3.65` → **9 in-progress,
  7 blocked rows** in `docs/releases/0.3.65.md`. One commit `385e6d9`, one push to `main`.
- **Bundle shape**: 221 task vocabulary + `/v1/jobs` dispatcher · 222 provider task matrix · 223
  wallet-derived referral codes · 224 operator-facing code surface (dashboard/`--referral`/`/api/referral`) ·
  225 `/join?ref=` share link · 226 operator-only referral visibility · 227 `gpn-referrals --export` ·
  228 the reorg band 33–288 that accepts and never downloads · 229 retarget constants vs the shipped
  0.3.62 artifact. 228 and 229 are real defects found by the staged row-194 run and belong ahead of
  the referral surface in priority, even though the checklist lists 221 first.
- **Generator nit (cosmetic, not fixed in code)**: row 223's title contains a second `done when`
  clause, so the generated box was truncated mid-title; that one box's title/`done when` line was
  repaired by hand in `0.3.65.md`. The other 8 boxes are clean. Worth teaching
  `make-release-checklist.py` to split on the *last* `— done when:` — a `scripts/` change, deferred.
- **Next run**: box 1 of `docs/releases/0.3.65.md` — 221 (task vocabulary + dispatcher), which is code,
  so the change is written and tested locally, not gated on the pending CI run. No decision needed;
  no [TRIAGE].

## 21:20 PT — 0.3.64 is FINISHED and FILED: cadence gate PASS on a fully 0.3.64-authored window; 217 + 218 COMPLETE; §10b timing recorded (and its tool fixed) [no triage]

- **A0**: HEAD `5b1ffc2` → `GET /actions/runs?head_sha=` → **`total_count: 0`** (docs-only commit,
  `ci.yml` paths-ignore — expected). No failure to diagnose.
- **Did not wait for the 21:31 timer.** Chain head was already `#337580`, i.e. a complete
  post-deploy window existed (restart 20:50:33). Ran the gate by hand:
  `scripts/check-block-cadence.py --journal-unit gpn-fleet` → window **337481..337580**, mean
  **14.13 s**, median 10.07 s, max **72.88 s**, clause 1 **0 violations**, clause 2 **6 gaps ≥3×
  mean / 0 unexplained**, **`RESULT: PASS`** (sample `/tmp/cadence-now.json`). Sub-3 s bursts and the
  152 s stall class are both gone; the six long gaps all carry `sealer: still searching template …`
  from the new binary (20:56:28, 21:01:24, 21:03:24, 21:06:17, 21:09:58, 21:10:42).
- **Boxes ticked:** §3b **217**, §3b **218**, §10 last box (task rows marked), §10b both, §0b both
  filing boxes. `docs/releases/0.3.64.md` → `docs/releases/archive/0.3.64.md` (0 unticked boxes),
  `INDEX.md` row added, GPN-LIST rows 217/218 → `COMPLETE 0.3.64`.
- **Tool bug found and fixed:** `scripts/release-timing.py 0.3.64` exited with *"no ticked boxes
  found in history"* while `git log` listed 9 commits — `git log --follow --reverse` returns exactly
  one commit (git cannot reverse-follow). Dropped `--follow`, sort in Python. Now traces 44 of 59
  boxes; `docs/releases/timing/0.3.64.{json,md}` committed. Slowest step named: §4's CI-green wait,
  1h00m, a runner-cache problem not a checklist one.
- **Next run:** no IN-PROGRESS items remain → §C: bundle remaining NEW rows, bump version, write
  CHANGELOG, one push, generate the checklist.

## 21:00 PT — 0.3.64: peer-stability box ticked on measured evidence; cadence window is 73 blocks short, handle set for the 21:31 timer run [no triage]

- **A0**: `GET /commits/main` → HEAD `0b20b74`; `GET /actions/runs?head_sha=0b20b74` → **`total_count: 0`**
  (docs-only commit, `ci.yml` paths-ignore — expected, not a missing build). No failure to diagnose.
- **Box ticked (§10) — "Peers hold >5 minutes; no new `Reason:` bans; no unexpected `gpn-isolated`/`gpn-forked`.**
  Measured on the post-update binary (restart `20:50:33`, pid 3436525): peer-count histogram over
  `20:51:00–20:58:12` → **85 × `Idle (3 peers)`**, 2 × `1 peers` (the two re-dial samples right after
  restart), 3 peers held **20:51:12 → 20:58:12 (7 min 0 s)**; `grep -icE 'Reason:|gpn-isolated|gpn-forked'`
  → **0**; `canonical/anchor-match=true` **35**, `anchor-match=false` **0**; `system_health`
  `{"peers":3,"isSyncing":false}`, `system_peers` → 3 AUTHORITY peers (#337489–#337494).
  The previous run had ticked nothing here because the hold was tied to the 100-block window; the box's
  three clauses are independently measurable and all pass, so it is ticked on its own evidence.
- **Not ticked — §3b 217/218, §10 last box, §10b, §0b filing (both):** all wait on the first 100-block
  window authored entirely by 0.3.64. Checked the gate state instead of guessing: `~/.gpn/cadence-gate.state`
  `checked_at 2026-11-21T04:51:20Z` window `337367..337466`, `RESULT: FAIL` — the **same two pre-deploy gaps**
  (67.90 s / 63.27 s) as the 21:02 run, unchanged and authored by 0.3.63. Chain head is **#337496** (hex
  `0x52655`), i.e. 27 of the 100 post-deploy blocks exist; the clean window needs head ≈#337567 (≈21:16 PT).
  `gpn-cadence-gate.timer` next fires **21:01** then **21:31** — **21:31 is the first clean one**. Handle written
  into the checklist; the run ended rather than sleeping ~19 min inside it.
- **Next run (≈21:27+):** read `~/.gpn/cadence-gate.state`; if `exit 0`, tick 217, 218, §10's last box,
  then run §10b `release-timing.py 0.3.64`, the two §0b filing boxes, and mark GPN-LIST rows COMPLETE.
  If it still FAILs with unexplained >3x gaps on **post-deploy** blocks, that is a real 217 failure —
  read the suspect gaps against `sealer:` lines and fix, do not re-run the timer for a nicer number.

## 21:05 PT — 0.3.64 IS RELEASED and the fleet runs it: cut, publish, site stamp, self-update all done; only the 218 behaviour proof waits on a clean 100-block window [no triage]

- **A0**: origin/main HEAD was `d6c0e37` → `total_count: 0` (docs-only, paths-ignore, expected).
  The release commit's CI run **44853610678** (`9df5341`) was polled with a bounded watcher and
  reached **`completed success` at 2026-11-21T04:42Z**: secret-scan ✅, fmt+clippy ✅, linux
  build+tests ✅ (02:59:29→03:30:45Z), windows self-hosted build+tests ✅ (03:23:56→04:42Z; its last
  step, gpn-agent, ran 04:02:37→04:42Z after the gpn-node step's 38 min — a cold build, not a hang,
  which is why it was not cancelled). No failure existed to diagnose this run.
- **Box ticked (§4, now 4/4 DONE)** — the CI-green gate, with the run id and per-job times.
- **§7 DONE — the release is cut.** `bash scripts/release-cut.sh --version 0.3.64 --timeout 300 --yes`,
  exit 0, `released 0.3.64 — https://gpncoin.ai/releases.json`. Content gate: `OK: 0.3.64 artifacts
  verified on linux, windows — 5 marker(s) present in each`. Manifest commit `75a3460`. Mirror digests
  re-checked by API: linux 340,489,746 B `e768dd45…`, windows 247,027,527 B `4d5b9f4e…` on both repos;
  `network.json latest_version` 0.3.64; `releases.json` served 0.3.64 twice, 11 s apart.
- **§7b DONE — the docs drift was real and is fixed.** The site's stamp said **0.3.61 in the md sources
  and 0.3.63 in the generated HTML** — i.e. the HTML had been edited without the sources. All 16
  `docs/*.md` stamps and 9 generated pages now read `release 0.3.64 — checked 2026-11-20 at block
  337,469` (block read from the live node), `download.html` says `Current signed release: 0.3.64`, and
  the live URLs were polled after the Cloudflare deploy, not read from the tree. Commit `07ba323`.
  Note: `build-site-docs.py` was run and then **reverted** — it rewrites every nav link to `.html`,
  which is a template change this release has no business making; the stamps were edited in place
  (28 files, 29+/29-).
- **§8 DONE** — anonymous download of both assets from `gpncoin.ai` (release-cut step 7) matched the
  signed manifest by digest AND size; live docs pages polled at 0.3.64.
- **§9 DONE — a node updated itself and is running 0.3.64.** `systemctl --user start gpn-update.service`
  → `confirm: process=up rpc=yes 24s version=0.3.64` → `confirmed 0.3.64 is running`; `POST :9944
  system_version` → `0.3.64`; rollback retained (`versions/0.3.63`, `current.previous` → it).
- **Still open (§3b 217/218, §10)**: the behaviour proof needs a 100-block window authored entirely by
  the new binary (deploy was 20:50:58 at #337466). The 21:02 gate run still covered two pre-deploy gaps
  (`#337367→#337368` 67.90 s, `#337449→#337450` 63.27 s), unattributable by construction. The timer runs
  every 30 min and writes `~/.gpn/cadence-gate.state`; **next run should read that file plus
  `journalctl --user -u gpn-fleet | grep 'sealer:'` and tick 217/218, §10, §10b and the filing boxes**.
- **Addendum 21:10 PT — the heartbeat is live and already explains a real gap.** Six `sealer:` lines since
  the deploy, e.g. `20:53:59 … template 0x02a894e2… after 20.1s (147 nonce window(s) searched, GPU healthy)`
  and `20:54:19 … same template … after 40.1s (294 nonce window(s))` — a second heartbeat exactly 20 s later.
  `Imported #337479` 20:53:39 → `#337480` 20:54:24 is a **45 s gap** and both lines fall inside it: the
  attribution 217's clause 2 asks for, on a gap the old binary left silent. §10's first box ticked on that
  (`204b8e3`). Only the 100-block window remains.

## 20:05 PT — 0.3.64: §5 closed (7/7); §4 CI gate still unanswered, one Windows job in flight [no triage]

- **A0**: origin/main HEAD `ebf2943` is docs-only (`total_count: 0` runs — paths-ignore, expected).
  The release commit's own CI (`9df5341`, run `44853610678`) polled **once**: secret-scan ✅,
  fmt+clippy ✅, linux build+tests ✅ (02:59:29→03:30:45Z), `Build + unit tests (windows,
  self-hosted)` **in_progress since 03:23:56Z on DESKTOP-USBUADL** (~35 min of a 40–75 min cold
  build). **No failure to diagnose**; not cancelled — it is the only attestation-bearing Windows
  build of this sha. Run `44858730640` (`6f0e780`, the image_channel test fix) still `pending`,
  `jobs: []`, queued behind the same concurrency group: contention, not a fault.
- **Box ticked** (`docs/releases/0.3.64.md`):
  - §5 "the attestation exists for the digest that is actually published" — re-derived in this run:
    `bash scripts/verify-windows-bundle.sh windows-latest` → API digest
    `sha256:4d5b9f4e…3927329` == downloaded bytes `4d5b9f4e…3927329` (247,027,527 B), bundle 9,033 B,
    `cosign verify-blob --bundle` → **`Verified OK`**. §5 is now **7/7 DONE**.
    Caveat written in the box: the `releases.json` half lands with the cut, and release-cut
    sha256-checks both assets against the API digests before signing, so a drifting manifest fails
    its own step 2 rather than publishing silently.
- **No cut attempted this run**: the gate is unchanged from 19:35 (`in_progress:None`), and a second
  identical `release-cut` retry without new information is a guess, not a test. `~/.gpn/release-in-flight`
  verified **absent** (last run's lock leak did not recur), so the next run can cut immediately.
- **Next run**: poll `44853610678` once. `success` → tick §4 and run
  `scripts/release-cut.sh --version 0.3.64`; `failure` → read that job's log, quote the line, fix
  the cause. Then §7 → §9/§10 (deploy 0.3.64 so 217/218 can be measured on the heartbeat binary).

## 19:58 PT — 0.3.64: cosign verify OK, cut attempted and correctly refused by its CI gate [no triage]

- **A0**: main HEAD `fecf30f` — `GET /actions/runs?head_sha=fecf30f` → `total_count: 0`; docs-only
  commit, `ci.yml` paths-ignore. Expected, not a missing build. The build/publish step this run
  works is §5–§7, so the CI rule mattered: the **release commit's** CI (`9df5341`, run
  `44853610678`) is in flight and its Windows job is a normal cold build, so it was **not**
  cancelled (35 min job-level, run-level `created_at` is queue time).
- **Boxes ticked** (`docs/releases/0.3.64.md`, one docs commit):
  - §5 "verify the downloaded artifact with the verifier that matches the mechanism" —
    `scripts/verify-windows-bundle.sh windows-latest` → **`Verified OK`**, keyless cosign, on the
    published bytes `sha256 4d5b9f4e…` (247,027,527 B) + bundle `2c20209c…`.
  - §7 "refuse to cut unless §5's attestation checks passed" — that same hand run against the
    asset `release-cut.sh` step 2 fetches.
  - §6 windows half: `unzip -p gpn-windows.zip 'gpn-node.exe' | grep -c -a -F` → `still searching
    template` 1, `reorg=` 1, `work ours=` 1, `gpn-forked` 2. Both platforms carry the change.
- **Cut attempted, refused (correctly)**: `scripts/release-cut.sh --version 0.3.64` downloaded and
  sha256-verified both assets, read `0.3.64` from the linux bundle's `gpn-build-v1` stamp, then
  `ABORT: CI for 9df5341 is 'in_progress:None' -- refusing to sign an unverified build` (exit 1).
  Nothing was mirrored, signed or published; gpncoin.ai still serves `0.3.63`.
- **Next run**: poll `44853610678` once; if green, re-run `scripts/release-cut.sh --version 0.3.64`
  (idempotent). Second handle: `44858730640` on `6f0e780` (image_channel 120 s deadlock-guard fix),
  `pending` behind the single-slot Windows runner.
- **Defect found, carry to next batch**: an aborted `release-cut.sh` leaves the commit lock behind —
  `~/.gpn/release-in-flight` named the dead `holder: jeremyg@openclawassist pid 3410598` while
  `cleanup()` only releases when the file reads `release-cut pid $$`, a string
  `release-lock.sh take` had already overwritten. Cost one manual `release-lock.sh release 0.3.64`.
  Fix belongs with the other queued script change (drop macOS from `dispatch-release.sh`).
- **Blockers/triage**: none. Neither 217 nor 218 can be proven until 0.3.64 is deployed (§9/§10).

## 19:12 PT — no red CI of ours on main; macOS job red is GitHub billing; 217/218 attribution measured and shown to be impossible on 0.3.63 [no triage]

- **A0**: main HEAD `f4a6ce3` is a docs commit → `head_sha=` query returns **`total_count: 0`**, expected
  under `paths-ignore` (not a fault, per the 2026-11-19 rule). No failing CI on main.
- **The one red run, read and classified**: **44856007475** `Unix binaries (Linux + macOS)`, job
  **136698271250** `build (macos-latest)`. Log blob is gone (`BlobNotFound`), so the evidence is the
  check annotation, quoted: *"The job was not started because recent account payments have failed or
  your spending limit needs to be increased"*. **Environment / billing, not our code, not our test.**
  macOS is not in the manifest and an owner rule forbids spending, so this is a wrong dispatch
  (`scripts/dispatch-release.sh` still fires it). One-line fix — `continue-on-error: true` on the
  macOS job at `release-unix.yml:32` — is deliberately **not** committed this run: no workflow change
  while a release is in flight. Queued for the next batch.
- **Work done on §3b (the first unticked boxes in file order)**: re-ran the cadence gate with log
  attribution, `--journal-unit gpn-fleet` → window 336982..337081, mean 15.76 s, max 97.64 s,
  **clause 1 PASSES (0 violations)**, clause 2 has 7 suspect gaps 3.09–6.19x mean. Then established
  *why* they are unexplained: the deployed binary is **0.3.63**, and `grep -c -a -F 'still searching
  template'` → **0** on it vs **1** on the built 0.3.64 binary. The attribution logging does not exist
  in the running version, so "no log supplied" is a measurement limit, not a silent stall. Both
  notes are in `0.3.64.md` §3b; both boxes stay unticked until §9 deploys 0.3.64.
- **Release state**: Windows build **44856009729** `in_progress` at step 10/23 (`build node`) since
  02:27Z; CI 44858730640 pending, 44853610678 queued; both runners report `busy: true`. §5's last two
  boxes need that zip → dispatched handle recorded in the box, **not waited on** (§5/§7 boxes belong to
  a build step, and the run is 45 min old, under the 90-min stale threshold).

## 19:10 PT — red CI was a 5 s speed assertion on a loaded runner; cancelled the stale run blocking BOTH the CI gate and the Windows build; §6 linux scan ticked [no triage]

- **A0**: main HEAD was `13a0c8c` (loop-log only) → no CI run, expected under paths-ignore. The
  release gate run **44853610678** (`9df5341`) had been `pending` with **0 jobs for 60 min**.
- **Root cause found (the blocker was not "contention")**: `ci.yml:53 concurrency: group:
  ci-${{ github.ref }}` was held by run **44847689477** (`f5ed974a`, started 01:02Z, 118 min old,
  superseded, one job already red). That same run held the **single-slot Windows runner**, which is
  why the 0.3.64 Windows build **44856009729** sat `queued` for 40 min. Cancelled by API → 202,
  observed `completed cancelled`; **44856009729 went `in_progress` within 60 s** and the gate run left
  `pending`.
- **Red job read, quoted, classified**: job **136689883851**, step `Test gpn-agent (non-GPU)` →
  `test happy_path_202_poll_completes_with_zero_transactions ... FAILED`,
  `panicked at tests/image_channel.rs:276:5: an off-chain render job must not wait for block
  inclusion`, `test result: FAILED. 7 passed; 1 failed ... 8.14s`. **Our test** — it asserted
  `elapsed < 5s` around a stub round-trip: a speed test wearing a correctness test's name. The same
  test idles in **0.03 s** here. The real property is structural (no chain client ⇒ no create
  possible; `f.stub.jobs == 1` ⇒ exactly one off-chain POST). Bound is now a **120 s deadlock guard**
  with that reasoning in the comment. Fixed in **6f0e780**, tests-only
  (`git diff --stat 9df5341..6f0e780` → no production code, so the 9df5341 artifacts are not stale);
  local re-run `cargo test --release --test image_channel` → **8 passed; 0 failed** in 0.10 s.
  New CI run **44858730640** on `6f0e780`, handle recorded, not waited on.
- **§6 box 1 + box 2 ticked with artifact evidence**: linux asset `gpn-linux.tar.gz`
  340,489,746 B, `updated_at 02:40:52Z`, API digest `sha256:e768dd45…c560cb`, downloaded through the
  authenticated asset API and `sha256sum` **matches**. Strings scanned in the *node* binary
  **`gpn-node`** (not `gpn_node`, the launcher, which carries none of them): `reorg=` 1,
  `work ours=` 1, `gpn-forked` 2, `still searching template` 1. `./gpn-node --version` →
  `gpn-node 0.3.64`; `./gpn_node --version` → `gpn_node 0.3.64 (spec 52, 9df5341)`.
- **Still unticked (needs the Windows artifact)**: §4 CI green (**44858730640** pending),
  §5 attestation-for-published-digest, §5 verifier, §7 cut. Windows build 44856009729 is building.
- **Blockers**: none. **Triage**: none. **Next run**: ask GitHub once for 44858730640 and
  44856009729; on green + publish, tick §4, the §5 remainder, and run §7's `release-cut.sh`.

## 18:33 PT — 0.3.64 builds in flight (linux building, windows queued); the one red run is the known macOS billing job, cause re-confirmed [no triage]

- **A0**: main HEAD `3798b1f` (loop-log/docs only) → `runs?head_sha=3798b1f…` → `total_count: 0`,
  expected under `ci.yml` paths-ignore. The release's own CI gate run **44853610678** (`9df5341`)
  is still `pending` with **0 jobs** after 30 min — both self-hosted runners report `busy: true`
  (`openclawassist` Linux/gpu, `DESKTOP-USBUADL` Windows/X64), i.e. queue contention from the two
  release builds, not a fault. Not waited on, not cancelled.
- **Builds**: `44856005252` linux `in_progress` — steps 1-4 `success`, step 5 `build (waits for
  ~/.gpn/build.lock)` running since 02:27:44Z (~10 min); package/publish pending.
  `44856009729` windows `queued` (single-slot Windows runner). Both on `9df5341`. No box ticked:
  §5 boxes 6-7 and the §5/§6 attestation+scan boxes all need the published artifacts.
- **Red run diagnosed — `44856007475` "Unix binaries (Linux + macOS)", job 136698271250
  `build (macos-latest)` failed in 3 s with `steps: []`, `runner_id: 0`.** Annotation (check-run
  API, re-read this run): *"The job was not started because recent account payments have failed or
  your spending limit needs to be increased."* Classification: **environment/billing**, not our code
  and not our test — GitHub-hosted macOS runners are unpaid on this account, and the owner rule
  forbids spending. macOS is not in the release manifest. The cause on our side is the dispatch, not
  the workflow: `scripts/dispatch-release.sh:127-131` still lists `release-unix.yml`.
- **Fix prepared, deliberately not pushed**: while 0.3.64 is in flight a `scripts/` commit would start
  a `ci.yml` run on the same single-slot Linux runner and queue behind the release build. The exact
  change for the next batch (apply, do not re-diagnose): drop the `"release-unix.yml"` line from the
  `workflows=(…)` array at `scripts/dispatch-release.sh:127-131`, keep the comment naming macOS as
  unbuildable (billing), and grep `docs/updates.md:392`'s `gh workflow run release-unix.yml` example
  to replace it with `release-linux-selfhosted.yml`.
- **Blockers**: none. **Triage**: none. **Next run**: ask GitHub once for `44856005252` /
  `44856009729`; on success tick §5 boxes 6-7 and move to §6 (attestation + binary string scan).

## 18:30 PT — 0.3.64 §1-§3 ticked with fresh local evidence; §5 both builds dispatched and the run ended [no triage]

- **A0**: main HEAD `9df5341` (the 0.3.64 bundle commit) has CI run **44853610678**, `pending`,
  `created_at 02:02:16Z`, **`total_count` of jobs = 0** at 18:35 PT — queued behind the single-slot
  runner (contention, not a fault). Not waited on; the checklist carries the handle.
- **§1 + §2 + §3 ticked, all re-run in this checkout at `9df5341`** (not copied from the bundle log):
  - `seal_watch_tests` → `5 passed; 0 failed (290 filtered out)`.
  - `cargo test --release -p gpn-node --bin gpn-node` → `293 passed; 0 failed; 2 ignored`.
  - `cargo fmt --all -- --check` (in `gpn-node/`) → exit 0.
  - `gpn-agent` parity `shipped_bootnodes` + `launcher_bootnodes` → `4 + 4 = 8 passed, 0 failed`.
  - `bump-version.py --check` → `lockstep: 15 manifest(s) and 14 lockfile entr(ies) agree`, 0.3.64.
  - Fail-before for 218 recorded machine-re-runnably: `git grep -c 'sealer:' 1300b28` → **0 hits
    (exit 1)**, at HEAD → 2. Call site `service.rs:3229` (`SealWatch::new(SEAL_HEARTBEAT_MS)`).
- **§4**: control pair proves paths-ignore still selective — `head_sha=465a818` (docs) → 0 runs;
  `9df5341` (Cargo.toml) → run 44853610678. My one push this run (`182916f`, checklist only) →
  `runs?head_sha=182916f` → **0**, so it cannot supersede the release CI run.
- **§5 dispatched (this is why the run ended here)**: `scripts/dispatch-release.sh 9df5341…` → tag
  `build/9df534110e3b`; linux run **44856005252** (`in_progress`), windows run **44856009729**
  (`queued`), same `head_sha` on both. Attestation ordering verified statically for a private
  repo (cosign sign at :699, unconditional provenance gate at :720 that throws, publish at :748).
- **Found and named, not fixed (wrong place to push)**: `release-unix.yml` is dispatched every
  release and dies in 3 s — annotation *"The job was not started because recent account payments
  have failed or your spending limit needs to be increased"* = hosted macOS billing. macOS is not
  in the manifest and there is an owner rule against spending money, so the dispatch itself is the
  bug; `scripts/dispatch-release.sh`'s workflow list should drop it. Queued for the next batch.
- **Blockers**: none. **Triage**: none. **Next run**: poll 44853610678 (CI) + 44856005252 /
  44856009729 (builds) once, tick §4 box 4 and §5 boxes 6-7, then §6 artifact scan.

## 17:55 PT — 0.3.63 verified complete; 0.3.64 bundled (217 re-bundle + 218 sealer heartbeat) [no triage]

- **A0**: main HEAD `465a818` — no CI run exists for that sha (`head_sha=` → `total_count: 0`);
  docs-only close-out commit, `ci.yml` paths-ignores `**/*.md`. Expected.
- **B verified, nothing to mark COMPLETE**: every box in `archive/0.3.63.md` is ticked, the archive
  move and the `INDEX.md` row are in `465a818`, `https://gpncoin.ai/releases.json` serves `0.3.63`
  (`published_at 2026-11-21T00:39:45Z`, linux `461f8b77…`), and this box's **running** node reports
  it: `system_version` on `127.0.0.1:9944` → `0.3.63`. Row 217 was deliberately released IN-PROGRESS
  (0.3.63 has an empty `.rs` diff), so there is no row to flip to COMPLETE.
- **C — bundled 0.3.64**, per the sequencing recorded in 0.3.63 §3b and NEEDS-TRIAGE:
  - merged `origin/fix/218-sealer-heartbeat` (`fe71e35`) → merge commit `c83149b`;
    one file, `gpn-node/node/src/service.rs`, +205/−2 (SealWatch: 20 s heartbeat,
    ≥45 s slow-solve WARN, >2 s blocked-submit WARN).
  - rows **217** (re-bundle) and **218** marked IN-PROGRESS with the bundle note.
  - `/usr/bin/python3 scripts/bump-version.py patch` → 0.3.64, 15 manifests + 14 lockfiles +
    `network.json latest_version`; CHANGELOG section written (no `_TODO_`).
  - `scripts/make-release-checklist.py 0.3.64` → `docs/releases/0.3.64.md` (2 in-progress).
- **Testing gate** (run in a clean clone at `~/gpn-release-work`, because this checkout is the
  runner's work dir and CI force-checked it out mid-build at 17:58 — that is what killed the first
  local build with `can't create …/librocksdb-sys-*/out/*.o: No such file or directory`;
  environment, not code):
  - `cargo test --release -p gpn-node --bin gpn-node` → **293 passed; 0 failed; 2 ignored**
  - `… seal_watch_tests` → **5 passed**
  - `cargo fmt -- --check` → exit 0; CI allow-list `cargo clippy --release --workspace --all-targets -D warnings` → clean.
- **Next run**: work box 1 of `docs/releases/0.3.64.md`; the long pole is the build/publish (§5–§7).
  Do not merge anything else into main while 0.3.64 is in flight.

## 17:4x PT — 0.3.63 CLOSED AND FILED: §10 row update, §10b timings, archive + INDEX [no triage]

- **A0**: CI on main HEAD `a42374a` — `GET /actions/runs?head_sha=a42374a` → `total_count: 0`.
  Docs-only commit, `ci.yml` paths-ignores `**/*.md`; expected, not a missing build.
- **Self-check of the previous tick**: `https://gpncoin.ai/releases.json` still serves
  `0.3.63` `published_at 2026-11-21T00:39:45Z`, assets `461f8b77` / `a7b1793b` — holds.
- **Boxes ticked** (all in `docs/releases/archive/0.3.63.md`, one docs commit):
  - §10 box 3 "mark the task row DONE" — ticked **as n/a, not as DONE**: 0.3.63's scope for row 217
    was the restated acceptance + its gate, and the gate's live verdict is FAIL (17:01 PT, mean
    11.80 s, 3 unexplained >3x gaps, max 84.27 s). Row 217 therefore stays IN-PROGRESS in GPN-LIST.md
    and now carries `RELEASED 0.3.63 (2026-11-20) — release ccfa687, build 1bd78d8 … re-bundles with
    fix/218-sealer-heartbeat (fe71e35)`. Marking it COMPLETE would have been a false claim: 0.3.63
    has an empty `.rs` diff.
  - §10b both boxes — `scripts/release-timing.py 0.3.63 --json docs/releases/timing/0.3.63.json`
    exit 0, `total: 6h29m`, `38 of 51 seen`; `docs/releases/timing/0.3.63.md` committed (74 lines).
    Slowest measurable step **4h15m**, and the honest reading is recording latency, not work:
    36 of 38 ticks share one timestamp (17:05:29) because the back half was ticked in a single
    commit. Named shortening: tick each box in the commit that satisfies it.
  - §0b filing both boxes — `git mv docs/releases/0.3.63.md docs/releases/archive/0.3.63.md` done;
    `docs/releases/INDEX.md` has its row.
- **Blockers**: none. **Triage**: none.
- **Next run**: every 0.3.63 box is ticked and the release is filed, so the next run may go to
  rule **C** if no item is IN-PROGRESS — but rows **217 (IN-PROGRESS)** and **218 (NEW, instrumented
  on `fix/218-sealer-heartbeat` @ `fe71e35`)** are queued for the next code bundle, 217 by the
  decision recorded in §3b. Bundling those two into 0.3.64 (merge `fe71e35`, gate goes green) is
  the obvious next step.

## 17:24 PT — §9 deploy/confirm DONE (4/4): this box self-updated to 0.3.63; §10 2/3 [no triage]

- **A0**: CI on main HEAD `44f75e4` — no run exists for that sha (`GET /actions/runs?head_sha=`
  → `total_count: 0`), which is correct: it is a docs-only commit and `ci.yml` paths-ignores `**/*.md`.
- **Box worked**: §9 "Deploy and confirm" — all four boxes, on this host's own node.
  - self-update: `systemctl --user start gpn-update.service` 17:14:45 PT → `confirm: process=up
    rpc=no 0s` → `rpc=yes 24s version=0.3.63` → `confirmed 0.3.63 is running` 17:16:55 PT.
    `update-attempts.json`: `outcome confirmed`, `release_id 461f8b77…` == the linux asset sha256.
  - running process: `/home/jeremyg/.local/bin/gpn-node`, pid 3278482 (was 2944334), 107,875,536 B,
    mtime 17:16; `--version` → `gpn-node 0.3.63`; RPC `system_version` → `0.3.63`.
  - honest gap: the unit runs `--quiet`, so the journal has the confirm arm but not the
    check/download/swap step lines; the transition is dated on disk instead (versions/0.3.63 at
    17:15, symlink + pid at 17:16).
  - retention/failure: `current.previous` → `versions/0.3.62`; a refused apply
    (`--state-dir /tmp/relprobe/state`) → rc 1 "cannot tell which version this box runs", and the
    node answered `0.3.63` on the **same pid** right after — a failure does not take it down.
- **§10 2/3**: change observed = the cadence gate running unattended (17:01:22, timer) and printing
  `RESULT: FAIL` — clause 2, 3 unexplained gaps (84.27 s/7.14x, 47.60 s/4.03x, 54.01 s/4.58x).
  The gate works; the acceptance is not met, so **row 217 stays IN-PROGRESS** and §10's "mark the row
  DONE" box is deliberately left unticked. Peers: 6 m 33 s after restart, 3 × AUTHORITY, all at
  `#336669` / `0x01e6ca49`, 0 ban/isolated/forked lines since the restart.
- **Next run**: §10 box 3 (row 217 status) is the first unticked box; then filing (§0b) once §10 closes.

## 17:05 PT — §7b capability box ticked: audio is now callable from gpncoin.ai (two Worker defects fixed) [no triage]

- **A0**: CI on main HEAD `4f3b803` = `pending` (run 44846236695, queued 00:48Z) — noted, not waited on,
  because the box worked was docs + Worker, not a build step.
- **Box worked**: §7b "Every capability added in a bundle is discoverable from gpncoin.ai" — the last
  unticked box before §9. Row 202's audio shipped in 0.3.61 and `website/ai.html` had zero mentions.
- **Defect 1 — the site could not submit a clip at all.** `worker.js` used `AP_PROXY_MAX_BODY` (64 KB)
  for every proxied POST. A 5 s/22 kHz reference clip is 220 KB raw → ~294 KB base64. Live probe:
  `70 KB`, `100 KB`, `200 KB` → `"the access point sent a malformed response"`; `400 KB` →
  `"could not complete the request"`. The AP itself answers the same bodies `202` over loopback and
  over a raw TCP socket, so the fault was ours. Fix: `AUDIO_MAX_BODY = 12 MB` on `/v1/audio/speech`
  only (AP refuses >8 MB decoded, base64 inflates 4/3) — `dfd36e1`.
- **Defect 2 — truncated socket writes.** After the cap was raised, 200 KB still failed while 60 KB
  worked, i.e. the boundary moved to exactly 64 KB again. `socketHttpRequest` wrote the whole body in
  one `writer.write()`; the Workers runtime truncates past ~64 KB. Now 16 KB chunks — `f5ed974`.
- **Proof (live, through the site)**: POST `https://gpncoin.ai/api/v1/audio/speech` with the 9.66 s
  `ref.wav` (568 KB body) → `202` → `completed` in ~21 s → `67,536 B, audio/mpeg, seconds 9.228`,
  `payment: channel`, `channel_id 7`; `GET /api/v1/audio/<id>` → `content-type: audio/mpeg`,
  `sha256 a9eeb92b…` == `data[0].sha256`; `file` → `MPEG ADTS, layer III, v2, 64 kbps, 24 kHz, Monaural`.
- **Docs**: `website/ai.html` gains a live "🔊 Audio / speech" tab (Linux / macOS / Python) covering the
  5–15 s window, `consent:true`, MP3-only, measured-seconds billing with refund, no retention +
  `voice_sha256`, poll-then-download, and the `503 payment: not_charged` out-of-window case;
  `AudioGeneration` named beside the other capabilities; meta description updated.
- **Tests**: `node --test worker/worker.test.mjs` → **61 passed, 0 failed**, including a new case that a
  300 KB clip body is forwarded whole where the chat cap would have 413'd.
- **Rows**: 219 and 220 marked COMPLETE with this evidence. Template §7b now says **one box per
  capability**, and that "example request actually works" means paste the output.
- **Next box**: §9 (one node updates itself and reports the new version). Nothing needs Jeremy.

## 16:5x PT — 0.3.63 PUBLISHED (releases.json 0.3.63, both assets); §5/§7/§7b/§8 ticked, two release-cut defects fixed [no triage]

- **A0**: `GET /actions/runs/44818515439` → `completed / success` on `1bd78d8` (the scripts commit);
  docs SHA has no run (paths-ignore) — expected.
- **Published**: `scripts/release-cut.sh --version 0.3.63 --timeout 300` → commit **ccfa687**
  `release: sign 0.3.63 (linux 340.5MB 461f8b77, windows 247.0MB a7b1793b)`, live manifest 0.3.63,
  anonymous mirror download verified, exit 0. `releases.json` polled twice 10 s apart → 0.3.63 both
  times, same `published_at` 00:39:45Z.
- **§5 (2/2)**: windows run `44840000852` success; asset `gpn-windows.zip` 247,019,226 B sha256
  `a7b1793b…` + `.bundle` 9,049 B; `cosign verify-blob` with the workflow identity regexp → **Verified OK**
  on the re-downloaded bytes, *before* the cut.
- **Defect 1 (mirror provenance)**: step 4 renamed the old zip aside but left the previous `.bundle`,
  so 0.3.63's mirror served a bundle for 0.3.62's zip — `cosign verify-blob` failed with
  `bundle="ce0781c6…", payload="a7b1793b…"`. Added step 4b (download source bundle, verify digest,
  mirror rename-aside, refuse the cut if absent, `a76dd59`), ran it verbatim to repair the mirror:
  now `Verified OK`, bundle `b187cf60…`.
- **Defect 2 (lock leak)**: `--dry-run` took `~/.gpn/release-in-flight` and exited without releasing —
  it blocked this run's first docs commit. Fixed in `a76dd59`: `take` skipped under dry-run, and
  `cleanup()` drops the lock when this process is the holder (exercised: prints
  `released the commit lock (0.3.99) on exit 0`, file gone).
- **§7 (4/4)**: cut done; `network.json:23 latest_version 0.3.63`; manifest polled twice.
  **§7b (3/4)**: live `gpncoin.ai/download` 0.3.63, `docs/run-a-node` + `docs/downloads`
  `release 0.3.64 — checked 2026-11-20 at block 337,469`; both repos' latest tags carry
  `461f8b77…` / `a7b1793b…`; `seeds.json` has no version field (generated per-request, n/a).
  Nine `website/docs/*.html` stamps were still on 0.3.62 and moved to 0.3.63 in `a76dd59`.
- **§8 (3/3)**: anonymous GET of both manifest URLs → 200, 247,019,226 B / 340,500,918 B, sha256
  exactly the manifest's; `docs/downloads.md` was still stamped 0.3.61 and is now 0.3.63.
- **Next run**: §7b's last box (every capability discoverable from gpncoin.ai — row 202's audio
  generation is absent from `website/ai.html`), then §9 deploy/confirm (a node updating itself).
  No decision needed; no [TRIAGE].

## 16:30 PT — 0.3.63 §6 DONE (2/2) against the published linux asset; windows run still building, handle kept [no triage needed]

- **A0**: `GET /commits/main` → `e571a50` (docs-only commit, `total_count: 0` runs — expected under
  paths-ignore). In-flight release runs are pinned to `build/1bd78d84250a`, so they are unaffected.
- **Release builds**: `44839998336` (linux) **completed / success**, asset `gpn-linux.tar.gz`
  published 00:05:57Z (340,500,918 B, sha256 `461f8b771cef0f43…db7b9b`). `44840000852` (windows)
  **in_progress** — steps through `build agent` green, `build node in_progress`, attest/sign/
  provenance-gate/publish all still `pending`; 37 min in, inside the 40-75 min cold-build budget.
  Handle kept in the box; not waited on.
- **Boxes ticked (§6, 2/2)**: downloaded the *published* linux asset via the asset API (the
  `releases/download/…` URL returns 9 bytes on this private repo — must use
  `/releases/assets/<id>` with `Accept: application/octet-stream`), sha256 matched the API digest,
  then `scripts/verify-release-artifact.py --version 0.3.63 --bundle linux=… --no-manifest` →
  **exit 0**, `version stamp 'gpn-node/0.3.63'`, `'fork-choice' x4`, `'gpn-forked' x2`,
  `'deep-fork' x3`, `'primary-authoritative' x3`, `'gpncs/2' x2`, `MAX_REORG_DEPTH = 288` ok;
  `./gpn_node --version` → `gpn_node 0.3.63 (spec 52, 1bd78d8)`.
- **Scope truth**: 0.3.63 contains **zero Rust changes** (`git diff --stat 2a15c16 1bd78d8 -- '*.rs'
  'Cargo.toml'` empty), so §6 box 1's "strings the change added" has nothing new to find; the tick
  is the previous batch's markers still present plus the correct version stamp, and it says so.
- **Next run**: first unticked box is §5 "attestation exists for the published digest". If
  `44840000852` is success, take the windows sha256 from the run/`windows-latest` and check the
  cosign bundle path (private repo → `attestations/sha256:` 404 is expected, not a failure); then §7.
  If failure, read the failing step's log first (it will be `build node` or the provenance gate).
- **Watch item**: if windows publishes, `C:\gpn\bin` on box 1 is replaced by that job — no mutating
  SSH to box 1 until it finishes.

## 15:53 PT — 0.3.63 §4 DONE (CI green on `1bd78d8`) and §5 builds dispatched; handles recorded, run ended [no triage needed]

- **CI (A0)**: HEAD `0e95cc9` is docs-only → `total_count: 0` (expected). In-flight run
  **44818515439** (`1bd78d8`) reached `conclusion: success` at 23:49Z — fmt+clippy ✅,
  secret-scan ✅, linux build ✅, **windows build ✅** (macos/GPU/audit skipped by design).
- **Box ticked**: §4 box 4 (CI green on that exact SHA), with the jobs API output quoted.
  §4 is now **DONE (4/4)**.
- **§5 (Build both platforms) — 5/7 boxes ticked**: per-item Windows evidence `n/a` (217 is a
  wording+gate item, no Windows proof); attestation-before-publish ordering verified in
  `release-windows.yml` by step order (678 → 692/698 → 720 no-`if:` gate → 748 publish), repo is
  private so the cosign path is live.
- **Dispatched, then ended the run** (dispatch-and-end rule):
  - `release-linux-selfhosted.yml` → run **44839998336**
  - `release-windows.yml` → run **44840000852**
  both on tag `build/1bd78d84250a` → sha `1bd78d8`, created this run because `workflow_dispatch`
  refuses a raw sha.
- **Still open**: §5 attestation-exists-for-published-digest + verifier boxes (need the builds),
  then §6 onward.
- **Next run**: ask GitHub once for 44839998336 and 44840000852. Both green → tick the two
  remaining §5 boxes from `releases.json`'s sha256 + cosign verify-blob, then §6 (string scan,
  `--version`). Either red → read that job's log first.

## 15:38 PT — 0.3.63 §4 box 4: windows job alive and progressing (node green, agent building); no tick, run ended [no triage needed]

- **CI (A0)**: HEAD `e07cc90` is docs-only → `total_count: 0` (expected). In-flight run
  **44818515439** (`1bd78d8`) — `fmt + clippy` ✅, `secret-scan` ✅, `Build + unit tests (linux)`
  ✅ (23:03Z), `Build + unit tests (windows, self-hosted)` **in_progress**.
- **Not stale, so not cancelled**: the windows job's own steps prove work is happening —
  `Build + test gpn-node (release)` completed **success** 22:33→23:11Z (~38 min, inside the
  documented 40–75 min cold build), `Build + test gpn-agent (release)` started 23:11Z and still
  running. Job runtime 60 min; run age 138 min. Cancelling here would discard a nearly-done build.
- **Nothing ticked**: §4 box 4 (*CI green on that exact SHA*) asserts the windows leg, which has
  not finished. Ticking it now would be a false claim; the handle is already in the box.
- **§5 attestation mechanism resolved without using the runner** (read-only, allowed while the
  runner is busy): `GET /repos` → `visibility: private`, `private: true`, owner `jeremyzguo`, so the
  GitHub-native attestation path is impossible and **cosign/Fulcio** is the mechanism.
  `release-windows.yml` already matches — method chosen from visibility, `cosign sign-blob
  --bundle`, and `Provenance gate - refuse to publish an unsigned artifact` at line 720 runs
  **before** `publish GitHub Release` at line 748. Recorded as a note under the §5 box, not a tick.
- **Next run**: ask once for 44818515439. `success` → tick §4 box 4, tick the §5 attestation-
  ordering box from the file:line evidence above, then **dispatch** `release-linux-selfhosted.yml`
  and `release-windows.yml` on `1bd78d8`, write both run ids, end. `failure` → read the windows
  job log first.

## 15:18 PT — 0.3.63 §4: linux leg went green on `1bd78d8`; only the windows job remains; box 3 ticked, run ended [no triage needed]

- **CI (A0)**: HEAD `82bc17f` is docs-only → `total_count: 0` runs (expected). In-flight run
  **44818515439** (`1bd78d8`) re-checked: `fmt + clippy` ✅, `secret-scan` ✅,
  `Build + unit tests (linux)` ✅ (was in_progress last run), `Build + unit tests (windows,
  self-hosted)` **still in_progress** (job started 22:32Z, so ~46 min of its own runtime).
  `status=in_progress`, `conclusion=None`. Not waited on past six polls, per the dispatch-and-end rule.
- **Boxes ticked (§4, 2/4)**: box 3 ("nothing else pushed in the same window") — this run's push is
  docs-only, so it starts no run and cannot supersede 44818515439.
- **Still open**: §4 box 4 (CI green on that exact SHA) — one job left; §5 onward untouched.
- **Next run**: ask GitHub once for 44818515439. `success` → tick box 4, move to §5 and **dispatch
  the two release builds, write the run ids, end the run**. `failure` → read the windows job log first.

## 14:52 PT — 0.3.63 §4: CI build for `1bd78d8` still running on both runners; handle recorded, run ended [no triage needed]

- **CI**: `GET /commits/main` → `3cfc4f2`; `actions/runs?head_sha=3cfc4f2` → `total_count: 0`
  (docs-only commit, paths-ignore) — expected, nothing waited on.
- **In-flight run**: **44818515439** on `1bd78d8` (created 21:14Z, jobs started 22:32Z) —
  fmt+clippy **success**, secret-scan **success**, `Build + unit tests (linux)` and
  `(windows, self-hosted)` **in_progress** since 22:32–22:35Z. Seconds-ago `runners` API:
  `DESKTOP-USBUADL` busy=true, `openclawassist` busy=true — contention, not a fault.
- **Not cancelled**: the run is 98 min old by creation but the builds themselves are only ~20 min
  in and progressing; cancelling would discard live work. The 90-minute rule is for a *stale*
  run, not a queued-then-started one.
- **Boxes left open on purpose**: §4 "nothing else pushed in the same window" and "CI green on
  that exact SHA" — both assert the run's outcome; cannot be ticked from a dispatch. Handle is
  already in the box, so the next run asks GitHub once and ticks or re-dispatches.
- **No push this run** except this docs-only LOOP-LOG commit (starts no CI run).
- **Next run**: if 44818515439 is green → tick both §4 boxes, then §5: dispatch
  `release-linux-selfhosted.yml` + `release-windows.yml` on `1bd78d8` (or the code SHA the next
  run finds as HEAD-of-code), record handles, end the run. If red → read the failing job log and
  fix the cause first.

## 14:20 PT — 0.3.63 §3b row 217: the gate is honest and calibrated; its first green belongs to 218 [no triage needed]

- **CI**: `GET /commits/main` → `ba7cc96`; `actions/runs?head_sha=ba7cc96` → `total_count: 0`
  (docs-only, paths-ignore) — expected, nothing waited on.
- **Box ticked**: §3b row 217 — as *this batch's scope*, per the recorded decision
  (`NEEDS-TRIAGE.md`, 11:58 PT, option A): 0.3.63 ships the restated acceptance + the gate, the
  live green is 218's and rides the release carrying `fe71e35`. Row 217 stays IN-PROGRESS.
- **Evidence**: `--self-test` → `PASSED (7/7)`; live `--journal-unit gpn-fleet` → window
  335795..335894, mean **19.83 s**, median 14.06 s, max **87.21 s**; clause 1 (10× mean = 198.3 s)
  **0 violations**; clause 2 → 4 suspect, **4 unexplained** (62.24 / 87.21 / 80.75 / 61.17 s) →
  `RESULT: FAIL`. Caller is live: `gpn-cadence-gate.timer` → `active`, state file
  `{"checked_at":"2026-11-20T22:01:10Z","exit":1,"violations":4,"max_gap_s":112.26}`.
- **Finding (why FAIL is the right state)**: clause 2 needs a cause in the node's own output, and
  the `sealer: still searching` heartbeat exists only on `fix/218-sealer-heartbeat` — not in this
  build. So the gate cannot pass by luck, and a wording-only batch has no runtime claim to prove.
  Went looking in the journal for one offending window (13:32:18–13:34:10 PT, the 112 s hole):
  the node was alive and dialling (`peer-priority: dialled 1/4 …`, `chain-work` every 30 s) but
  emitted no sealing line at all. That is 218's blind spot, not a gate defect.
- **Next run**: §3 box 1 (`bump-version.py patch`, already done in `2a15c16`) and box 3
  (`--check` → `lockstep: 15 manifest(s) and 14 lockfile entr(ies) agree`, verified this run),
  then §4 push rules and §5 dispatch.

## 13:55 PT — 0.3.63 §2 box 6 (the change is in the tree) verified against committed objects [no triage needed]

- **CI**: `GET /commits/main` → `1ffb9ca`; `actions/runs?head_sha=1ffb9ca` → `total_count: 0`
  (docs-only, paths-ignore) — expected, nothing waited on.
- **Box ticked**: §2 box 6 (*the change is in the tree you are about to release*). Proof is
  `git log` on the four artefacts plus `git ls-files`, not a grep of the working copy:
  `42f1089` → `3ad07e3` → `1bd78d8` touch `scripts/check-block-cadence.py`,
  `scripts/cadence-gate-watch.sh`, `scripts/systemd/gpn-cadence-gate.{service,timer}`; all four are
  tracked, and the two *installed* units are byte-identical to the committed ones (`diff -q`, no
  output). `git diff --stat ce5bc5e..HEAD -- '*.rs'` **empty** — the positive form of "no runtime
  change", which `Cargo.lock` in the diff list could otherwise cast doubt on.
- **Note left for §6**: this release has no buildable Rust artefact, so the "scan the binary for
  strings the change added" box has to read `scripts/*.sh|py` + the units, not `gpn-node`.
- **Pushed**: docs-only commit (checklist + this log). No CI run expected; the runner cost is zero.
- **Runner state (not a fault, checked because it will matter for §5)**: run `44807133030` for
  `3ad07e3` is genuinely still executing — `fmt+clippy` success at 21:14:36Z, linux build success
  21:41, windows self-hosted still compiling `gpn-agent` (started 21:54). It waited 19:58→21:14 in
  *queue*. So there is no stuck job to cancel; §5's dispatch will simply queue behind it. Run
  `44818515439` (`1bd78d8`) is `pending` behind it for the same reason.
- **Next run**: §3b row 217 (read as *measured by the gate*, not proven by a binary), then §3
  bump/lockstep.

## 13:40 PT — 0.3.63 §2 box 5 + §3 changelog: the release said it changed code it does not contain [no triage needed]

- **CI**: no run for HEAD `b0b4dc6` (docs-only, paths-ignore) — checked via `actions/runs?head_sha=`,
  `total_count: 0`. Nothing waited on.
- **Box ticked**: `0.3.63.md` §2 box 5 (*nothing new is off by default behind a flag nobody sets*).
  Proof is a diff, not a grep: `git diff --stat ce5bc5e..HEAD -- '*.rs'` is **empty**, so no flag can
  have been added. The only new executable artefact is the cadence-gate user timer, and it is *on*:
  `systemctl --user is-enabled gpn-cadence-gate.timer` → `enabled`, LAST `13:31:18 PT` (1 min before
  the check), state `checked_at 2026-11-20T21:31:18Z`, exit 1 (7 unexplained gaps >3× mean, 46–80 s).
- **Box ticked**: `0.3.63.md` §3 box 2 (*CHANGELOG section written*) — but only after rewriting it.
- **Defect found and fixed**: the 0.3.63 CHANGELOG claimed *"this version tunes the retarget to the
  field it runs in (window/step sized for 3–6 miners)"*. That was **false** — zero Rust files differ
  from 0.3.62. §6 (scan the binaries for strings the change added) would have gone hunting for a change
  that does not exist, and the published changelog would have described a release nobody built. Rewritten:
  leads with "This version contains no runtime change", lists what does ship (the gate + timer, the
  restated acceptance), and tells an operator upgrading for cadence to wait for the release carrying
  #217's retarget change. Cause: the driver copied #217's *intent* into `### Changed` while the batch was
  deliberately a wording-only cut.
- **Also noted**: 77 `_TODO_` lines survive in already-published sections 0.3.0–0.3.43 (CHANGELOG.md:796+).
  Historical hole, not this release's; not rewritten.
- **Pushed**: `35e86db` (docs only — second docs commit this run; no CI run started for it,
  verified `total_count: 0`, so the runner cost was zero).
- **Next run**: §2 box 6 (*the change is in the tree you are about to release*), then §3b row 217, then
  §3 bump/lockstep. Note for whoever gets there: 0.3.63 has no code change, so §6's string scan and
  §3b's "done when" for 217 both need to be read as *measured by the gate*, not *proven by a binary*.

## 13:15 PT — 0.3.63 §2 box 4: the cadence gate now has a caller (user timer + persisted state) [no triage needed]

- **CI**: no run existed for HEAD `be509d9` (docs-only commit, paths-ignore) — expected, nothing waited on.
- **Box ticked**: `0.3.63.md` §2 box 4 (*a call site exists*). The only new artefact in 0.3.63 is
  `scripts/check-block-cadence.py` and it had no invocation path — the same shape as
  `check-deploy-drift.sh` before its watcher. Added `scripts/cadence-gate-watch.sh` (runs the gate
  against `127.0.0.1:9944`, attributes from `--user -u gpn-fleet`, keeps `~/.gpn/cadence-gate-sample.json`,
  writes `~/.gpn/cadence-gate.state`) and installed it as **`gpn-cadence-gate.timer`** (`OnCalendar=*:0/30`,
  user unit). Units committed under `scripts/systemd/` so another box can install the same caller.
- **Observed**: `systemctl --user list-timers` → next `Fri 2026-11-20 13:30:45 PST`; forced run at
  13:13:57 PT → `Result=exit-code / ExecMainStatus=1`; state file
  `{"checked_at":"2026-11-20T21:13:57Z","exit":1,"violations":3,"max_gap_s":51.96}`; journal line
  `sample written : /home/jeremyg/.gpn/cadence-gate-sample.json`. The non-zero exit is the honest
  verdict — the gate is failing on 3 unexplained gaps (34.7 / 52.0 / 46.6 s, 3.0–4.5× the 11.49 s mean),
  which is row 218's work and rides the next code release.
- **Fixed on the way**: first wrapper run persisted an empty `message` — the gate output was piped into a
  heredoc-owned stdin, so `sys.stdin.read()` saw nothing. Now passed via `GATE_OUT`; the state above is
  from the fixed code.
- **Pushed**: `1bd78d8` (scripts + checklist; not docs-only, so CI will start for it — not waited on).
- **Next run**: §2 box 5 (*nothing new is off by default behind a flag nobody sets*).

## 12:5x PT — 0.3.63 §2: full test suites green (node 752/0, agent 1164/0) [no triage needed]

- **CI**: no run for HEAD `69b9c3c` (docs-only, paths-ignore) — expected, nothing waited on.
- **Box ticked**: `0.3.63.md` §2 box 3 (*Full affected suites run, not just the new test*).
  `gpn-node` `cargo test --release` exit 0 — **752 passed, 0 failed**; `gpn-agent`
  `cargo test --release` exit 0 — **1164 passed, 0 failed**; both compiled fresh under
  `CARGO_BUILD_JOBS=2` on main `69b9c3c` (rustc 1.98.1). Gate self-test 7/7 stands from §2 box 1.
  Only warnings: pre-existing `dead_code` in `gpn_gov_upgrade` / `gpn_provider_status` bins.
  Logs: `/tmp/gpn-node-test-0363.log`, `/tmp/gpn-agent-test-0363.log`.
- **Pushed**: `965900f` (docs-only). Note: remote main had moved to `62be9f3` between check and push
  (concurrent commits `8d27790`, `62be9f3` landed on the same checkout); my push was a clean
  fast-forward on top, nothing of theirs rewritten.
- **Next run**: §2 box 4 (*a call site exists* — grep the caller).

## 12:10 PT — 0.3.63 §2: clause 2 decided (attribution, not an absolute ceiling) and the gate's clause logic now has a passing test [no triage needed]

- **CI**: no run for HEAD `d05de76` (docs-only, paths-ignore) — expected, nothing waited on.
- **Triage answered, not re-raised**: the standing [TRIAGE] item ("clause 2 is unsatisfiable at a
  15 s target") is decided **A** — clause 2 is now "every gap above 3x the window mean has a cause
  in the node's own output"; clause 1 (10x mean) stays. Rationale: at a 15 s mean the arrival
  process yields ~5 gaps >45 s per 99 (0.4 % clean windows), so the absolute form measured the
  chain's arrival process rather than our defect. Recorded in `docs/NEEDS-TRIAGE.md`.
- **Box ticked**: `0.3.63.md` §2 box 1. `scripts/check-block-cadence.py` gained `--self-test`
  (7/7 cases, output pasted in the box) and `--journal-unit`/`--log` attribution against the real
  literals emitted by `fe71e35` (`sealer: still searching`, `sealer: slow solve`,
  `seal submit for pre_hash`). The live run still FAILS and is meant to: 11:58 PT, blocks
  335294..335393, mean 13.83 s, three unexplained gaps (47.7 / 53.5 / 65.3 s) — the heartbeat that
  explains them is on `fix/218-sealer-heartbeat`, which rides 218's release.
- **Sequencing, stated once**: 0.3.63 remains the wording + gate batch; the live green is 218's
  acceptance and moves with `fe71e35`. Nothing else pushed.
- **Next run**: §2 box 2 (`cargo fmt --check` / clippy).

## 12:20 PT — 0.3.63 §2: fmt + clippy green on `3ad07e3` (both crates, CI's exact allow-list)

- **CI**: run `44807133030` for main HEAD `3ad07e3` is **pending** (queued; `44800020725` for
  `42f1089` still in_progress on the single runner). Not waited on, per A0.
- **Box ticked**: `0.3.63.md` §2 box 2 (*`cargo fmt --check` clean; clippy `-D warnings` clean*).
  rustc/cargo 1.98.1 = CI's pinned `RUST_TOOLCHAIN`. `cargo fmt --all -- --check` exit 0 in
  `gpn-node` and `gpn-agent`; `cargo clippy --release --workspace --all-targets -- <ci.yml
  CLIPPY_ALLOWS> -D warnings` exit 0 in both (`Finished release profile` 54.04 s and 20.49 s; only
  the upstream `subxt v0.37.0` future-incompat note). Allow-list copied verbatim from `ci.yml`.
- **Next run**: §2 box 3 (*full affected suites run*) — `cargo test -p gpn-node --bin gpn-node` and
  the agent suite, since this batch has no runtime change but the gate script is ours.
- No blockers, no [TRIAGE].

## 12:20 PT — 0.3.63 §2: row 218 instrumented (sealer heartbeat), third stall found; gate clause 2 shown unsatisfiable [TRIAGE]

- **CI**: no run exists for HEAD `1300b28` (docs-only, paths-ignore) — expected. The queued run
  `44800020725` for `42f1089` was pending behind `44794237787` (in_progress) all run, single runner;
  not waited on, per A0.
- **Box worked**: §2 box 1 (*the test passes*). It cannot be ticked this release and the checklist
  now says why: the gate is `scripts/check-block-cadence.py` against the **live** chain, and the
  fix rides the next code release (assistant A′, 11:07 PT). No fake tick.
- **Diagnosis of 218 (logs first)**: a **third** silent stall reproduced live inside these very
  samples — `Prepared … at 335233` 11:15:18 → next import 11:17:34 = **135.7 s**, `best=#335232`
  on all three peers throughout, `grep -Ei 'error|panic|fatal'` = 0 hits, and the node's own
  `💤 Idle` lines kept printing every 5 s, so the *process* was healthy and only the **sealer**
  was silent. That excludes "node hung" and leaves GPU-seal / submit / template with no witness.
- **Fix (branch `fix/218-sealer-heartbeat`, commit `fe71e35`, not main)**: `SealWatch` — one INFO
  heartbeat per 20 s while the same template stays unsolved (fingerprint, elapsed, nonce windows),
  a WARN when a solve took ≥45 s, and a WARN when `worker.submit` blocks >2 s. Row 218's done-when
  ("every gap above 3× the mean has a cause in our output") becomes checkable by construction.
  Evidence: 5 new unit tests pass; `cargo test -p gpn-node --bin gpn-node` = **293 passed, 0 failed**;
  `cargo fmt --check` clean; clippy with CI's documented allow-list `-D warnings` = clean.
  Held off `main` on purpose: 0.3.63 is the wording+gate batch and a runtime change would ride it.
- **Why the gate cannot go green as worded [TRIAGE]**: at the measured mean (15.4–16.6 s) a
  memoryless hashing race puts **5.3 gaps >45 s per 99** (P = e^-45/16.6 = 5.4 %), so a clean
  100-block window has probability 0.4 %. Both live samples hit the prediction exactly (4 and 6
  violations) — they are the arrival process, not defects. Recommended clause 2 = 218's
  attribution rule. Full arithmetic + options in `docs/NEEDS-TRIAGE.md` (last entry).
- **Next run**: read the triage answer; if A, restate clause 2, merge `fe71e35` into the next
  bundle; if the gate is to stay absolute, the target block time has to move to ~6 s.

## 11:14 PT — 0.3.63 §1: cadence gate written, run live, FAILS (as it must); row 218 reproduced

- **CI**: no run for HEAD `c1651ee` (docs-only, paths-ignore) — expected. This run's push `42f1089`
  touches `scripts/`, so a run will start; not waited on.
- **Triage consumed**: assistant's 217 A′ decision (`c1651ee`) is now reflected in the checklist and
  the list row — 217 is **not** blocked any more, it is simply not met. 0.3.63 = wording + gate +
  checklist batch, no runtime change.
- **Box ticked**: `0.3.63.md` §1 box 2 + box 3(n/a). New `scripts/check-block-cadence.py` encodes
  the restated acceptance (no gap > 10× mean AND none > 45 s, chain time from each block's
  `timestamp.set` extrinsic). Live at 11:12 PT, head #335216, exit 1: mean 15.38 s, **median
  9.08 s**, 4 gaps >45 s (50.8 / 153.0 / 66.0 / 88.7). 153.04 s = 9.95× mean — the ratio clause
  alone would have passed a 2.5-minute chain stop, which is exactly why the absolute clause exists.
- **Row 218 reproduced** inside that sample: #335159→#335160, `Prepared … 335159 (114 ms)` at
  10:55:38, **no `GPU sealed block` for 155 s**, all 3 peers level at #335158, seal at 10:58:13 then
  instant import; `error|panic|fatal` grep = 0 hits. Two occurrences, both silent sealer stops.
- **Next (next run)**: §2 box 1 — the test must pass, i.e. fix the sealer silence (row 218) so the
  gate goes green. No decision needed; no [TRIAGE].

## 10:53 PT — 0.3.63 §1: named the cadence target, measured the 153 s stop, split it out as row 218 [TRIAGE]

- **CI**: no run exists for HEAD `aede251` — it is a docs-only commit and `ci.yml` paths-ignore covers
  `docs/**` and `**/*.md`, so "no run" is the expected state, not a red.
- **Box ticked**: `0.3.63.md` §1 box 1 (name the behaviour + machine-observable proof). Behaviour:
  *no inter-block gap above 10× the 100-block running mean*. Observable: per-block `Timestamp::Now`
  via `chain_getBlock` on :9944 plus this node's `Imported #N` lines.
- **Measurement** (09:05–09:45 PT, 175 imports, #334681→#334872): one 153.0 s gap,
  #334818 09:33:18 → #334819 09:35:51 = 9.6× the 16.0 s mean. It *passes* a 10× threshold by 6 %,
  which is exactly why the threshold alone is the wrong gate.
- **New finding**: the gap is not an arrival tail. It sits at the window's *lowest* difficulty
  (this node's own `chain-work:` increments: 34,844,807 for #334817–#334819 vs 69,689,614 and
  104,534,421 in the same 20 min), where the expected gap is ~8 s, so p ≈ e^-19. This node started a
  consensus session and prepared #334819 at 09:33:18, then logged no `GPU sealed block` for 232 s
  while all 3 peers sat at `best=#334818`. No error, no restart, no cause recorded → **row 218**.
- **Not done on purpose**: no code written for 217. Its acceptance wording is unsatisfiable
  (6 vs 5.95 gaps >45 s and 17 vs 16.93 gaps <3 s = the arrival process exactly), so an
  implementation would be aimed at a target that cannot exist. Row 217 → `AWAITING-OWNER`.
- **[TRIAGE]** `docs/NEEDS-TRIAGE.md` 217/218 split: recommend answering 217 as **A** (restate the
  target) *and* keeping 218 as the real fix, because A alone would sign off a silent 2.5-minute stop.
  The sequencing question I am not deciding: 0.3.63 carries no code, so either it ships as a
  wording-only batch or it is rolled into the next real one.



## 14:50 PT — 0.3.63 §3 done (lockstep + seed parity); §4 staged, CI run 44818515439 in flight

- **CI (A0)**: `GET /commits/main` → `072d46e`; `actions/runs?head_sha=072d46e` → `total_count: 0`
  (docs-only) — expected. In-flight run **44818515439** on `1bd78d8` (newest `scripts/` commit):
  queued 21:14Z, jobs only started 22:32Z (~78 min queue behind the single-slot Windows runner);
  fmt+clippy ✅, secret-scan ✅, linux + windows builds still running at 22:45Z. Not waited on.
  Previous code SHA `2a15c16` = run 44794237787 **success** (fmt/clippy, linux, windows, secret-scan).
- **Boxes ticked (§3, 3/3)**:
  - `bump-version.py patch` — already applied in `2a15c16` (`git log -S'version = "0.3.63"' -- gpn-agent/Cargo.toml`);
    re-running it would have made 0.3.64 and orphaned the checklist, so it was verified, not repeated.
  - `bump-version.py --check` → `lockstep: 15 manifest(s) and 14 lockfile entr(ies) agree`, every row `ok 0.3.63`.
  - Seed parity — `cargo test --test shipped_bootnodes --test launcher_bootnodes` = **8 passed, 0 failed**
    (7m01s compile, tests 0.17s total).
- **§4**: box 1 ticked (`total_count: 0` for the docs SHA, with `1bd78d8` queueing a run as the
  control — paths-ignore is selective); box 2 satisfied by this single docs commit.
  Boxes 3 and 4 left OPEN with the run handle above; next run asks GitHub once for 44818515439.
- **Next run**: if 44818515439 is success → tick §4 boxes 3/4 and move to §5 (build both platforms,
  dispatched and ended, not waited on). If failure → read the failing job's log first.
