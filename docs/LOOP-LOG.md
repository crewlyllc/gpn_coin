## 00:52 PT — both release builds dispatched on the CI-green sha; §5 SSH-evidence box ticked; run ended (not waited on)

- **A0**: `origin/main` HEAD `8dea06a2577464d04ab13592864387e4ad711e67` (checklist docs commit) →
  `?head_sha=8dea06a…` ⇒ `total_count: 0` — correct under `ci.yml` paths-ignore (docs only). No
  CI failure to act on. Local HEAD == origin/main, nothing stale.
- **Box 1 — §5 "Per-item Windows evidence was collected over SSH".** Ticked from the §3b notes: all
  four Windows proofs (233 `LIVE233 RESULT: PASS` with update.log taskkill→schtasks ordering and
  node pid 41048; 234 live `GetSystemMetrics(8192)`⇒0 + 39 `CTRL_CLOSE_EVENT` lines; 235
  `live-defer-235.ps1` on the box's real last-exit.json; 236 real `Get-ShutdownState` ⇒
  `pending=True … PendingFileRenameOperations is non-empty`) came from `scripts/box1.sh`, not dispatches.
- **Dispatched, then ended the run (dispatch rule).** Tag `build/88f2eea74788` created and pushed at
  sha `88f2eea…` (the run-44987167844-green release sha; `git diff --stat 88f2eea HEAD` = docs only,
  so the artifact bytes are the CI-verified tree):
  - `release-linux-selfhosted.yml` → run **45004843977** (in_progress 08:52:38Z)
  - `release-windows.yml` → run **45004869470** (in_progress 08:52:58Z)
  Both handles are written into their boxes; no polling, no waiting.
- **Next run**: ask GitHub once for those two run ids; if success, tick §5 boxes 2/3, then §6
  artifact scans against the published digests.

## 00:40 PT — §2 last box ticked ('the change is in the tree about to be released'); §2 closed 6/6

- **A0**: `origin/main` HEAD `401ad10e069b9bd27148ef1e57a9fe54c17374eb` (LOOP-LOG docs commit) →
  `?head_sha=401ad10…` ⇒ `total_count: 0` — correct under `paths-ignore`. Nothing to act on.
- **Box — §2 "The change is in the tree you are about to release" (was the only unticked box in §2).**
  - `git log --oneline -3 -- install.ps1` ⇒ `e9e2890` (236), `78684d8` (235), `74a17a6` (234);
    `gpn_node.rs`/`exit_report.rs` top commit `74a17a6`.
  - Symbols present in HEAD itself (not just history): `install.ps1` 15 hits over
    `Start-NodeAfterStop|Get-LoaderExit|Get-ExitCauseNote|Get-ShutdownState|Show-UpdateDeferred`;
    `gpn_node.rs` 7 hits `classify_shutdown_event|record_shutdown_meaning`; `exit_report.rs`
    `:129 slug: "console-teardown"`, `:136 slug: "system-shutdown"`.
  - `git diff --stat 88f2eea HEAD -- install.ps1 gpn-agent/src gpn-agent/Cargo.toml scripts/` ⇒ **empty**;
    `git merge-base --is-ancestor 88f2eea HEAD` ⇒ true. The tree about to be released is byte-identical
    in code to `88f2eea`, the sha CI run 44987167844 built green.
- **Next run**: §5 box 1 (release-linux-selfhosted.yml) — dispatch and END the run per the dispatch rule.

## 00:3x PT — flag box + §4 CI-green ticked; release sha 88f2eea is green on all three required platforms

- **A0**: `origin/main` HEAD `195022dd` (docs-only) → full-40 `?head_sha=` ⇒ `total_count: 0`, correct
  under `paths-ignore`. No failure to act on.
- **Box 1 — §2 "Nothing new is off by default behind a flag nobody sets" (line 229).** Swept, not eyeballed:
  - crate has **no `[features]` table** — the 8 `features` hits in `gpn-agent/Cargo.toml` are all
    dependency features (`tokio` full, `serde` derive, `libp2p` tcp/noise/yamux/dns/tokio/websocket, …);
  - `grep -c 'cfg(feature'` in `gpn_node.rs` / `exit_report.rs` ⇒ `0` / `0`; 0 `AtomicBool`/`static bool`
    gates on added lines;
  - `install.ps1` diff vs pre-change `78684d8`: **0** added `$env:` reads, **0** added `[switch]` params.
    The only new params are `Get-ShutdownState`'s injectable probes (`:1368`, default `$null`) — and
    `$null` means the *real* probe runs (`:1372-1382` `GetSystemMetrics(0x2000)`, `:1384-1398`
    PendingFileRenameOperations / CBS RebootPending / WU RebootRequired);
  - the call is straight-line: `$sd = Get-ShutdownState` at `:1911`, before `Stop-NodeTree 60` `:1921`;
    Rust arm `CTRL_SHUTDOWN_EVENT => { let gate = …GetSystemMetrics(SM_SHUTTINGDOWN); match
    classify_shutdown_event(gate) }` at `gpn_node.rs:2024-2045` with nothing in front of it.
- **Box 2 — §4 "CI green on that exact SHA".** Run **44987167844** on `88f2eea` ⇒ `completed success`
  (04:55:47Z → 06:52:37Z). Per job: secret-scan success; fmt+clippy success; linux success (step 13
  `Autostart installers` green — the assertion `88f2eea` fixed); **Windows self-hosted success**
  (05:34:50Z → 06:52:37Z, 77.8 min). macOS/GPU-testnet/audit `skipped` as on every push.
- **Release sha is `88f2eea`.** Checklist commit `d799410` is docs-only ⇒ no run expected.
- **Next run**: first unticked box is §2 "The change is in the tree you are about to release", then §5
  (Windows final build via runner + linux build), §6, §7. Nothing needs Jeremy; no [TRIAGE].

## 23:5x PT — §2 "a call site exists" ticked by mechanical sweep, not by a hand-picked list

- **A0**: `origin/main` HEAD `d631c43` (docs-only) → `?head_sha=` full-40 ⇒ `total_count: 0`, correct
  under `paths-ignore`. The code/release sha `88f2eea` has run **44987167844**, API-read this turn:
  `completed success`, 2026-11-22T04:55:47Z → 06:52:37Z. CI is green on the release commit.
- **Working tree was stale again**: local HEAD `df75123` vs `origin/main` `d631c43` (2 commits behind).
  `git pull --ff-only` first, then worked — the SS2/SS3 ticks were already there.
- **Box worked — §2 "A call site exists for anything new" (line 228).** Instead of listing the new
  symbols by hand (the way election/self-heal got missed), swept mechanically:
  - `install.ps1`: 74 `function` definitions, every name referenced outside its own definition line ⇒
    **0 defined-but-uncalled**.
  - `exit_report.rs` 24 non-test fns, `gpn_node.rs` 128 non-test fns ⇒ all referenced (only `#[test]`
    handlers are unreferenced, filtered by attribute).
  - The bundle's own symbols with their production call sites: `Start-NodeAfterStop` def 1757 →
    calls 1927/1940/2004/2037/2053/2082; `Get-LoaderExit` 1321→1998; `Get-ExitCauseNote` 1284→1986;
    `Get-ShutdownState` 1367→1911 (before `Stop-NodeTree` at 1921); `Show-UpdateDeferred` 1041→2017;
    Rust `classify_shutdown_event` 1978→2026, `record_shutdown_meaning` 2072→2028/2038.
  - note: the single bare `& schtasks /Run /TN $NodeTask` is inside the `Start-NodeAfterStop` catch
    (documented last-resort fallback), not an uncalled path.
- **Next run**: first unticked box is §2 "Nothing new is off by default behind a flag nobody sets."
  then §2 "the change is in the tree you are about to release", then §4's CI box (already evidenced by
  44987167844) and §5 (dispatch both platform builds on `88f2eea`, write handles, end the run).
- No blocker, no [TRIAGE].

## 23:45 PT — §2 box 3 ticked: full affected suites run in full (785 lib + 34 bin rust, 8/8 script suites) [no triage]

- **A0**: `origin/main` HEAD `72ad930` (docs-only ⇒ `?head_sha=72ad930…` `total_count: 0`, correct under
  `paths-ignore`). Release build sha stays `88f2eea`, whose CI run **44987167844** is **success**.
- **Box ticked**: §2 `- [ ] Full affected suites run, not just the new test.` Worktree `/tmp/rel67` @ `72ad930`:
  `cargo test --release --lib` ⇒ `785 passed; 0 failed; 14 ignored`; `cargo test --release --bin gpn_node`
  ⇒ `34 passed; 0 failed`; pwsh 7.4.6 `test-update-defer` / `-exit-cause` / `-shutdown` / `-start-after-stop`
  all `RESULT: PASS (all checks)`, `test-update-confirm` `FAILURES: 0`, `test-defender-exclusions` all passed;
  bash `test-launcher-identity` `RESULT: PASS`, `test-autostart-installers` all assertions passed. All EXIT 0.
- **Blockers**: none. **Triage**: none.

## 23:28 PT — §2 box 2 ticked: fmt clean + clippy green with CI's exact allow-list (my first attempt used the wrong flags; that was the cause of the 33 errors) [no triage]

- **A0**: HEAD `f42c915`, `?head_sha=f42c915…` is docs-only ⇒ no run. Release CI run **44987167844**
  (`88f2eea`) already recorded by the previous run as **success** (linux+windows+lint green).
- **Box ticked**: §2 `- [ ] cargo fmt --check clean; clippy with CI's -D warnings clean.`
  In detached worktree `/tmp/rel67` @ `f42c915`, toolchain 1.98.1:
  `cargo fmt --check` ⇒ exit 0, no diff; `cargo clippy --release --all-targets -- <ci.yml CLIPPY_ALLOWS> -D warnings`
  ⇒ `Finished release profile [optimized] target(s) in 11.18s`, no errors (only upstream `subxt v0.37.0`
  future-incompat note, which CI prints too).
- **Failure diagnosed, not parked**: attempt 1 ran plain `cargo clippy --all-targets -- -D warnings` and reported
  33 `type_complexity`/etc. errors. Cause = my command omitted the documented `CLIPPY_ALLOWS` allow-list that
  `ci.yml` passes (29 `-A` entries); the code was never at fault. Re-ran with the list parsed out of `ci.yml`.
- **Blockers**: none. **Triage**: none.

## 23:55 PT — §2 opened: "the test passes" ticked with a same-turn run of all five pwsh suites + rust suites on the release tree; run 44987167844 went GREEN (linux+windows+fmt/clippy) [no triage]

- **A0**: `origin/main` HEAD `68d7116` (docs-only checklist commit ⇒ `total_count: 0` under
  `paths-ignore`; no CI to read, correctly). Release sha stays `88f2eea`; its run **44987167844**
  read once, not waited on: `secret-scan` ✅, `fmt + clippy` ✅, `Build + unit tests (linux)` ✅,
  `Build + unit tests (windows, self-hosted)` ✅, overall **completed success 2026-11-22T06:52:37Z**.
  That clears §4's CI box (to be ticked in order).
- **Box worked — §2 "The test passes."** Re-ran everything on a detached worktree of the release
  tree (`/tmp/rel67` at `68d7116`, so the single-slot runner's checkout was untouched): pwsh
  `test-update-defer` PASS (44 checks), `test-update-exit-cause` PASS, `test-update-shutdown` PASS
  (`ok I it leaves the node running`), `test-update-start-after-stop` PASS (`ok E: ... parent are
  recorded`), `test-update-confirm` `FAILURES: 0`; bash `test-autostart-installers.sh` ⇒
  `all autostart/installer assertions passed` EXIT=0; `cargo test --lib exit_report` ⇒ 11 passed,
  `cargo test --bin gpn_node` ⇒ 34 passed, 0 failed. Ticked with that evidence.
- **Next run**: §2 box 2 — `cargo fmt --check` + clippy `-D warnings` on the same tree, then the
  remaining §2 boxes (full suites, call-site grep, no off-by-default flag, change present in tree).
- No blocker, no [TRIAGE].

## 22:35 PT — §1 closed: the "cannot be tested" box is n/a, both pwsh suites re-run PASS this run; CI Windows job still building [no triage]

- **A0**: `origin/main` HEAD `ac47db7` (docs-only ⇒ `?head_sha=` full-40 would give `total_count: 0`
  under `paths-ignore`). Release sha stays `88f2eea`; its run is **44987167844**, read once from the API:
  `secret-scan` ✅, `fmt + clippy` ✅, `Build + unit tests (linux)` ✅ 06:02:58Z (incl. step 13 autostart
  installers), `Build + unit tests (windows, self-hosted)` **in_progress** at step 7 `Build + test gpn-agent
  (release)` since 05:34:50Z (~57 min — under box 1's normal build time and the 90-minute threshold).
  **Not waited on, not cancelled.**
- **Box worked — §1 "If the change cannot be tested, stop and tell Jeremy"** ticked **n/a** with
  same-turn evidence: no 0.3.67 change is untestable. On the clean release tree (`e9e2890`, `git status`
  empty), pwsh 7.4.6: `test-update-defer.ps1` ⇒ `RESULT: PASS (all checks)` EXIT=0
  (`ok L it names 0xC000026B (signed)`, `ok L it files it as session-closing`, `ok ascii the wrapper
  template is pure ASCII`); `test-update-shutdown.ps1` ⇒ `RESULT: PASS (all checks)` EXIT=0
  (`ok I it leaves the node running`). §1 is now fully ticked.
- **Next run**: §2 Implement — `cargo fmt --check` + clippy `-D warnings`, full affected suites,
  call-site grep, no off-by-default flag, change present in the release tree. Then §4's CI box once
  the Windows job concludes.
- No blocker, no [TRIAGE].

## 22:12 PT — §4 CI box observed, not waited on: linux job green (step 13 fixed), Windows build still running

- **A0**: `origin/main` HEAD `df75123` (docs-only; `?head_sha=` full-40 ⇒ `total_count: 0`, correct under
  `paths-ignore`). The release sha stays `88f2eea`; its run is **44987167844**.
- **Run 44987167844, read once from the API (not waited on)**: `secret-scan` ✅ 05:36:31Z,
  `fmt + clippy` ✅ 05:38:24Z, `Build + unit tests (linux)` ✅ 06:02:58Z — **step 13
  `Autostart installers (Windows + Linux, reversible)` success**, i.e. the two assertions `88f2eea`
  repaired now pass in CI (that step was the failure in 44984299924). `Build + unit tests (windows,
  self-hosted)` still `in_progress` at step 7 `Build + test gpn-agent (release)` since 05:34:50Z
  (~37 min; under both the normal box-1 build time and the 90-minute threshold).
- **Box worked — §4 "CI green on that exact SHA"**: left unticked with the per-job evidence recorded
  inline. Ticking it needs the Windows job's conclusion, which does not exist yet; the box is not a
  build/publish step I can dispatch, and the run is healthy, so nothing was cancelled or re-dispatched.
- Local tree fast-forwarded to `df75123`; working tree was clean before the edit.
- **Next run**: read 44987167844 once more. Windows job success ⇒ tick §4, then §2 (fmt/clippy/
  call-site/tree) and open §5 with dispatch handles. Windows job failure ⇒ quote the failing step line.
- No blocker, no [TRIAGE].

## 21:5x PT — §1 fail-before box ticked with same-turn re-runs; CI run 44987167844 healthy and still building

- **A0**: `origin/main` HEAD `4b358fa` (docs-only, `?head_sha=` full-40 ⇒ `total_count: 0` — correct under
  `paths-ignore`). The code/release sha stays `88f2eea`; its run is **44987167844**.
- **Run 44987167844 status, read from the API**: `secret-scan` ✅, `fmt + clippy` ✅ (05:38Z),
  `Build + unit tests (windows, self-hosted)` in progress at step 6, `Build + unit tests (linux)` in
  progress at step 7 — i.e. running, ~60 min in, under the 90-minute build threshold. **Not waited on.**
- **Box worked — §1 "Write the test that fails now".** Re-produced the fail-before in this turn instead of
  quoting old notes: with `78684d8^:install.ps1` (no `LoaderCodes`/`Get-LoaderExit`, `grep -c` ⇒ 0),
  `test-update-defer.ps1` ⇒ `FAILURES: 38` (`FAIL K the branch reports through it  not called`,
  `FAIL L it names 0xC000026B (signed)  missing`, …) and `test-update-shutdown.ps1` ⇒ `FAILURES: 13`
  (`FAIL I it leaves the node running  reassurance missing`). Pass-after on the release tree:
  `RESULT: PASS (all checks)`.
- **Correction to the earlier record**: the first attempt used `78684d8:install.ps1` and got
  `RESULT: PASS` — because 78684d8 *is* the commit that added 235. The pre-change file is its parent.
  A fail-before taken from the wrong side of the commit is not evidence; noted in the box.
- **Next run**: read 44987167844's conclusion. Green ⇒ tick §4's CI box, then §2 (fmt/clippy/call-site/tree)
  and open §5 by dispatching both release builds and ending the run with handles. Red ⇒ quote the step line.
- No blocker, no [TRIAGE].

- **A0**: `origin/main` HEAD `db5c88e1e4a1a1a0e4095c9b8ff0445399fd7736` is docs-only (`?head_sha=`
  full-40 ⇒ `total_count: 0`, correct under `paths-ignore`). The box this run had to resolve was the
  CI for the code commit `88f2eea`: run **44987167844** read `pending` at 20:56 PT, `pending` again
  at 21:35 PT — **40 minutes with `total_count: 0` jobs**, which is not slowness.
- **Diagnosed the queue rather than waiting on it.** `GET /actions/runners` ⇒
  `DESKTOP-USBUADL online busy=true`; the busy job was `137091523988` (Windows build) belonging to the
  *previous* run `44984299924`, whose linux job had already failed at 04:48. `ci.yml` line 53-55:
  `concurrency: group: ci-${{ github.ref }}`, `cancel-in-progress` only for `pull_request` — so a new
  push-triggered run queues behind the old one and gets no jobs at all. Classified: **environment /
  runner contention**, not our code, not a missing run.
- **Fixed by removing the cause, no cancel needed**: attempted `POST .../cancel` on `44984299924` and
  got `409 Cannot cancel a workflow run that is completed` — it had gone `completed failure` at
  05:34:47Z. `44987167844` picked up its 6 jobs within seconds (`secret-scan` + Windows build
  `in_progress`, `fmt + clippy` queued). Handle stays in §4's box; not waited on.
- **Useful fact from the old run**: its **Windows job `137091523988` completed `success`**
  (steps 6/7 `success`). The red was linux-only — the Windows toolchain on box 1 is not implicated,
  and the 233-236 Windows tests were never the failing side.
- **Box worked — §1 "Name the behaviour and the machine-observable thing", ticked with same-turn
  runs**: behaviour = an update never mistakes its own stop for a reason to give up. Observables
  quoted from the live box-1 log (`19:22:14 taskkill ... SUCCESS (7 pids)` → `19:22:16 start ... node
  pid 41048 parent cmd.exe` → `19:24:19 CONFIRMED: 0.3.66 is running after 124 s`), `last-exit.json`
  slugs `console-teardown` vs `system-shutdown` (`exit_report.rs:129`), and exit 75 + `DEFERRED:`.
  Re-run this turn: `test-autostart-installers.sh` ⇒ `all autostart/installer assertions passed`;
  `test-update-exit-cause.ps1` ⇒ `RESULT: PASS (all checks)` EXIT=0; `test-update-defer.ps1` ⇒
  `RESULT: PASS (all checks)`. Fail-before counts on record (38 and 13) from the pre-change tree.
- **Next run**: read 44987167844's conclusion. Green ⇒ tick §4's last box and open §5 (dispatch
  linux + windows release builds on the same SHA, write the handles, end the run). Red ⇒ quote the
  failing step's line. Then §2 boxes (fmt/clippy/call-site/tree) are the cheap ones.
- No blocker, no [TRIAGE] this run.

## 21:15 PT — 0.3.67 §4 "CI green" still open: the red was two of our tests, fixed in 88f2eea; green run now queued

- **A0**: `origin/main` HEAD = `3dc6609` (loop-log commit, docs-only) → `?head_sha=` full-40 ⇒ **total_count 0**, correct under `paths-ignore`. The *code* sha is `88f2eea` (CI fix), run **44987167844**, `pending` since 04:55Z (20 min) — queued behind run 44984299924's Windows job. 44984299924 (sha `e9e2890`) was in progress at 04:16Z → **65 min**, under the 90-minute threshold and it is a build (not a publish) job, so it was **not** cancelled; letting it finish frees the single runner.
- **State audit (first, from the API, not from memory)**: the local checkout was 8 commits **behind** `origin/main` and clean; `git pull --ff-only` brought it to `3dc6609`. Checklist tally: §3b **DONE** (233–236 all ticked), §3 DONE, §4 3/4. First unticked box = **§4 "CI green on that exact SHA"** (line 253).
- **Box worked — §4 CI green (left open, handle recorded).** `44984299924` concluded **failure**, and the failing job names the exact lines: linux job step 13 `Autostart installers (Windows + Linux, reversible)` → `FAIL scripts/tests/test-update-exit-cause.ps1 contains non-ASCII bytes (breaks Windows PowerShell 5.1 parsing)` and `FAIL a failure dialog can be shown before the node is restarted`. Classification: **our tests**, not the product — the first is box 234's test carrying U+2500 box-drawing runs (the 5.1 ANSI-decode bug that cost run 44728980382 its release); the second is a stale locator, because box 233 moved the restart into `Start-NodeAfterStop`, so the bare `schtasks /Run /TN $NodeTask` now sits after the failure dialog in source order. Both fixed in `88f2eea` (test files only, no product code).
- **Re-checked locally rather than trusting the commit message**: on `88f2eea`'s tree `bash scripts/tests/test-autostart-installers.sh` ⇒ `all autostart/installer assertions passed` (EXIT=0); `/tmp/pwsh/pwsh -NoProfile -File scripts/tests/test-update-exit-cause.ps1` ⇒ `RESULT: PASS (all checks)`; `grep -cP '[^\x00-\x7F]'` ⇒ **0** in both files.
- **Why the box is not ticked**: it asks for a green run on the release SHA, and the run for `88f2eea` has not started. Ticking on local evidence would be exactly the thing the gate forbids. The handle is written into the box; the next run reads its conclusion and either ticks it or dispatches again.
- **This run's docs commit** updates only `docs/releases/0.3.67.md` and this log — docs-only, so it starts no run (checked above). No blocker, no [TRIAGE] (231 remains owner-held and unchanged).
- Next run: read run 44987167844. If `success` → tick §4, then §5 box 1 (dispatch both platform builds on `88f2eea` and end the run). If `failure` → quote the new failing line and fix that cause.
## 20:56 PT — A0 was a red CI, and the red was two of our own assertions (233-236), now fixed

- **A0 read the failure, did not park it.** `origin/main` HEAD at start = `711a123` (docs commit,
  `?head_sha=` full-40 ⇒ no run, correct under `ci.yml` paths-ignore). The code commit it describes,
  `e9e2890`, had run **44984299924**; job list showed `Build + unit tests (linux)` **failure** at
  2026-11-22T04:48:14Z while `fmt + clippy` (success, 04:19Z) and `secret-scan` (success, 04:20Z)
  were green. So this run's box was the red, per the A0 rule.
- **Exact failing lines** (job 137092055714, step 13 "Autostart installers (Windows + Linux, reversible)"):
  `FAIL .../scripts/tests/test-update-exit-cause.ps1 contains non-ASCII bytes (breaks Windows PowerShell
  5.1 parsing)` and `FAIL a failure dialog can be shown before the node is restarted`. Classified: **our
  tests** — no shipped code was implicated, and steps 4-12 (node build/test, agent build/test, gpn_update
  CLI, launcher identity, wallet provisioning, fast-sync guards, referral capture) all passed.
- **Cause 1 — the encoding bug again.** Box 234's new test carried seven comment lines of U+2500
  box-drawing characters. Windows PowerShell 5.1 decodes a script's bytes with the ANSI codepage, so
  those UTF-8 bytes shift the literal; this is the exact class that cost run 44728980382 its release.
  Fixed by replacing the runs with ASCII `-`; `grep -P '[^\x00-\x7F]'` now returns nothing (0 non-ASCII
  bytes) and the repository's own ASCII guard passes.
- **Cause 2 — a stale locator, not a real regression.** `test-autostart-installers.sh` found the
  restart-after-apply by the literal `schtasks /Run /TN $NodeTask`. Box 233 moved that call inside
  `Start-NodeAfterStop`, so the only remaining literal is the recovery fallback at `install.ps1:2082` —
  which sits *after* the failure dialog in source order, so the assertion failed while the ordering it
  guards was intact (`Start-NodeAfterStop 'after apply'` = 1940, `Show-UpdateFailure $apply` = 1959).
  The locator now accepts either the literal or `Start-NodeAfterStop`; the invariant (a modal dialog
  must never keep the node down) is unchanged and still asserted.
- **Verified locally, not inferred**: `bash scripts/tests/test-autostart-installers.sh` ⇒
  `all autostart/installer assertions passed`, with `ok test-update-exit-cause.ps1 is ASCII-only`,
  `ok restarts AFTER --apply`, `ok restarts before showing a failure dialog (a modal box cannot keep the
  node down)`; `pwsh ... test-update-exit-cause.ps1` ⇒ `RESULT: PASS (all checks)`;
  `cargo fmt --all -- --check` exit 0 in both `gpn-node` and `gpn-agent` (fresh run this turn).
- **Pushed once**: `88f2eea` (tests only, `711a123..88f2eea main`). CI run **44987167844** dispatched by
  that push, `pending` at 04:55:47Z — handle written into §4's last box, not waited on.
- **§4 "CI green on that exact SHA" stays open** with the failure quoted and the fix handle recorded;
  the next run reads 44987167844's conclusion and ticks it. §3b is DONE (233-236 all ticked); §3 DONE.
- No blocker, no [TRIAGE] this run — 231 is unchanged (owner-held offline sudo key) and nothing new
  needs a decision.

## 20:45 PT — 0.3.67 §3 and three §4 boxes ticked; the CI for the code SHA is in flight, box 1 (build) is next

- **A0**: `origin/main` HEAD at start = `1669436c…b090` (box 4/236 tick commit). Its run list is
  `total_count: 0` because it is a docs commit — per the docs-do-not-build rule that is correct, not a
  missing run. The last **code** commit `e9e28909…29a3` has run **44984299924** (`CI`, push,
  2026-11-22T04:16:11Z) still `in_progress` at 20:41 PT; noted, not awaited.
- **State audit**: crate = `0.3.67`; §3b fully ticked (233/234/235/236); first unticked box = §3
  `scripts/bump-version.py patch`. Working tree at HEAD (no stale-tree repeat).
- **Box worked — §3 "Bump and record", 4 boxes, all already true in the tree, ticked with evidence**:
  - bump: `gpn-agent/Cargo.toml` = 0.3.67.
  - changelog: `## [0.3.67] - 2026-11-21` present, 2031 bytes, `_TODO_` count 0.
  - lockstep: `bump-version.py --check` → `lockstep: 15 manifest(s) and 14 lockfile entr(ies) agree`,
    `gpn version: 0.3.67`, exit 0.
  - seeds: no `seeds.json` in this tree; the one bootstrap seed in `network.json.bootstrap_seeds`
    (`dns4 gpn-testnet.duckdns.org` + `ip4 142.129.54.88`, `12D3KooWAFtUNxc…hSVFo`) appears verbatim in
    `install.ps1:169`, `install.sh:52`, `scripts/run-windows.cmd:165`, `run-windows-provider.ps1:174`,
    `release-windows.yml:393` (dns4-only there, a consistent subset).
- **§4 ticked (3 of 4)**: docs-only push starts no build — `?head_sha=6910cd47…` → `total_count: 0`
  (proven, not assumed); one docs commit this run; nothing else pushed. The CI-green box is left open
  with the run handle **44984299924** written into it, per the dispatch-and-end rule.
- **Next run**: read 44984299924's conclusion. Green ⇒ tick it and open §5 (dispatch the two release
  builds, one SHA, then end the run). Red ⇒ quote the failing line and fix.
- No [TRIAGE] item. No owner decision reached this run.

## 20:2x PT — 0.3.67 box 4 (row 236) done: an update never starts while Windows is going down

- **A0**: `origin/main` HEAD at start = `78684d8` (box 3/235). Its CI run **44982921625** was `pending` —
  noted, not awaited (the open box is installer work, not a build/publish step). That run is now
  `cancelled`, superseded by this run's pushes; the current HEAD `e67fa7f` has run **44984531666**
  `pending`. Nothing about 236 depends on it.
- **State audit**: crate = `0.3.67`; §3b had 235 ticked and **236** open. First unticked box = 236.
- **Working tree was stale again** (`b741e94` local vs `78684d8` main) — fetched before touching anything,
  per the 2026-11-21 lesson.
- **Box worked — 236, ticked with evidence.** `Get-ShutdownState` in the update wrapper template reads
  three OS-level signals before the node is stopped or `--apply` is called: `GetSystemMetrics(0x2000)`
  (`SM_SHUTTINGDOWN`), a non-empty `PendingFileRenameOperations`, and the CBS / Windows Update
  `RebootPending` keys. Any hit ⇒ log `deferred: system is shutting down - <why>`, node NOT stopped,
  nothing installed, `exit 75`; the next cycle after the restart applies normally (§5-§8 work).
  `ExitWindowsEx` is the call that *starts* a shutdown, not a state query, so it cannot be polled —
  `SM_SHUTTINGDOWN` is its observable state and that is what is read (recorded in the code comment, since
  the row names `ExitWindowsEx`).
- **Evidence**: `scripts/tests/test-update-shutdown.ps1` 21 checks `RESULT: PASS (all checks)` on pwsh
  7.4.6 here and under Windows PowerShell 5.1 on box 1 (`EXIT=0`, at `e67fa7f`); **13 failures** against
  `git show 78684d8:install.ps1`. Live on box 1 the un-stubbed probes answered
  `pending=True why=a reboot is pending (PendingFileRenameOperations is non-empty)` — the detector fired on
  a real machine state. Regression: `test-update-defer` PASS, `test-update-exit-cause` PASS,
  `test-update-confirm` 0 failures, `test-update-start-after-stop` PASS; `install.ps1` parses 0 errors,
  pure ASCII.
- **One test assertion was wrong, not the code**: the first version asserted "a healthy host is not
  deferred" and failed on box 1 because box 1 genuinely owes a reboot. Corrected to "a pending answer
  always carries a reason" (commit `e67fa7f`) rather than weakening the detector — that failure *was* the
  live proof.
- Commits: `e9e2890` (code + test), `e67fa7f` (test assertion). Both pushed to main.
- **Next**: §3b is now fully ticked (235, 236). Next run: §3 bump/CHANGELOG check → §5 build (dispatch and
  end the run), then publish, deploy, prove.


## 20:1x PT — 0.3.67 box 3 (row 235) done: a teardown-class start failure defers, it never rolls back

- **A0**: `origin/main` HEAD at start = `b741e94` ("loop log: 0.3.67 box 2 (234) done"), a docs commit;
  `?head_sha=<full 40>` ⇒ **total_count 0**, correct under `ci.yml` paths-ignore. The last code commit,
  `74a17a6` (box 2), has CI run **44981895134** `pending` since 03:43Z (~10 min) — noted, not awaited;
  the open box is neither a build nor a publish step.
- **State audit**: `gpn-agent/Cargo.toml` = `0.3.67`; `docs/releases/0.3.67.md` §3b has 233/234 ticked and
  235/236 open. First unticked box = **235**.
- **Box worked — 235, ticked with evidence.** The updater now classifies the loader status BEFORE it decides
  to roll back: `$script:LoaderCodes` + `Get-LoaderExit` in the update wrapper template read this attempt's
  `last-exit.json` (signed, unsigned or `0x` hex; a record older than `$cycleStartUnix` is ignored) and map
  `0xC000026B`, `0xC0000142`, `0xC0000135`, `0xC000013A` to *defer* — no rollback, one retry after 10 s, then
  `deferred: environment did not allow the process to start (0x…)` via the new `Show-UpdateDeferred`
  (`DEFERRED:`, exit 75 = EX_TEMPFAIL, dialog says "not failed … Nothing was rolled back"). `0xC0000005` is
  named but still rolls back: an access violation at load is the build's fault. The launcher's own
  `report-exit.ps1` gained `session-closing` / `dll-init-failed` / `load-access-violation`, so the synthetic
  record no longer says `unknown`.
- **Verified**: `scripts/tests/test-update-defer.ps1` — 44 checks, `RESULT: PASS` on pwsh 7.4.6 here and on
  **Windows PowerShell 5.1 on box 1**; against `git show HEAD:install.ps1` the same test gives
  `FAILURES: 38` (fail-before/pass-after). Regression: `test-update-exit-cause` PASS, `test-update-confirm`
  `FAILURES: 0`, `test-update-start-after-stop` PASS; `install.ps1` parses 0 errors, 0 non-ASCII bytes.
- **Live on box 1**: a real `last-exit.json` written the way the launcher writes it, read back from disk ⇒
  `0xC000026B STATUS_DLL_INIT_FAILED_LOGOFF defer=True`; the same record older than the cycle ⇒ ignored; the
  box's own record ⇒ `kind='provider' code=1 loader=none` (no false deferral); live node still up
  (`gpn_node pid 41048`, `C:\gpn\data\update\current\gpn-node.exe`).
- **Pushed once** — see the commit line appended below. No blocker, no [TRIAGE] this run (231 is unchanged and
  owner-held; nothing new needs a decision).
- Next run starts at box 4 (row 236: do not start an update while the system is shutting down).

## 19:5x PT — 0.3.67 box 2 (row 234) done: a console teardown is no longer called a machine shutdown

- **A0**: `origin/main` HEAD at start = `9636408` ("0.3.67 box 1 (233)…"), a code commit; `?head_sha=<full 40>` ⇒ CI run **44980529326** `pending` since 03:25Z (~6 min). Not awaited — under the 90-minute threshold and the open box is neither a build nor a publish step.
- **State audit**: `gpn-agent/Cargo.toml` = `0.3.67`; `docs/releases/0.3.67.md` exists with §3b boxes 233 (ticked) / 234-236 (open). First unticked box = **234**.
- **Box worked — 234, ticked with evidence.** The discriminator is the OS's own answer, not a guess:
  `GetSystemMetrics(SM_SHUTTINGDOWN)` is non-zero only while a session is really ending, so a
  `CTRL_SHUTDOWN_EVENT` with `SM_SHUTTINGDOWN == 0` is a console teardown — which is exactly what our own
  `taskkill /T /F` stop provokes. `classify_shutdown_event()` + two new `last-exit.json` slugs
  (`console-teardown` `0xC000013A`, `system-shutdown` 0) in `exit_report.rs`; `install.ps1` gained
  `Get-ExitCauseNote`, which quotes the node's own record in the confirm failure line and refuses to quote a
  record older than `$cycleStartUnix`. Before: `git show HEAD:gpn-agent/src/bin/gpn_node.rs:1984-1990`
  printed "the machine is shutting down" for every such event.
- **Verified**: 2 new gpn_node unit tests + 12-check `scripts/tests/test-update-exit-cause.ps1` PASS;
  `test-update-confirm.ps1` 0 failures; `test-update-start-after-stop.ps1` PASS; gpn_node bin suite 34 passed;
  `cargo test --lib exit_report` 11 passed; `cargo fmt --check` clean; clippy with CI's allow-list
  `-D warnings` clean; `install.ps1` parses 0 errors and stays pure ASCII.
- **Live on box 1**: `GetSystemMetrics(8192)` ⇒ **0** on a healthy running node
  (`C:\gpn\data\update\current\gpn-node.exe`); `node.log` already carries **39**
  `[supervisor] CTRL_CLOSE_EVENT` lines from our own stops — the same control-event class, previously
  mislabelled. The end-to-end (0.3.67 supervisor through a release-driven update) is §5-§8 work and is
  recorded as such in the box, not claimed.
- **Pushed once**: `74a17a6` (`9636408..74a17a6 main`). CI for it is dispatched-and-forgotten, per the rule.
- **No blocker, no [TRIAGE] this run** — 231 is unchanged (owner-held offline sudo key), nothing new to raise.
- Next run starts at box 3 (row 235: never roll back on a teardown-class start failure).
## 19:26 PT — 0.3.67 box 1 (row 233) done: replacement starts after the tree is gone, detached, and logs the parent

- **A0**: `origin/main` HEAD at start = `4e7a606` ("loop log") — docs-only, `?head_sha=` full-40 ⇒ no run, correct under `ci.yml` paths-ignore. The code-touching sha `c7d33de` (0.3.67 bump) has CI run **44978223917** `in_progress` since 02:54Z; under the 90-min threshold and no build/publish box open, so not awaited.
- **Row 233 implemented** in `install.ps1`'s `$UpdateWrapperTemplate`: new `Start-NodeAfterStop` — re-checks `Wait-NodeExit` before launching, starts through `schtasks /Run` (parent = Task Scheduler service) with a detached `Win32_Process.Create(wscript hidden-launch.vbs)` fallback, and logs `start (<why>): <how> at <time> - stopped tree gone at <time>, node pid N parent <name>`. All five start sites use it (after apply, refused stop, rollback, rollback retry, crash recovery). Before: a bare `& schtasks /Run` with no ordering record and no liveness re-check (`git show HEAD:install.ps1:1706`).
- **Test** `scripts/tests/test-update-start-after-stop.ps1` (15 checks): PASS under pwsh 7.4.6 locally and under **Windows PowerShell 5.1 on box 1** (`RESULT: PASS (all checks)`); `test-update-confirm.ps1` still 0 failures; `install.ps1` parses with 0 errors; file remains pure ASCII.
- **Live on box 1** (real node, not a simulation): new wrapper installed at `C:\gpn\update-and-restart.ps1` (backup `.bak-233`); `update.log` shows `19:22:14 taskkill gpn_node.exe: SUCCESS` (7 pids) → `19:22:16 start (live 233 proof): schtasks (Task Scheduler service) … node pid 41048 parent cmd.exe` → `19:24:19 CONFIRMED: 0.3.66 is running … after 124 s`. `LIVE233 RESULT: PASS` — the start is strictly after the last taskkill SUCCESS and the new node's parent is the hidden launcher, not the stopped console.
- **Still open for 233**: the same proof from a *release-driven* apply (0.3.66 → 0.3.67). That is §5–§7 work on the 0.3.67 artifact, not something to fake now. Rows 234/235/236 remain unticked (next run starts at box 2).
- **No blocker, no [TRIAGE] this run** — 231 is unchanged (owner-held offline sudo key) and is not re-raised as new information.

## 19:05 PT — 0.3.66 confirmed closed; 0.3.67 bundled (rows 233–236, the teardown class) [TRIAGE: 231 unchanged]

- **A0**: `origin/main` = `82f8a1b8c463c379928de6f09f0a45d18cef0d59` ("list: fix the teardown class,
  not just this instance"), a docs-only commit (1 file: `GPN-LIST.md`) ⇒ `?head_sha=` full-40
  returns `total_count: 0`, correct under `ci.yml` paths-ignore. Nothing waited on.
- **0.3.66 verified released, not assumed**: `releases.json` serves `0.3.66` (published
  2026-11-22T01:41:06Z), the running node answers `system_version` ⇒ `"0.3.66"`, and the checklist is
  archived 51/51 (`docs/releases/archive/0.3.66.md`). So B was satisfied and C was the right rung.
- **231's status was wrong and is corrected, not silenced**: the row said `IN-PROGRESS` while its
  source half had shipped in 0.3.66 and its only remaining step is an owner-held offline key — that
  status is what made C unreachable for six runs. Verified the capability limit rather than repeating
  the claim: `gpn-node/node/src/chain_spec.rs:67` `testnet_sudo_key()` = `5HHSuw…PjuMn`, live
  `Sudo.Key` storage = `0xacc828cd12728ec7841ff70d1bccd0775a25c23c5bf518702777977c17089440`, and
  `state_getRuntimeVersion` still `52`. No key material for it exists on this host. Now
  `AWAITING-OWNER` with that evidence in the row.
- **Bundled 233/234/235/236** — the class behind the 5th machine's 0.3.66 → 0.3.64 → fail loop:
  start the replacement only after the old tree is gone and detached from the dying console (233);
  distinguish a console teardown from a real session end in the log, `last-exit.json` and the
  message (234); classify NTSTATUS loader codes (`0xC000026B` `0xC0000142` `0xC0000135` `0xC000013A`
  `0xC0000005`), retry once, then `deferred: environment did not allow the process to start` — never
  roll back (235); refuse to begin while a shutdown/restart is pending (236).
- **Version**: `scripts/bump-version.py patch` → **0.3.67**; `--check` ⇒ `lockstep: 15 manifest(s)
  and 14 lockfile entr(ies) agree`; `network.json latest_version` 0.3.67; CHANGELOG section written
  (0 `_TODO_` in the 0.3.67 block).
- **Checklist**: `scripts/make-release-checklist.py 0.3.67` ⇒ `docs/releases/0.3.67.md` (4
  in-progress in §3b). Commit `c7d33de`, one push (`82f8a1b..c7d33de`). This commit touches
  `Cargo.toml`, so CI runs for it — not waited on.
- **Next run**: start at 0.3.67 box 1 (§1 of the checklist). All four are Windows-side behaviour, so
  the proofs are `scripts/box1.sh` + the update test suite, and the final binary is the only runner job.
- **[TRIAGE] 231** — unchanged and unanswered (5th announcement). Spec-53 enactment needs Jeremy's
  offline sudo key; `docs/NEEDS-TRIAGE.md` §231 carries the question, the three options and
  recommendation A. Not re-raised as new information: nothing about it changed this run.

## 18:20 PT — 0.3.66 closed at 51/51; the only open item is 231's on-chain enactment (owner key) [TRIAGE]

- **A0**: `origin/main` = `2ffa0100d96e91e81b28cee955f7b32c951adfc9` ("0.3.66: record live site
  confirmation and the CI run handle", docs-only) ⇒ `?head_sha=` for it returns `total_count: 0`,
  correct under `ci.yml` paths-ignore. The *code-touching* sha from last run, `42b032a2…`
  (`website/download.html`, not paths-ignored), has CI run **44973607092** — `pending` since
  01:54:22Z (~22 min). Not awaited: under the 90-minute threshold and no build/publish box is open.
- **State audit before working**: `gpn-agent/Cargo.toml 0.3.66`; `https://gpncoin.ai/releases.json`
  ⇒ `version 0.3.66, published_at 2026-11-22T01:41:06Z`; the running node answers
  `POST :9944 system_version` ⇒ **`0.3.66`**; `system_health` ⇒ 3 peers, `isSyncing: false`, head
  `0x539eb` (342,507). So rule B's three gates for 0.3.66 are met for the rows already closed
  (230, 232 COMPLETE in `42b032a`).
- **GPN-LIST has no `NEW` and no `TODO` rows**: the single IN-PROGRESS row is **231**
  (`register_provider` fee-free). Rule C therefore does not apply — nothing new is bundled this run.
- **Box worked — 0.3.66 §3b box 231**, ticked `blocked — <reason>` so the release is not held hostage:
  the staged artifact is real and is the *only* thing left — `wasm/gpn_runtime_spec53_free_registration
  .compact.compressed.wasm`, 534,023 B, `sha256 d5c1b69a32f3735da4e7e62384bac154fdfa191dbfdfd7f1d41bd
  33546a3419f`; live `state_getRuntimeVersion` at 18:18 PT still answers **`specVersion 52`**, so the
  chain itself confirms the fix is not applied. `gpn-node/scripts/build-spec53-wasm.sh` says in its own
  header that it does not apply the artifact and "the testnet sudo key is Jeremy's offline key, so this
  machine cannot enact it" — a capability limit, not a missing tool I can install. Checklist is now
  **51/51**.
- **Row 231 marked `AWAITING-OWNER`** in its evidence column with the same facts, pointing at
  `docs/NEEDS-TRIAGE.md` §231 (question, options A/B/C, recommendation A ≈ 2 min with key D5). This is
  the third run it has been raised; it is the only thing standing between 231 and COMPLETE.
- **Nothing else was fixed-and-parked**: the second open triage item (self-heal IPv6 fetch failure,
  needs `/etc/gai.conf` — DNS change the standing rules reserve for the owner) is untouched by design.
## 17:56 PT — 0.3.66 RELEASED: cut, published, self-updated, observed [TRIAGE: 231 enactment still owner-gated]

- **A0**: `origin/main` HEAD `06f4cb6` (docs-only) ⇒ `?head_sha=` gives `total_runs: 0`, correct under
  `ci.yml` paths-ignore. No red run for the code sha `1b9ded2` (44958227352 succeeded).
- **§7 release-cut COMPLETED** — the 17:35 detached dispatch (pid 294969) ran to exit 0 at 17:44 PT;
  `/tmp/release-cut-0.3.66.log` ends `release-lock: released (0.3.66)` / `released 0.3.66 —
  https://gpncoin.ai/releases.json`. Signed manifest commit **`753e76c`** (`06f4cb6..753e76c main`).
  Mirror digests re-verified in `crewlyllc/gpn_coin`: linux `547a1d21…` 340,399,822 B, windows
  `9d89d3ea…` 248,794,331 B.
- **§7c/§7b/§8 ticked**: `network.json latest_version 0.3.66`; `releases.json` served 0.3.66 at
  17:45:08 **and** 17:45:18 PT; anonymous download of both mirror assets (no credentials) re-hashed by
  hand ⇒ both digests equal the manifest. Both GitHub repos carry the same two asset sizes.
- **Stale version stamps found and fixed**: `website/download.html` said `Current signed release:
  0.3.65`, `docs/run-a-node.md` said 0.3.64. Fixed in **`a3a94a1`** (0.3.66; doc stamp also corrected to
  block 342,914 — the 0.3.65 stamp had carried a stale block number).
- **§9 deploy: a node updated ITSELF** — `systemctl --user start gpn-update.service` 17:46:49 PT;
  `17:48:43 confirm: waiting … for 0.3.66` → `17:48:58 confirm: process=up rpc=yes 14s version=0.3.66`
  → `confirmed 0.3.66 is running`. Attempt record `release_id 547a1d21…` (the signed linux sha256),
  `current -> versions/0.3.66`, `current.previous -> versions/0.3.65`.
- **§10 observed on the real machine**: `POST :9944 system_version` ⇒ `0.3.66`; installed tree's
  `gpn_register --help` prints `direct registration (free, no proof of work)` (item 232, live);
  `system_peers` ⇒ 3 peers at 17:52, head 342,421→342,422 moving; no `gpn-isolated`/`gpn-forked` markers.
  Note recorded in the checklist: right after the restart the node re-anchored from 342,914 down to
  342,421 and climbed again — stated openly rather than presented as a forward-only chain.
- **Filed**: `docs/releases/archive/0.3.66.md`, INDEX row added, timing json written (replay reports
  0s because all ticks share one commit — noted, not hidden). Rows 230 and 232 → `COMPLETE 0.3.66`.
- **Live site confirmed**: `curl -sL https://gpncoin.ai/download.html` at 17:57:49 PT ⇒
  `Current signed release: <strong>0.3.66</strong>` (the page 301-redirects, so `-L` matters).
- **CI handle for the next run**: the push of `a3a94a1`+`42b032a` created a *CI* run (`pending` at
  17:57 PT) for `42b032a2a6c15c131384aef380d279db88f9626a` — caused by `website/download.html`, which is
  not in `paths-ignore`. Not waited on. Next run: ask for that sha's run conclusion; `failure` is the
  box, `success` closes it.
- **[TRIAGE] 231 unchanged**: source + spec bump shipped in the 0.3.66 WASM; **enactment needs the
  offline sudo key (Jeremy's D5)** — re-raised in `docs/NEEDS-TRIAGE.md` §231. Checklist is 50/51 with
  that single box unticked by design.

## 17:33 PT — 0.3.66 §5 verifier + §6 both boxes ticked; §7 cut dispatched detached [TRIAGE: 231 unchanged]

- **A0**: `origin/main` = `a9d8754c…` (docs-only); `?head_sha=a9d8754c…` ⇒ `total_runs: 0`, as
  `ci.yml` `paths-ignore` requires. No red anywhere; the code sha under test (`1b9ded2…`) is the one
  44958227352 already passed. Checkout was stale (`2ebd280`, one docs commit behind) ⇒ reset to
  `origin/main` first.
- **§5 last box ticked — "verify the downloaded artifact with the verifier that matches"**: local
  re-hash of the cached zip ⇒ `sha256 9d89d3ea…076c` = API digest; `cosign verify-blob --bundle`
  vs `release-windows.yml@.*` + GitHub OIDC issuer ⇒ **`Verified OK`, exit 0**.
- **§6 both boxes ticked (section DONE 2/2)** — windows half, from the *published* zip, not a local
  target: `gpn_node.exe` has `Registering costs nothing on any path` ×1 +
  `register_provider_entry_unsigned` ×6 and zero paid-registration phrases; `gpn_update.exe` has
  `first output:`/`node log:`/`child: `; and the embedded stamp
  `gpn-build-v1 version=0.3.66 sha=1b9ded2 spec=53` matches the Linux tarball's
  `gpn_node --version`. No exe was executed on Linux — the claim is the binary's own stamp plus the
  job's `success`, stated as such.
- **Fixed on the way (environment, not code)**: `release-cut.sh` aborted `3/6` with
  `no CI run for commit 1b9ded2 within 25 commits`. Cause: this runner checkout is a 4-commit
  shallow clone (`.git/shallow`), so the script's "nearest green ancestor" walk had nothing to walk.
  `git fetch --unshallow origin` ⇒ 1941 commits; dry run then reached `6/6` and signed
  `version=0.3.66 min_version=0.1.0` (both asset digests identical to the API).
- **§7 box 2 dispatched, not waited on**: `setsid nohup scripts/release-cut.sh --version 0.3.66`,
  log `/tmp/release-cut-0.3.66.log`, started 17:31 PT. Next run reads that log: success ⇒ tick
  §7's cut/network.json/releases.json-twice boxes; failure ⇒ quote the line and fix.
- **Worth knowing**: `release-cut.sh` warns `scripts/release-lock.sh` is missing, so the repo is not
  locked during a cut (a commit now would cancel the release's CI run). Not touched this run —
  it is a separate fix, not a blocker.
- **[TRIAGE] item 231 unchanged** — still AWAITING-OWNER on the spec-53 enactment (sudo key D5);
  question in `docs/NEEDS-TRIAGE.md` §231, live chain still `specVersion 52`.

## 17:00 PT — 0.3.66 §5 attestation box ticked: cosign `Verified OK` on the published zip [TRIAGE: 231 unchanged]

- **A0**: `?head_sha=2ebd280…` ⇒ 0 runs (docs-only, expected). No CI red anywhere; last code run
  44958227352 on `0a7f2e5` was already `success`.
- **Build pair finished**: `release-linux-selfhosted.yml` run **44966033883** `success` (00:34:41Z),
  `release-windows.yml` run **44966053558** `success` (00:45:32Z), both on
  `1b9ded2116dd632269e2c4d62e79e348fcb1949a`.
- **Box ticked — §5 "attestation exists for the digest that is actually published"**:
  `windows-latest` carries `gpn-windows.zip` 248 794 331 B `sha256 9d89d3ea…076c` **and**
  `gpn-windows.zip.bundle` 9 033 B `sha256 928277b2…d18b` (both asset-updated 00:45:25Z).
  Downloaded both from the asset API, re-hashed locally ⇒ exact digest match, then
  `cosign verify-blob --bundle … --certificate-identity-regexp 'release-windows.yml@.*'
  --certificate-oidc-issuer https://token.actions.githubusercontent.com` ⇒ **`Verified OK`**.
  `releases.json` still serves 0.3.65 (the cut is §7), so the digest came from the published asset
  and was checked by local re-hash — no site URL was hashed. Zip+bundle cached in `/tmp/attest066/`.
- **Next run**: §5 box 2 (paste the verifier output, ~2 s off the cache), finish §6 for windows
  (the linux half was already read from the published asset at 16:41), then §7 `release-cut.sh`
  (dispatch, write the handle, end).
- **[TRIAGE] item 231 still AWAITING-OWNER** — spec-53 enactment needs sudo key D5. Source half
  proven, spec-53 WASM built (`sha256 d5c1b69a…42819f`, code hash `0xd4224aa6…a3e6a`), live chain
  still answers `specVersion 52` (re-checked 16:56 PT). Question in `docs/NEEDS-TRIAGE.md` §231;
  recommendation **A** (loop enacts with D5, ~2 min, no fork). Harmless to the 0.3.66 cut — 0.3.66
  ships the source change either way; only the on-chain half waits.

## 16:41 PT — 0.3.66 §5: linux build success + published; linux half of §6 verified, windows run still in flight [no triage]

- **A0**: `origin/main` = `2c28424c…` (docs-only: the §5 dispatch commit); `?head_sha=2c28424c…` ⇒
  `total_count: 0` — expected under `ci.yml` `paths-ignore`. The code sha in flight, `1b9ded2116dd…`,
  is already CI-green (run 44958227352). Checkout was detached at `1b9ded2`, one docs commit behind ⇒
  reset to `origin/main` before touching anything.
- **Asked once, did not wait**: linux run **44966033883** ⇒ `completed / success` (00:19:12Z →
  00:34:41Z, 15.5 min; every step green incl. `publish GitHub Release (rolling 'linux-latest')`).
  Windows run **44966053558** ⇒ `in_progress` at 00:38Z, 19 min in. Per the dispatch rule that is a
  handle, not a wait, so the run ends here.
- **Work done while windows builds — the linux half of §6, from the published bytes.** Downloaded the
  real asset (777346783, 340,399,822 B, sha256 `547a1d21…` = API digest, uploaded 00:31:52Z) and
  read its binaries: `gpn_node 0.3.66 (spec 53, 1b9ded2)`, `gpn-node 0.3.66`; strings show item 232
  positives present (`Registering costs nothing on any path` ×1,
  `register_provider_entry_unsigned` ×8) and the false language absent (`paid registration`, `paid
  path`, `deposit GPN`, `fund the wallet` all ×0); item 230's renderer present in `gpn_update`
  (`first output:`, `node log:`, `child: `). §6 boxes left **unticked** and marked PARTIAL — the
  boxes cover both platforms and the windows zip does not exist yet.
- **Gotcha recorded in the checklist**: the bundle's `gpn-node` is the *chain* node; the agent
  onboarding binary is `gpn_node` (underscore). Scanning `gpn-node` for the new strings reads as a
  false "missing" — it cost a detour this run.
- **Still open**: item 231 `AWAITING-OWNER` (spec-53 enactment needs offline sudo key D5; live chain
  answers specVersion 52) — see `docs/NEEDS-TRIAGE.md`, unanswered across runs.
- **Next run**: read run 44966053558 once. Green ⇒ cosign-verify the published zip, tick §5's
  attestation + verifier boxes, finish §6 for windows, then §7 `release-cut.sh --timeout 300`
  (dispatch, write the handle, end). Red ⇒ quote the failing step and fix the cause.

## 16:20 PT — 0.3.66 §5 build pair dispatched on one SHA (both runs in flight) [no triage]

- **A0**: `GET /commits/main` ⇒ `1b9ded2116dd…` (**docs-only**: checklist §4 tick + LOOP-LOG);
  `?head_sha=1b9ded2116dd…` ⇒ `total_count: 0` before this run — expected under `ci.yml`
  `paths-ignore`, not a fault. Last code sha `0a7f2e5b…` is already CI-green (run 44958227352).
- **Boxes worked — §5's first four:**
  - per-item Windows evidence: **n/a with a stated reason** — no item in this bundle (230/231/232)
    has Windows-specific behaviour, so there is nothing per-item to assert, and no workflow was
    dispatched to pretend otherwise. Collected the SSH baseline §9 will need instead:
    `scripts/box1.sh` ⇒ `(Get-Process gpn-node).Path` = `C:\gpn\data\update\current\gpn-node.exe`,
    `--version` = `gpn-node.exe 0.3.65`.
  - `release-linux-selfhosted.yml` ⇒ run **44966033883**, dispatched 00:19:12Z.
  - `release-windows.yml` ⇒ run **44966053558**, dispatched 00:19:28Z.
  - same SHA: both `head_sha 1b9ded2116dd632269e2c4d62e79e348fcb1949a`, ref tag
    `build/1b9ded2116dd` (created via API, 201, pointing at main HEAD). Runners were
    `online busy=False` for both at dispatch time (`DESKTOP-USBUADL`, `openclawassist`).
  - attestation ordering: `.github/workflows/release-windows.yml` sign :698 → provenance gate :720
    (no `if:`) → publish :748; repo `private: true` ⇒ keyless cosign path, as the box requires.
- **Not waited on.** Both builds are 40-75 min; run ended at the handle, per the dispatch rule.
- **Still open**: item 231 `AWAITING-OWNER` (spec-53 enactment needs the offline sudo key D5; the
  live chain still answers specVersion 52). **Re-announced [TRIAGE] — see the 231 entry in
  `docs/NEEDS-TRIAGE.md`; it has now gone unanswered across three runs.**
- **Next run**: read both run ids once. `success` ⇒ continue to §5's attestation-digest box (claim
  the windows sha256 from `releases.json` / the run's package step, then
  `cosign verify-blob` against the published bundle); `failure` ⇒ read the failing job's log, quote
  the line, classify, fix.

## 15:1x PT — 0.3.66 §4 CI-green box ticked: run 44958227352 success on 0a7f2e5 [no triage]

- **A0**: `GET /commits/main` ⇒ `454ab07f…` (docs-only: checklist §3/§4 ticks + LOOP-LOG);
  `?head_sha=454ab07f…` ⇒ `total_count: 0` — expected under `ci.yml` paths-ignore, not a problem.
  Local checkout was 2 behind (on `0a7f2e5`) and clean, so it was reset to `origin/main` first.
- **Box worked — §4 "CI green on that exact SHA", ticked with evidence.**
  **Run 44958227352 = `success`** for `0a7f2e5ba5c588e3796ee8cc6da94e9b9bc9b170` (created 22:43:40Z,
  completed 00:09:35Z / 15:09 PT): `fmt + clippy` 108 s, `secret-scan` 51 s,
  `Build + unit tests (linux)` 22:56→23:28Z (32 min), `Build + unit tests (windows, self-hosted)`
  22:52→00:09Z (77 min, DESKTOP-USBUADL). Skipped by design: dep audit, two-node GPU, macos.
  No push was needed; it is the run for the item-232 commit already on `main`.
- **The red before it, diagnosed rather than waived**: 44950717469 (`f1db7476`, 21:17Z) failed with
  `error[E0433]: cannot find "ChildEvidence" in "update"` at `src/bin/gpn_update.rs:1375`, plus
  `E0407`/`E0609` (`child_evidence` not a member of `update::ConfirmProbe`; `no field "child" on
  exit_report::Report`). Class: **our code, transient mid-series** — the bin half of item 230 landed
  before its lib half. Fixed by `6f2b768` (`update.rs:2361 pub struct ChildEvidence`,
  `exit_report.rs:184 pub child`), an ancestor of `0a7f2e5`. Verified on this host too:
  `cargo check --bin gpn_update --bin gpn_node` ⇒ `Finished ... in 7.45s`.
- **Waiting note (honest)**: this box was CI-dependent, so the run did read the conclusion instead of
  ending blind — 40 min in_progress at hand-off time, 87 min total end to end. From here the loop is
  back on the dispatch-and-end pattern: §5's two build dispatches get triggers + run ids only.
- **Still open**: item 231 `AWAITING-OWNER` in `docs/NEEDS-TRIAGE.md` (spec-53 enactment needs the
  offline sudo key D5; live chain still answers specVersion 52). Nothing new for Jeremy this run.
- **Next run**: §5 — dispatch `release-linux-selfhosted.yml` **and** `release-windows.yml` on the
  same SHA `0a7f2e5b…`, write the run ids in the boxes, end the run.

## 14:58 PT — 0.3.66 §3 (bump/CHANGELOG/lockstep/seeds) ticked with evidence; CI noted, not waited [no triage]

- **A0**: HEAD = `0a7f2e5ba5c588e3796ee8cc6da94e9b9bc9b170`. One run for it,
  **44958227352** (`CI`), `in_progress` (created 22:43:40Z). No conclusion to act on ⇒ **not waited
  on**, per the dispatch rule. (The older `6f2b7685` dispatch is superseded; the §4 box now carries
  the new handle.)
- **Boxes worked — §3, four for four, each re-derived this run:**
  - bump: `gpn-agent/Cargo.toml` = `0.3.66`, HEAD subject `0.3.66 item 232: …`. Not re-run — a
    second `bump-version.py patch` would have produced 0.3.67.
  - CHANGELOG: `## [0.3.66] - 2026-11-21` with Added/Changed/Fixed, `grep -ciE '_TODO_|TODO'` ⇒ 0.
  - lockstep: `lockstep: 15 manifest(s) and 14 lockfile entr(ies) agree` · `gpn version: 0.3.66`.
  - seeds parity: `cargo test --test shipped_bootnodes --test launcher_bootnodes` (gpn-agent) ⇒
    **8 passed, 0 failed** in 6m03s of build + 0.1s of test.
- **Item 231 stays AWAITING-OWNER, unticked.** The source half is proven (Pays::No + spec 53 +
  73/73 pallet tests, spec-53 WASM built and hash-checked) but the live chain still answers
  `specVersion 52`, so the row's own claim is false on the network. Question sits in
  `docs/NEEDS-TRIAGE.md` §231 — if there is no answer by the next run it gets re-announced, not
  silently dropped.
- **Next run**: read run 44958227352's conclusion for `0a7f2e5b…`. `success` ⇒ tick §4's CI box and
  move to §5 (build both platforms, dispatched not waited). `failure` ⇒ quote the failing line and
  fix it before anything else.
## 14:43 PT — 0.3.66 item 232 done: "paid registration" purged from code, docs and binaries [no triage]

- **A0**: `GET /commits/main` ⇒ `b7087825d7cc1bf2f7110f02a17de800daae65d3`; one run for it
  (**44957382382**, `CI`) is `pending` — noted, **not waited on**, per the dispatch rule. Local
  checkout was 1 ahead / 5 behind and its one commit (`4de3ad0`) had landed upstream as
  `6f2b768`, so it was reset to `origin/main` before any work.
- **Box worked — §3b item 232, ticked with evidence.** Identifiers, not just strings:
  `EntryPlan::PaidPath` → `EntryPlan::Direct`, `EntryOutcome::RegisteredPaid` →
  `RegisteredDirect` (9 call sites), so "paid" is no longer a concept in the onboarding code.
  Operator text rewritten: `gpn_register` prints
  `mode: direct registration (free, no proof of work)`; the spent-free-entry failure now names the
  **unsigned re-registration path** (`register_provider_entry_unsigned`) and the fee-free
  `register_provider` instead of "deposit GPN first" — in `onboard.rs` (3 sites),
  `classify_submit_error` (2), `gpn_node.rs`'s retry hint and `docs/run-a-node.md`'s failure table.
- **Proof**: `grep -rniE "paid (path|registration)|registration fee|deposit GPN"` now returns only
  historical notes, the rule that forbids the phrase, job-payment wording, and the test that
  outlaws it. `strings` of the built `gpn_register` **and** `gpn_node` → 0 matches, and the node
  binary carries the new hint verbatim. `cargo test --lib onboard` 12/12 (the
  `free_entry_spent_reports_a_specific_reason` test was extended to assert the unsigned path is
  named and "paid"/"deposit GPN" are absent), `cargo test --bin gpn_node onboard` 2/2,
  `cargo fmt -- --check` clean, CI's own clippy line (`-D warnings` + allow-list) Finished clean.
- **Still open in this release**: item 231 is `AWAITING-OWNER` in `docs/NEEDS-TRIAGE.md` (spec-53
  enactment needs Jeremy's offline sudo key D5). Nothing new for him this run.
- **Next run**: item 231 stays parked on his answer; otherwise §3 (bump/CHANGELOG already written
  at bundle time — verify with `bump-version.py --check`) and §4.

## 15:0x PT — 0.3.66 item 231 source half: `register_provider` is `Pays::No` at spec 53, build dispatched [TRIAGE]

- **A0**: `GET /commits/main` ⇒ `fecadeff5e59f14d624abca85a84a0d797bd6620` (docs-only: LOOP-LOG +
  list + checklist); `?head_sha=fecadeff…` ⇒ `total_count: 0` — expected under `ci.yml`
  paths-ignore. The code run for its parent `6f2b7685` (**44955662523**) was `pending` at 14:12 PT
  and is still `pending` at 14:2x — **not waited on**, per the dispatch rule. No conclusion to act
  on: nothing red, nothing green.
- **Box worked — §3b item 231** (`register_provider`, call_index 0, was the last paid path to
  registration). Box left **UNTICKED** with evidence notes; only enactment is missing.
  - Pallet: `#[pallet::weight((<T as Config>::WeightInfo::register_provider(), Pays::No))]` (:704)
    and `DispatchResultWithPostInfo` + `Ok(Pays::No.into())`, mirroring `accept_job` (#205) — the
    pool reads `pays_fee` from `DispatchInfo` before dispatch, so a dispatch-only fix is invisible
    to it.
  - New test `register_provider_is_fee_free`: `DispatchInfo` `Pays::No`, BOB's balance forced to 0,
    dispatch succeeds, post `pays_fee == Pays::No`, registration present, balance still 0.
    **Proved failing first**: with the annotation and signature reverted ⇒
    `assertion left: Yes / right: No — "register_provider must be Pays::No in DispatchInfo"`;
    restored ⇒ `cargo test -p pallet-compute-marketplace --lib` **73 passed, 0 failed**.
  - `spec_version` 52 → **53** (`runtime/src/lib.rs:127`), pinned by
    `attestation_identity_spec46::spec_version_matches_live_chain` (**4 passed**); `network.json`
    `spec_version` 51 → **52** (it was stale — the chain has been at 52 for days), so
    `check-spec-version.sh` ⇒ `source 53 / network 52 · OK`; `cargo fmt --check` clean on both
    crates; `cargo check -p gpn-runtime` Finished.
- **Dispatched, not waited on**: `gpn-node/scripts/build-spec53-wasm.sh` (new file) → 
  `wasm/gpn_runtime_spec53_free_registration.compact.compressed.wasm`, log `/tmp/spec53-build.log`,
  features `testnet-zero-ed,testnet-no-stake,testnet-fast-timelock` (reproduces live testnet
  behaviour), canonical genesis WASM hash-guarded/restored (`76f37b23…16fa97`).
- **[TRIAGE] — item 231 enactment.** The testnet sudo key is "a fresh OFFLINE key held by Jeremy"
  (`node/src/chain_spec.rs:67`, key D5); this host cannot `set_code`. Live chain still answers
  `specVersion 52`, so `Pays::No` is on nothing yet. Options for him: A run
  `Sudo.sudo(System.set_code)` with D5 (the path specs 44/45/51 took, ~2 min) · B committee
  governance (3-of-5, 10-block timelock) · C fold it into the next release's `set_code`.
  Recommendation: **A**. Written to `docs/NEEDS-TRIAGE.md`.
- **Build finished inside this run (3m41s), verified rather than assumed**: 534 023 B, `sha256
  d5c1b69a…42819f`, `blake2_256` code hash `0xd4224aa6…a3e6a`, and — after zstd-decompressing the
  blob (8-byte magic `52bc537646db8e05` first) — `authoring 1 / spec_version 53 / impl 1`. The build
  script's first decode pass scanned the *compressed* bytes and printed a false
  `spec_version: NOT FOUND`; fixed in `build-spec53-wasm.sh` so a rebuild self-verifies. Artifact
  committed at `wasm/gpn_runtime_spec53_free_registration.compact.compressed.wasm`, row added to
  `wasm/README-pending-upgrade.md` (BUILT + TESTED — NOT APPLIED, owner-gated).
- **Next run**: item 232 (purge the paid-registration language from code, docs and the site), unless
  Jeremy answers the spec-53 enactment TRIAGE first — then enact and close 231.

## 14:12 PT — 0.3.66 item 230 implemented and proven live (confirm failure names the child's cause) [no triage]

- **A0**: `GET /commits/main` ⇒ `7988c47dc79891755bc4d54faa48a04cf50ba8bf` (docs-only "releases: file
  the eight unarchived checklists"); `?head_sha=` ⇒ `total_count: 0` — expected under `ci.yml`
  paths-ignore. Nothing in flight, nothing waited on.
- **Boxes worked (§1 + §2 of the 0.3.66 checklist, item 230)**: tests written first and failing
  (`error[E0407]: method `child_evidence` is not a member of trait `ConfirmProbe`` + 8 more), then
  the implementation. `ChildEvidence` on the probe trait, carried on `ProcessGone` and rendered by
  `describe()`; production `ProcessRpcProbe` reads the fresh `last-exit.json` record and the node
  log, baselined at attempt start so a stale record can never be quoted; the same facts are written
  back to `last-exit.json` as a `child` object (`Report.child`, optional, so old NDJSON still parses
  — `exit_report.rs` is `#[path]`-shared with the node, whose `cargo check` was run: Finished).
- **Proof, end to end**: `gpn_update --confirm --expect 0.3.66 --rpc 127.0.0.1:3999` inside
  `unshare -rpf --mount-proc` with an empty version tree and a fixture ledger ⇒
  `confirm failed: signal=process (never appeared) after 30 s — 0.3.66 never came up; … child:
  exit 69 (no-ollama); ollama is required and not reachable at 127.0.0.1:11434; first output:
  [gpn-node] OLLAMA NOT RUNNING at 127.0.0.1:11434; node log: [gpn-node] exiting: no-ollama`.
  The ledger gained `{"code":69,"kind":"no-ollama",…,"child":{"exit_code":69,…}}`; a stale 0.3.65
  `corrupt-key` record in the same file was correctly ignored.
- **Tests/suites**: `--lib` 785 passed / 0 failed / 14 ignored (update::tests 114); `--bin
  gpn_update` 8 passed (2 new). fmt clean; clippy with ci.yml's exact allow-list clean.
- **Pushed once**: `6f2b7685f5d8fd2e0039ae87b1518a62eb718d44` (code + checklist in one commit).
  CI run **44955662523** pending at 14:12 PT — handle written into the §4 box, **not waited on**.
- **Next run**: read run 44955662523's conclusion (tick §4 or fix the red), then §3b item 231
  (`register_provider` → `Pays::No`).

## 13:05 PT — 0.3.65 finished: §10 proved live (3/3), §10b recorded, checklist filed [no triage]

- **A0**: `GET /commits/main` ⇒ `23e2024b0f190889f0fab6e56175b5b2ae80ffce` (full 40), a docs-only
  commit; `?head_sha=` ⇒ `total_count: 0` — expected under `ci.yml` paths-ignore. Nothing waited on.
- **§10 box 1 — the change observed on a real machine, 12:53:49–12:54 PT.** `system_version` ⇒
  `0.3.65` (the process, head 341,231). `POST :8092/v1/jobs {"task":"chat","model":"qwen2.5:0.5b"}`
  ⇒ **200** in 0.079 s, `job_id 1795294430247`, answer `"update"`, and the AP's own journal line
  `12:53:50 [v1/jobs] task=chat model=qwen2.5:0.5b -> /v1/chat/completions`. `{"task":"teleport"}`
  ⇒ **400** in 0.0005 s with `valid_tasks[]`. `GET /v1/tasks` ⇒ 14 tasks, `served:true` for exactly
  chat/generate-image/speak/finetune; `GET /providers` carries `tasks` / `task_models` / `limits`.
  `/r/GPN-ZYCU-4W49-6R` ⇒ `301 -> /join?ref=…`; `/api/referrals` unsigned ⇒ **401** on the AP and
  **404** on the public site (operator-only, as designed); `releases.json` ⇒ `0.3.65`, the same
  bytes §8 verified.
- **§10 box 2 — peers hold.** 3 peers at 12:54:00 and 3 at 12:59:25 (head 341,231 → 341,247,
  `isSyncing:false`); ban/isolation grep over the last 30 min of `gpn-fleet`, `gpn-provider`,
  `gpn-accesspoint` ⇒ 0 / 0 / 0.
- **§10 box 3 / rule B** — `GPN-LIST.md` rows **221–229** flipped `IN-PROGRESS` → `COMPLETE 0.3.65`
  with version + §3b evidence + the live observation, plus the archive pointer.
- **§10b** — timing recorded: `docs/releases/timing/0.3.65.{json,md}`, **14h42m** total, slowest step
  **5h57m = §3b item 228**, and named honestly: it waited on the public testnet and was re-done
  against a fresh-genesis rig instead of the on-chain spec-52 runtime. Shortening it means craft the
  fork on the row-194 rig, not wait faster.
- **Filing (§0b, both boxes)** — `git mv` to `docs/releases/archive/0.3.65.md` + INDEX row
  (`0.3.65 | 2026-11-20 | 221–229`). Commits `6bafe73`, `87abf06`, `ac005c0`; one push.
- **Found and recorded a tooling bug, not papered over:** `scripts/release-timing.py` follows a
  single path, so run against the archive path after a rename every tick resolves to the rename
  commit and it printed `total 0s`. The committed snapshot is read at `6bafe73` (pre-move) and the
  header says so; fixing the script to use `git log --follow` is follow-up work, not a 0.3.65 box.
- **Remaining in 0.3.65, deliberate:** §1's "write the test that fails now" box stays unticked —
  no pre-fix failing run was archived for rows 221–227 (gap recorded 07:5x PT, not papered over).
- **Next run goes to C:** rows **230/231/232** are `NEW` — bundle them, bump, generate the checklist.

## 12:15 PT — §7b capability boxes ticked; found + fixed the site proxy's missing `/api/v1/jobs` route [no triage]

- **A0**: `origin/main` HEAD at start `5a006e21…` (full 40-char) ⇒ `total_count: 0` for that sha; a run
  `44942431771` sits `pending` for an earlier sha (`07dee52b`). Not waited on — the boxes worked are
  docs/site/verification, not builds. The loop checkout was 8 commits stale (`4e77ca1`) and was read via
  `git show origin/main:` rather than trusted on disk; edits were made in a fresh clone `~/gpn-work-231`.
- **The box found a real defect — the exact one it was written for.** `website/ai.html` calls
  `POST /api/v1/jobs` "One runnable call, right now" and renders its whole task table from
  `/api/v1/tasks`; **neither path was routed in `worker/worker.js`**. Live before the fix:
  `POST https://gpncoin.ai/api/v1/jobs` ⇒ `404 {"error":{"message":"unknown API route — see
  https://gpncoin.ai/ai"}}` (0.09 s) and `GET /api/v1/tasks` ⇒ 404, so a stranger copying the documented
  example got a 404 and the table said "Loading the live vocabulary…" forever. Not a stale deploy: the
  Worker was current with `main` (`/r/GPN5QYFHFW79M` ⇒ 301 `/join?ref=…`, the row-225 alias shipped the
  same day, was live).
- **Fixed** `b32a192`: `/v1/jobs` (POST) + `/v1/tasks` (GET, 60 s cache) routes, `/v1/jobs` added to the
  OPTIONS preflight, and the POST body guard now accepts a `task` word / `inputs[]` (that payload was
  rejected as "missing prompt" before the AP saw it). `node worker/worker.test.mjs` ⇒ **65 passed**; the
  3 new cases **fail (3 failing) on the pre-fix file** — fails before, passes after. Deploy is the
  Cloudflare git build on push to `main`; verified live after it: `/api/v1/tasks` 200 (3,290 B, 14 words,
  4 served), `POST /api/v1/jobs` with the page's verbatim body ⇒ 200 + a real answer.
- **Ticked two capability boxes** (split them per the one-box-per-capability rule, which the generated
  file had lumped): task dispatcher 221/222, and referrals 223/224/226 — `/referrals` 200, names
  "your code is your wallet, shortened", `gpn_node --referral` prints `GPN-5QYF-HFW7-9M` + address +
  share link and exits 0, `?ref=` link 200, unsigned `/api/referrals` 401. Dashboard `GET /api/referral`
  was **not** observed live (node runs without `--gui`; :8099 not listening) — covered by
  `cargo test --lib gui::` 17 passed instead, and recorded as such.
- Remaining unticked in 0.3.65: §8 (3 stranger-download boxes), §10 (3), §10b (2), §0b filing (2), §1 (1).
- Pushed: worker fix `b32a192`, then this docs+checklist commit.

## 11:43 PT — §7b box 3 ticked: site == GitHub == binaries on 0.3.65, proven from re-downloaded published bytes [no triage]

- **A0**: `main` HEAD `c30c68c8a1fca03ca76ca2889389f92edb50d26c` ⇒ one run `44941059831`, `status=pending`
  (created 19:27Z). Not waited on, per the do-not-block rule; the box worked is docs/verification, not a build.
- **§7b "both surfaces agree with the binaries" ticked.** Measured this turn, not copied:
  `releases.json` `0.3.65` (18:57:29Z) with linux `c620fa76…8d2` 340,776,960 B / windows `38d9ee82…ede`
  248,752,159 B; `GET /releases/tags/{linux,windows}-latest` on **jeremyzguo and crewlyllc** return those
  same digests; the published linux tarball was re-downloaded (sha256 = manifest), `tar xz -O ./gpn-node`
  ran ⇒ `gpn-node 0.3.65`; the published windows zip (sha256 = manifest) contains `gpn-node/0.3.65`;
  running process `POST :9944 system_version` ⇒ `"0.3.65"`.
- **Found, not a blocker:** the rolling releases carry no version in `name`/`body` (name `'GPN Linux (latest)'`,
  body names only a build commit — and the two platforms name *different* commits, `1f980e58…` vs `6c3dd06d…`).
  Version identity is therefore only provable by digest + in-binary stamp. Noted in the box; a future
  release script should stamp the release name.
- Next run: §7b capability box, then §8 (stranger download), §10, §10b, then archive/INDEX filing (§0b).

## 11:33 PT — fleet node self-updated to 0.3.65; §7b-1 closed by walking the install path, 9 doc stamps refreshed from it [no triage]

- **A0**: `main` HEAD `3d9dc5e4ccf9ee5aabfcded0db09d648c0ba254f` (full 40-char sha) ⇒ `total_count: 0`
  — docs commit, `paths-ignore` holding. No red to triage. (The local `origin/main` ref was stale at
  `4e77ca1`; the API was used, per the short/stale-sha rule.)
- **§9 (4/4) ticked — the deploy happened for real.** `systemctl --user start gpn-update.service`
  → signed-manifest download + swap by the updater itself; `~/.gpn/update/current -> versions/0.3.65`,
  `current.previous -> versions/0.3.64`. Journal: `confirm: process=up rpc=no 0s` → `process=up rpc=yes
  32s version=0.3.65` → `confirmed 0.3.65 is running`; `update-attempts.json` records
  `outcome=confirmed, release_id=c620fa76…58d2` (the signed linux digest). `POST :9944 system_version`
  ⇒ `0.3.65` from the running process, head 340,878, 62 imports in the surrounding 12 min.
  Observation recorded rather than assumed: the unit runs `gpn_update --apply --quiet`, so the
  check/download/swap lines are **not** in the journal — the attempt record is the evidence, not the log text.
- **§7b box 1 ticked (4/4)**: its last condition was the `docs/run-a-node` stamp, and it was closed
  by doing what the stamp claims. Anonymous download of the manifest URL (http 200, 340,776,960 B,
  sha256 = manifest), `./gpn-node --version` ⇒ `gpn-node 0.3.65`, documented `install.sh` (scratch
  prefix, `--no-autostart`) completed — binaries, wallet, chain spec, payout address — and that install's
  node on spare ports 30363/9945 answered `system_version` ⇒ `0.3.65` and synced to the live chain.
  Live: specVersion 52 measured on `:9944`. Stamp refreshed once in **all 9 pages** that share it to
  `chain spec 52 / release 0.3.65 — checked 2026-11-21 at block 340,878`. Scratch node and dirs removed.
- **§10 partially measured (not ticked)**: 9 min past the restart with no `Reason:` bans, no
  `gpn-isolated`/`gpn-forked` lines in `gpn-fleet` since. Next run can tick §10 box 2 on a longer window.
- Note for the next run: while walking the scratch install I reproduced row 229's genesis signature
  (`ADJUST now=10071 … blocks=10 avg_s=4 target=4`) — a fresh-genesis node still logs the old 10/4
  constants, exactly as row 229 concluded. Not a new defect; confirms the separation genesis vs spec-52.
Newest first. Written by the release loop; read by the assistant, who relays anything
that needs Jeremy. `[TRIAGE]` = a decision waiting on the assistant.

## 11:12 PT — 0.3.65 IS PUBLISHED (windows run green, cosign Verified OK, release-cut exit 0); §5/§6/§7 closed 13/13 [no triage]

- **A0**: `main` HEAD `50ed5f7` = `total_count: 0` (docs commit, `paths-ignore` holding). No red to triage.
- **§5 windows (the box the last four runs were parked on)**: run **44926165305** finished
  `conclusion = success`, 18:01:51Z → 18:27:34Z, all 23 steps green — incl. 21 keyless sign, 22 provenance
  gate, 23 publish. So the 78-minute queue was contention, exactly as diagnosed; nothing needed cancelling.
- **§5 boxes 3–7 ticked** (7/7): asset `gpn-windows.zip` 248,752,159 B
  `sha256 38d9ee82…0bede` + `.bundle` 9,049 B. Downloaded both by API and re-hashed locally (matched the
  API digests), then `cosign verify-blob … ⇒ Verified OK` against the sha256 that will ship.
- **§6 closed (2/2)**: windows strings scanned — `reorg=` 1, `work ours=` 1, `gpn-forked` 2 in
  `gpn-node.exe`; `valid_tasks` 1, `task_models` 1, `serves task=` 2 in `gpn_scheduler.exe` — the same
  literals the linux tarball proved (previously the pending half). `--version` from the *installed*
  box-1 binary: `gpn-node.exe 0.3.65`, `LastWriteTimeUtc 18:23:51Z` (the build's own deploy step).
- **§7 closed (4/4)** — **the release is out.** `release-cut.sh --version 0.3.65`: content scan `5 marker(s)
  present in each` platform, mirror digests re-verified on `crewlyllc`, signed manifest, commit **4e77ca1**
  pushed, `live manifest is 0.3.65`, anonymous download digests match, `released 0.3.65`.
- **§7b**: GitHub + mirror asset digests identical on both repos (ticked). `download.html` was **still
  serving 0.3.64** — found, fixed, pushed `a9f1b17`, live page now `0.3.65`. `seeds.json` has no version
  field (vacuous, recorded). Left open only on `docs/run-a-node.html`'s `Verified against … 0.3.64`
  stamp: 12 pages share it and re-stamping it before actually walking the install path on 0.3.65 would
  be a fake verification — that belongs to §9/§10.
- **Evidence correction made mid-run**: I first wrote the §5 box-6 note as "the `releases.json` digest",
  then checked and found `releases.json` still served 0.3.64 (release-cut had not run). Wording fixed to
  cite the GitHub release API digest, which is what the verifier actually bound.
- Next run: §7b box 1's last condition via §9/§10 (fleet nodes on 0.3.65), then §8 stranger-view, then
  §8's capability list and the filing boxes.

## 10:18 PT — 0.3.65 §5 windows build is stepping (step 10/23, 19 min in); §4 header closed on the ASCII-fix SHA [no triage]

- **A0**: `main` HEAD `44817f057bdc` = `total_count: 0` — the 10:02 docs commit, `paths-ignore` holding.
- **§5 box 3 (windows artifact)**: run **44926165305** `in_progress` since 18:01:51Z; job-level check
  (`/runs/44926165305/jobs`) shows **step 10 `build node` `in_progress`**, steps 11–23 `pending`. 19 min
  into a 40–75 min cold build ⇒ stepping, not hung; under the 90-min stale threshold, so not cancelled.
  Handle stays in the box; verdict asked next run only.
- **§4 closed**: all four boxes were already ticked, so the header now reads `DONE (4/4)`; added the
  missing evidence line — CI **44926088682** `success` on `head_sha 93b4195a959b`, the ASCII-fix sha the
  release is built from (`3a76aa3f`'s green run predates the fix and cannot witness this tree).
- **§5 header corrected** `3/6` → `2/7` (7 boxes there: 2 ticked, 5 open), so partial progress reads
  without counting.
- **Attestation branch pinned**: `GET /repos/jeremyzguo/gpn_coin` ⇒ `visibility: private`, owner
  `jeremyzguo` (`User`) ⇒ cosign keyless + `gpn-windows.zip.bundle`; `attestations/sha256:` 404 is
  expected on this path. Recorded in the §5 attestation box rather than re-derived later.
- Nothing ticked (every remaining box in §5–§10 needs the windows artifact). No blocker, no decision.
  Next run: ask 44926165305 once, tick §5 boxes 3–4 on `success`, then §6 windows scan.

## 10:02 PT — 0.3.65 §5 box 3 unblocked: CI went green on the code SHA and the windows release build started [no triage]

- **A0**: `main` HEAD `409a8bde44a7` (`docs(release): 0.3.65 — windows build still queued …`) has
  `total_count: 0` — docs-only, `paths-ignore` working.
- **§4 CI gate (was open)**: run **44926088682** on `93b4195a959b` ⇒ `completed / success`, 16:43:06Z →
  **18:00:45Z**. The ASCII-guard fix is verified by CI itself, not by the loop's word for it.
- **§5 box 3 (windows artifact)**: run **44926165305** `queued` 16:43:58Z → **`in_progress` 18:01:52Z**.
  The 78-minute wait was contention on the single-slot box-1 runner (`DESKTOP-USBUADL busy:true`), and it
  ended the moment CI freed it — same cause the previous three runs named, now resolved rather than
  restated. Handle written into the box; **not waited on**. Windows cold build = 40–75 min.
- **§5 boxes 4/5 (same SHA, attestation)**: held until 44926165305 reports. Linux sibling
  **44926142445** is already `success` on the same `93b4195a959b`, so the "same SHA" box ticks with one
  verdict left.
- No blockers, no owner decisions. Next run: ask 44926165305 once, then §6 windows scan + `--version`.

## 09:42 PT — 0.3.65 §5 box 3 still queued; CI holder is stepping (gpn-agent build), not hung [no triage]

- **A0**: no CI run for `main` HEAD `8a817aecf423` (`total_count 0`) — that is the 09:25 docs commit,
  `paths-ignore` applies. Nothing to wait on.
- **§5 box 3 (windows)**: run **44926165305** `queued` since 16:43:58Z — **58 min**, `updated_at` unchanged.
  Read the holder at job level instead of guessing (`/runs/44926088682/jobs`): secret-scan success
  16:44:49Z, fmt+clippy success 16:46:31Z, linux `Build + unit tests (linux)` **success 17:30:39Z**,
  windows job step 6 `Build + test gpn-node (release)` **completed 17:21:53Z** and step 7
  `Build + test gpn-agent (release)` **in_progress since 17:21:53Z**. 59 min old ⇒ a stepping build,
  not a stale run; under the 90-min cancel threshold, and cancelling would discard the gpn-node
  compile. Handles recorded, verdict asked next run.
- **Nothing ticked this run** — every remaining box in §5–§8 is gated on the windows artifact.
- **Ended the run here** per the dispatch rule.
- No blocker, no decision needed.


## 09:25 PT — 0.3.65 §5 box 3 still queued (CI holds the single-slot runner); §6 pre-verified on the linux artifact [no triage]

- **A0**: no CI run for `main` HEAD `fdb7266d06b0` (`total_count 0`) — it is the 09:00 docs commit,
  `paths-ignore` applies. Correct, nothing to wait on.
- **§5 box 3 (windows)**: run **44926165305** `queued` since 16:43:58Z — **37 min**, `updated_at` unchanged.
  Cause named, not guessed: CI run **44926088682** (`head_sha 93b4195a95`, started 16:43:06Z) is
  `in_progress` and owns the single-slot box-1 runner (`GET /actions/runners` ⇒
  `DESKTOP-USBUADL online busy:true`, `openclawassist busy:true`). 37 min old ⇒ progress, not a stale
  run; not cancelled, not waited on. Handle recorded, verdict asked next run.
- **§6 staged (not ticked — §5 box 3 still gates it)**: pulled the published linux asset by API
  (`releases/assets/776235125`; the anonymous `browser_download_url` returns 9 bytes on this private
  repo), sha256 `c620fa76…b458d2` = the §5 box 2 digest. Scan of the 60 extracted binaries:
  `gpn-node` has `reorg=`, `work ours=`, `gpn-forked`; `gpn_scheduler` has `valid_tasks`,
  `task_models`, `serves task=` (x2). `gpn-node --version` ⇒ `gpn-node 0.3.65`, exit 0. The referral
  page literals are correctly absent — they ship via the website deploy, not the tarball.
- Nothing else is workable: §5 box 4-6, §6, §7 all gate on the windows conclusion, and no new row
  may be bundled while a release is in flight.

## 09:00 PT — 0.3.65 §5: linux re-dispatch on the fixed SHA CONFIRMED success; windows still queued (runner busy) [no triage]

- **A0**: no CI run exists for `main` HEAD `27fac82aab87` (docs-only commit, `paths-ignore`) — correct,
  nothing to wait on.
- **§5 box 2 note updated**: `release-linux-selfhosted.yml` run **44926142445** (`head_sha 93b4195a959b`)
  `conclusion = success` 16:43:41Z -> 16:59:39Z; `GET /releases/tags/linux-latest` ⇒
  `gpn-linux.tar.gz` 340,776,960 B, `updated_at` 16:59:10Z,
  `sha256:c620fa76…b458d2`. Both assets will now come from the fixed commit.
- **§5 box 3 (windows)** run **44926165305** still `queued` at 17:00Z (`updated_at` 16:43:58Z),
  behind the CI windows job on the single-slot box-1 runner — contention, not a fault. Handle
  recorded; verdict asked next run. Not waited on.
- Nothing else ticked: §5 box 3 gates boxes 4-6, §6 and §7 (sequence rule).

## 08:45 PT — 0.3.65 §5: linux build PUBLISHED, windows build FAILED on our own encoding guard -> cause fixed, both re-dispatched on one SHA [no triage]

- **A0**: run **44911936175** (`3a76aa3f`) `conclusion = success` (14:03Z -> 16:32Z, all jobs
  green, incl. windows `Build + test gpn-agent (release)`).
- **§4 box 4 TICKED** (CI green on the code SHA) with that verdict.
- **§5 box 2 TICKED**: `release-linux-selfhosted.yml` run **44924248633** `success`
  (16:22:42Z -> 16:38:08Z); `linux-latest` carries `gpn-linux.tar.gz` **340,781,762 B**,
  asset `updated_at` 16:37:29Z.
- **§5 box 3 FAILED then fixed**: `release-windows.yml` run **44924273272** `failure` 16:33:49Z at
  step 3 `encoding guard`, 21 s in. Exact lines: `NON-ASCII ci.yml:348 byte 0xe2` / `0x80` /
  `0x94`, then `non-ASCII bytes in workflow files (3) - Windows PowerShell 5.1 decodes run:`
  `scripts as ANSI`. Classification: **our code** (a committed byte), caught by our own guard;
  ci.yml:348 was the comment `# ... four install commands — never a /download URL`. The build was
  never at fault — the windows artifact simply never got built.
- **Fix pushed `93b4195`**: ci.yml:348 is ASCII `--`; new `scripts/check-workflow-ascii.sh` runs the
  same scan in the cheap Linux `lint` job so the byte fails in minutes where it was written.
  Failing-before/passing-after: `LC_ALL=C grep -rnP '[^\x09\x20-\x7e]' .github/workflows` on
  `3a76aa3f:ci.yml` -> `ci.yml:348: ... commands — never a /download URL`; on the fixed tree -> no
  hits, and the script prints `workflow-ascii: OK (all workflow files are pure ASCII)`.
- **Re-dispatched on tag `build/93b4195a959b`**: linux run **44926142445** (queued 16:43:41Z),
  windows run **44926165305** (queued 16:43:58Z), both `head_sha = 93b4195a959b…`; the workflow
  change also started CI run **44926088682** on the same SHA. §5 box 4 (same SHA) re-opened until
  both conclusions are `success`.
- **Ended the run here** per the dispatch rule. Next run: ask once for 44926142445, 44926165305
  and 44926088682 (the last verifies the ci.yml change). Any failure -> quote the line first.
- No blocker, no decision needed.

## 08:22 PT — 0.3.65 §5 boxes 2-4: both release builds dispatched on 3a76aa3f, §5.4 (same SHA) ticked; CI windows still compiling [no triage]

- **A0**: `GET /actions/runs?head_sha=3a76aa3f…` = 1 run, **44911936175** `in_progress` (started
  14:03Z). Jobs: secret-scan success, fmt+clippy success, **linux build success 15:46:15Z**,
  windows `Build + test gpn-agent (release)` in progress at step 7/14 since 15:14:45Z (~65 min —
  normal band). Runner `DESKTOP-USBUADL` busy:true, `openclawassist` idle, 0 queued.
  Judgement: not cancelled. The 90-min cancel rule targets a stale build of mine; this one is
  stepping and cancelling would burn the hour already spent on a single-slot box.
- **§5 box 4 TICKED (same SHA)**: created + pushed immutable tag `build/3a76aa3f1b14` →
  `3a76aa3f1b1483251535a4a669a998683793e06c` (workflow_dispatch refuses raw shas).
  `POST …/release-linux-selfhosted.yml/dispatches` and `…/release-windows.yml/dispatches` both
  returned 204 with `{"ref":"build/3a76aa3f1b14"}`; the runs list shows both runs carrying
  `head_sha = 3a76aa3f1b14`.
- **§5 box 2 handle**: linux run **44924248633**, in_progress 16:22:42Z on the idle linux runner.
- **§5 box 3 handle**: windows run **44924273272**, queued 16:22:59Z behind the CI windows job.
- **Ended the run here** per the dispatch rule — no polling, no waiting.
- **Next run**: ask once for 44924248633 and 44924273272. Both success → tick §5 boxes 2-3, then
  work §5 boxes 5-7 (attestation mechanism = check repo visibility first, digest from
  `releases.json`, verify with cosign/gh). 44911936175 success → tick §4 box 4. Any failure →
  quote the failing line first.
- No blocker, no decision needed.

## 08:0x PT — 0.3.65 §5 (1/6): per-item Windows check done over SSH; CI still building gpn-agent on Windows [no triage]

- **A0**: origin/main = `7dad4071067132d63191160cddd56388628e6c17` (local HEAD was stale at `3a76aa3f`;
  fast-forwarded). No workflow run exists for `7dad407` — all four commits since `3a76aa3f` are
  `docs/**` + `GPN-LIST.md`, which `ci.yml` paths-ignore. The live code run is **44911936175**
  (`3a76aa3f`): `in_progress`, `updated_at` 15:18:57Z — secret-scan **success**, fmt+clippy
  **success**, linux build + all 26 tests **success**, windows `Build + test gpn-agent (release)`
  **in_progress** (gpn-node build succeeded 15:15–15:19Z). 44 min in, under the 90-min stale
  threshold → not cancelled; §5 builds not dispatched into the same single-slot runner. Verdict
  re-asked next run.
- **§5 box 1 ticked (n/a as per-item, real as Windows witness)**: `scripts/box1.sh`, no dispatch —
  box-1 node process path `C:\gpn\data\update\current\gpn-node.exe`, start `Fri Nov 20 21:27:06 2026`,
  `--version` = `gpn-node.exe 0.3.64`, binary 71,213,056 bytes `LastWriteTime 11/20/2026 7:20:10 PM`.
  None of the nine rows in this bundle has Windows-only behaviour (221/222 access-point surface,
  223–227 referral/web, 228 reorg band, 229 retarget constants), so per-item Windows proofs are n/a;
  the Windows witness for 0.3.65 is the release build itself. Also confirms the box-1 node is running
  from `data\update\current` — i.e. the updater placed it — and correctly still 0.3.64.
- **Next run**: tick §4's CI box if 44911936175 concluded; then §5 boxes 2–3 (dispatch
  `release-linux-selfhosted.yml` and `release-windows.yml` on `3a76aa3f`, record run ids, end the run).

## 07:5x PT — 0.3.65 §1 (2/3) and §2 (6/6) closed from §3b evidence; CI run 44911936175 still in flight [no triage]

- **A0**: `GET /commits/main` = `3112d86999118255a280fff862f24c8af2dc46ac`, local HEAD equal.
  Run **44911936175** (`3a76aa3f`) = `in_progress`, `updated_at` 15:18:57Z: secret-scan + fmt/clippy
  **success**, windows `Build + test gpn-node (release)` since 15:15:02Z, linux walking its 26 test
  steps (step 8 at 15:39:43Z). Genuinely progressing, not stale, and §4 is not a build/publish box
  → not waited on, not cancelled; verdict re-asked next run.
- **§2 Implement 6/6 ticked**: test counts (tasks 15, gpn_scheduler 103, referral 29, gpn_node 32,
  provider_status 69, gpn-referrals 5, gui 17, site 13/10/45, worker 62); fmt/clippy clean per item
  **and** independently by CI's `fmt + clippy` job on `3a76aa3f` (success 15:16–15:18Z); call sites
  grepped (`adapt_v1_jobs` ← `gpn_scheduler.rs:5477`, `record_once_checked` ← `:1582`,
  `derive_code` ← `gui.rs:77`/`gpn_node.rs:795`, `may_ask_for_blocks_anchored` ←
  `block_serving.rs:1191`); no env-gated default; changes present in the tree (`tasks.rs` ←
  `7924b35`/`c03600a`, `service.rs` ← `3a76aa3`/`7da7d42`/`4d6d0c7`).
- **§1 Test first 2/3**: behaviour + machine-observable named per item 221–229; the third box
  (**a failing test pasted before the fix**) is left **UNTICKED on purpose** — only row 228 has a
  recorded pre-fix failure (`Run 01:45 PT failed and was diagnosed, not parked`), and its inverted
  `force_branch_best` test was fixed by `3a76aa3`; rows 221–227 name their tests but archived no
  failing-first output. Recorded as a process gap rather than ticking seven claims that do not exist.
- **§4 box 4 (CI green)** still open with its handle; §0b filing boxes wait on publication.
- **Next run**: ask 44911936175 once. `success` → tick §4 box 4 and §5 box 1 (n/a — no item in
  221–229 has a Windows-specific observable) → dispatch `release-linux-selfhosted.yml` +
  `release-windows.yml` on `3a76aa3f`, write both run ids into §5, end the run. `failure` → quote the
  failing line first, then fix.
- No blocker, no decision needed.

## 07:2x PT — 0.3.65 §4 boxes 1-3 TICKED (paths-ignore measured, docs batched, no superseding push); CI verdict still open [no triage]

- **A0**: main is `40262467c2ba…fae090a2` (`git ls-remote origin refs/heads/main`, local HEAD equal).
  `GET /commits/main` agrees. Run **44911936175** (`3a76aa3f`) moved off `pending`: secret-scan
  **success**, fmt+clippy **success**, windows build `in_progress` since 15:14:45Z, linux build
  `in_progress` since 15:18:57Z. In flight → not waited on, and not a build step of mine to cancel.
- **§4 box 1** ticked: `ci.yml:29-35` / `:37-43` carry the six `paths-ignore` globs; measured
  `total_count: 0` for every docs sha since the code sha — `d8554f60`, `b43dd4d4`, `40262467`.
- **§4 box 2** ticked: `git log --name-only 3a76aa3f..HEAD` = 4 commits, all `docs/**`, `GPN-LIST.md`,
  `*.md`; one commit per run, no code file.
- **§4 box 3** ticked: no code push after `3a76aa3f`; the four docs pushes do not match the push
  filter, and `concurrency.cancel-in-progress` is false for pushes (`ci.yml:53-55`), so none
  superseded the run.
- **§4 box 4 left open** — it is a verdict; the handle (run 44911936175) is in the box.
- **Gap flagged, not hidden**: §1 and §2 of this checklist are still 0/3 and 0/6. Every §3b item
  carries its own test evidence, so §1/§2 were never filled in as sections this release (as
  0.3.64 did at the end). Next free run should close them from the §3b notes; they are recorded
  facts, not new claims — do not treat the release as untested because of it.
- **Next run**: ask GitHub once for 44911936175. `success` → tick §4 box 4, tick §5 box 1 (n/a —
  no item in 221–229 has a Windows-specific observable: node log lines, cadence, website, fork
  choice), then dispatch `release-linux-selfhosted.yml` + `release-windows.yml`, write both run
  ids into §5, and end the run. `failure` → read the failing job's log and quote the line first.
- No blocker, no decision needed.

## 07:20 PT AUDIT — fixed 2 (disk 95%→88%; stale scratch), needs-attention 1 (self-heal checkpoint fetch) [TRIAGE]

**Ran by the daily auditor, not the release loop.** Release 0.3.65 is in flight (Windows runner
`busy: true`, run 44911936175 `pending`), so **no service was restarted** and nothing touching
`docs/releases/*` was edited.

### What is running (command: `systemctl --user list-units 'gpn*' --all`, `list-unit-files | grep gpn`)

11 enabled GPN units, **all active**: gpn-accesspoint, gpn-diffusion, gpn-explorer, gpn-faucet,
gpn-finetune, gpn-fleet, gpn-fleet-directive, gpn-oracle, gpn-provider, gpn-provider-heartbeat,
plus the 4 timers (ap-topup, cadence-gate, ci-cache-prune, deploy-drift, prune-ci-cache, update).
`gpn-node.service` is `disabled`+dead **by design** (gpn-fleet runs the node — `run-fleet.sh[3436525]`
is the live process). **No enabled-but-dead unit.** No restart needed, so none was done.

Ports: 9944 RPC 405 (JSON-RPC needs POST), 8092 404 (root), 8093 404 (root), 8099 200, 11434 200.
Chain: `system_chain` = "GPN Public Testnet", `system_version` = **0.3.64**, `system_peers` = **3**
(all AUTHORITY), best #339883 at check time → #339890+ now (blocks importing ~3 s apart).
Box 1 over SSH: `(Get-Process gpn-node).Path` = `C:\gpn\data\update\current\gpn-node.exe`,
`system_version` = **0.3.64**.

### Logs read

`journalctl --user -u gpn-fleet -n 400` (400 lines): **0** `panic`, **0** `Reason: … ban`, **0**
`Incompatible pruning modes`, **0** `taskkill`, 0 restart loops. Only non-INFO line — see the fault
below (`self-heal: divergence check: no verdict — cannot fetch the signed checkpoint (GET
https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json: error sending
request for url …)`, twice in the window, every ~5 min).

`~/.gpn/update/update-attempts.json` (note: it lives under `update/`, not `~/.gpn/`; there is no
`~/.gpn/update.log`): last four attempts all `"outcome": "confirmed"` — 0.3.60 (3 attempts),
0.3.62, 0.3.63, **0.3.64 `release_id` `e768dd45…60cb`** which equals the linux `sha256` in
`website/releases.json`. `~/.gpn/update/current` → `versions/0.3.64`. No `signal=security`,
no failed attempt. `gpn-update.service` exits 0 (last 06:22:38, 2 s).

`openclaw cron runs --id feff1358…`: last entry `status: ok`, 602 s, 07:08 PT (the 0.3.65 §3 run
above). No repeated timeouts.

### Fault — self-heal divergence check is blind (ESCALATED, not mine to fix)

Evidence, same turn:

- fleet log: `cannot fetch the signed checkpoint (GET https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json: error sending request for url …)` — every ~5 min.
- `curl -4 -s -o /dev/null -w '%{http_code}' -L <that URL>` → **200**. The URL and repo are fine.
- `curl <same URL>` (no `-4`) → **000**, `curl: (28) Resolving timed out after 10000 ms`;
same for `https://api.github.com/`, `https://github.com/`, `https://objects.githubusercontent.com/`.
`curl -6` → the same 10 s timeout.
- `getent hosts objects.githubusercontent.com` and `resolvectl query` both answer AAAA
(185.199.11x.x); `ip -6 addr show enp42s0` shows **only `fe80::/64` link-local**, and `ip -6 route`
has no default — i.e. IPv6 is advertised by DNS but not routable on this host.
- `git ls-remote origin` works and `curl -4` works, so it is a *fallback* failure in the HTTP
client, not a dead uplink.

Consequence: the node cannot verify chain divergence against the signed checkpoint — that guard has
been effectively off. Fix is resolver/IPv6-preference config (`/etc/gai.conf` precedence, or stop
advertising AAAA) and the standing rules forbid me changing DNS/firewall. Written up in
`docs/NEEDS-TRIAGE.md`.

### Disk — was a FAULT, cleaned (owner request 2026-11-20)

`df -h /` **before: 439G total, 395G used, 22G avail, 95%, 5.0% free** → **after: 365G used,
52G avail, 88%, 11.8% free**. Freed ~30 G. No `cargo`/`rustc` was running (`pgrep -x` empty, checked
before every deletion); nothing was removed from `releases/`, `~/.ci-cache`, `~/.cache/huggingface`,
`~/ollama`, `~/.gpn` or `shared/`.

| what | size |
|---|---|
| `/tmp/p8c26xx` 41 scratch dirs (Alibaba scrape, Nov 14, `lsof +D` empty) | 7.4 G |
| `/tmp/artcheck`, `/tmp/nc`, `/tmp/p8bb` (release-verify scratch) | 392 M |
| `/tmp/gpn-node.{new,prev,running-old}*` 20 stale binary copies | ~2.1 G |
| `/tmp` dirs >3 d (`ps1test`, `w48check`, `t1`, `snaptest`, …) + `*.log` >7 d | 57 M |
| `projectmanager07/gpn-agent/target/debug` | 6.4 G |
| `gpn-work-223/gpn-agent/target/debug` (3rd worktree) | 8.4 G |
| `~/.cache/gpn-release-cut`, `~/.cache/gpn-artifact-verify` | 561 M |

**Left deliberately:** `/tmp` still 12 G — the biggest items (`v061` 1.3 G, `rel061` 1.3 G,
`gpn064` 1.3 G, `gpnlx` 960 M, `row228` 860 M, `dlcheck`/`anon063`/`rel060` ~560 M each) are
**Nov 19–21**, inside the >3-day rule's exclusion; they fall due on the next run.
`/tmp/gpn-testnet-snapshot-20261114-123709.tar.gz` (293 M) left — it is a chain snapshot, not
scratch. `*/target/release` untouched (6.8 G agent + 6.9 G node) — release artefacts.

**Top five consumers after cleanup** (hidden paths included): `~/.ci-cache` **85 G**
(`gpn-agent-target` 61 G + `gpn-node-target` 24 G; **owner decision: stays**), `~/.cache/huggingface`
**53 G** (XTTS-v2 + sd/sdxl/LTX — used by audio/diffusion; **stays**), `~/.openclaw/workspace`
**30 G** (projectmanager07 15 G, `shared/` 14 G, gpn-work-223 43 M after the sweep),
`~/.gpn` **24 G** (chain data, never delete), `/tmp` **12 G**, `~/ollama/models` **8.1 G**. Sessions
369 M, `cron/runs` 33 M, `plugin-runtime-deps` 1.5 G — all normal, nothing over 30 d worth flagging.

Minor leak worth naming, not fixed: `/tmp/gpn-ap-token-it-corrupt-*` — **40 dirs, 12 K each**, new
one every access-point run (test fixture not cleaned up). Cosmetic today; will be ~1000 dirs in a
month.

Resources: Mem 31 Gi total, **26 Gi available** (fault threshold <4 GiB — fine), but **swap fully
used 2.0 G/2.0 G**; load 2.37/5.66/4.96; GPU 1445 MiB / 8188 MiB.

### Release record consistency

`INDEX.md` rows 0.3.60–0.3.64 each have a file in `docs/releases/archive/` (0.3.60…0.3.64.md, 5/5).
Version triple: **running node 0.3.64 == `website/releases.json` 0.3.64 == box 1 0.3.64**. 0.3.65 is
in flight and correctly absent from INDEX/archive. `GPN-LIST.md` rows 217/218 marked
`COMPLETE 0.3.64`, 219 `COMPLETE` (no version — audio doc, no code). No inconsistency found.

### Remaining

1. **[TRIAGE]** self-heal checkpoint fetch (IPv6 fallback) — see above + NEEDS-TRIAGE.
2. `/tmp` 12 G of 2-day-old release-verify scratch — eligible next audit.
3. Swap at 100 % — watch, no action taken.
4. `/tmp/gpn-ap-token-it-corrupt-*` fixture leak.

## 07:08 PT — 0.3.65 §3 (bump and record) TICKED 4/4; §4 CI box left open with its run handle [no triage]

- **A0**: full-sha query for main `d8554f60` → `total_count: 0` (docs-only, `paths-ignore` working).
  Code sha beneath it `3a76aa3f`: run **44911936175 `pending`** since 14:03Z, queued behind
  **44910312718** (`7da7d42`, `in_progress` since 13:43Z) on the single-slot runner. Not waited on.
- **First unticked actionable box = §3.** All four verified rather than redone: version already
  `0.3.65` in `gpn-agent/Cargo.toml` (a second bump would have made an in-flight release 0.3.66);
  CHANGELOG `## [0.3.65] - 2026-11-20` with rows 221–229 and no `_TODO_` in that section;
  `scripts/bump-version.py --check` → `lockstep: 15 manifest(s) and 14 lockfile entr(ies) agree`;
  seed parity `cargo test --release --test launcher_bootnodes --test shipped_bootnodes` →
  **4 passed + 4 passed, 0 failed** (there is no separate `scripts/` seed checker — the two
  suites are the parity test the box names).
  Section header now `## 3. Bump and record  DONE 2026-11-21 (4/4)`.
- **§4**: boxes 1–2 already true and noted (docs sha started no build; the three docs edits ship in
  one commit `d8554f60`). Box 4 (CI green on the exact sha) left **open** with the two run ids in a
  note — it is a verdict, not a dispatch, so it cannot be ticked from a `pending`/`in_progress` state.
- **Next run**: if 44911936175 is `success` → tick §4 boxes 3/4 and move to §5: dispatch
  `release-linux-selfhosted.yml` (tag `build/<sha12>` for `3a76aa3f`) and `release-windows.yml`,
  write both run ids into their boxes, and **end the run** (no waiting). If either is red → read the
  failing job's log first.
- No blocker, no decision needed.

## 07:0x PT — 0.3.65 box 9 (row 229) TICKED: two runtimes, both intentional — the artifact embeds a frozen genesis WASM, the live chain runs spec 52 [no triage]

- **A0**: `GET /commits/main` → `923a8b95` (LOOP-LOG + `0.3.65.md` only) →
  `actions/runs?head_sha=923a8b95…` → **0 runs** (docs-only, `paths-ignore`) as expected.
- **Box ticked — §3b 229** (deployed 0.3.62 constants vs repo). Verdict: **not a build/tree mismatch.**
  - **Artifact → genesis code.** `./gpn-node build-spec --chain testnet` from the published bundle
    (`/tmp/gpn064/gpn-node`, `gpn-node 0.3.64`) → `genesis.runtimeGenesis.code` 481,002 bytes,
    sha256 `76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97`.
  - **That sha is pinned**: identical to the committed `gpn-node/runtime/gpn_runtime.compact.compressed.wasm`
    and to `CANON_SHA` (`release-linux-selfhosted.yml:36`, enforced :52-57); `runtime/build.rs`
    embeds it with `include_bytes!`. The artifact is the tree, not a stale target dir.
  - **Blob origin = `be390db` (2026-10-06)**, whose `runtime/src/lib.rs` had `spec_version: 11`,
    `TargetBlockTime = 4`, `DifficultyAdjustmentWindow = 10`. Decompressed blob (skip 8-byte header,
    zstd) `strings` → `elapsed_s=`, `avg_s=`, `target=` (pre-`e327d65` format) ⇒ the old pallet.
  - **Frozen by design**: `0f36ff9` "freeze genesis WASM at the chain's launch version — runtime
    changes go via set_code, never the genesis".
  - **Live chain is not on genesis**: `state_getRuntimeVersion` → `specVersion 52` at block 339,714;
    on-chain `:code` sha256 `925b74fe…`, 531,337 bytes, decompressed contains
    `elapsed_ms=/avg_ms=/target_ms=`. Live log:
    `ADJUST now=337886 last_adj=337841 … elapsed_ms=603071 blocks=45 avg_ms=13401 target_ms=15000 …`.
  - **`blocks=10 target=4` is fresh-genesis behaviour**, reproduced by row 228's rig:
    `ADJUST now=21 last_adj=11 elapsed_s=10 blocks=10 avg_s=1 target=4 … 288230376151711744 ->
    115292150460684697` (exactly 2.5×).
- **Correction of record**: always name the runtime — genesis (10 blocks / 4 s / 2.5×) vs on-chain
  spec 52 (45 blocks / 15 s / symmetric). Row 194's fork work must use the on-chain runtime; a *new*
  dev chain from any artifact retargets at 10/4 until a `set_code`.
- §3b now has no unticked items. Next unticked box is §1 (*name the behaviour / write the failing
  test*), then §2, then §3 — the bump and CHANGELOG for 0.3.65 are already in the tree
  (`Cargo.toml` 0.3.65, `CHANGELOG.md` `## [0.3.65] - 2026-11-20`), so §3's boxes need ticks with
  `bump-version.py --check` output. No blocker, no decision needed.

## 06:24 PT — 0.3.65 box 8 (row 228) TICKED: the eleventh rig reorged A from #219 onto B's shorter, heavier 170-block branch [no triage]

- Eleventh rig (BIN `-v11`, 14:02:00Z–14:21:03Z). A.log verbatim:
  - `06:17:59.465 fork-choice: work ours=252176 theirs=7134714 peer=12D3KooWLA8…; reorg=accepted depth=49`
  - `06:17:59.465 fork-choice: requesting heavier branch`
  - `06:17:59.5xx block-serving: asking … — fork choice accepted its heavier branch …`; `06:17:59.5xx block-serving: downloading the accepted heavier branch … — 169 blocks … in 2 chunk(s)`
  - `06:18:01.144 import: the branch at #129 (0x941b…9461) carries work 259934 vs our best #219 252176 — importing it as best, not as a side block (#228)`
  - `06:18:01.343…#170 🏆 Imported #165 … #170 (0xe045…45af → 0x4dae…01c2)`; `06:20:58 Idle (1 peers), best: #170 (0x4dae…01c2)`
- A abandoned its own 219-block chain and adopted B's 170-block heavier branch; the adopted tip hash is the block the accepted download carried. Stated caveat: the rig's 06:21 SAME/DIFF table shows DIFF because **B then followed A's abandoned longer chain** (standby node syncs, never mines) — adoption is proven by A's own tip, not by that table.
- The five causes found across runs 4–10 are all fixed in `gpn-node/node/src`: gate (`accepted_fork`/`may_ask_for_blocks_anchored`), 128-block pagination (`download_accepted_fork`), verdict lifetime (`next_accepted_fork`), measurement (`branch_work_from_parent`), fork decision (`force_branch_best(is_fork, …)`). Unit: `service::` 27 passed, `block_serving::tests` 21 passed, fmt clean.
- Box 228 ticked with this evidence. Next box: 229 (deployed 0.3.62 retarget constants).
- No blocker, no decision needed.

## 06:03 PT — 0.3.65 box 8 (row 228): branch measurement fixed and proven; the decision call was inverted one word; eleventh rig dispatched [no triage]

- Tenth rig (BIN `-v10`, 13:42:21Z–14:00:41Z). Construction OK (`B shorter by 51, heavier by 437504`); accept at `05:57:38.628`; gate opened; per-block line now has numbers: `05:57:40.672 import: fork block #169 (parent 0x7bb5…3406, best #220) — branch_work=Some(7170680) chain_work=Some(352494) (#228)`. **`branch_work_from_parent` is correct.**
- Still zero `import: the branch at …` and 180 blocks landed 🆕. Cause: `force_branch_best`'s param was named `parent_is_our_best` while the call site passed `parent != info.best_hash` — an inverted condition, so a true fork was refused. Renamed `is_fork`; body `is_fork && theirs > ours`; tests updated.
- `cargo test --release --bin gpn-node force_branch_best` → **3 passed**; fmt clean; release build 05:59 PT.
- **Eleventh rig dispatched 14:02:00Z** — `BIN=/tmp/row228/gpn-node-ea10413-v11`, phase 3 ~06:18 PT. Not waited on. Box 228 unticked.
- No blocker, no decision needed.

## 05:45 PT — 0.3.65 box 8 (row 228): instrumentation named the bug — `branch_work=None` because the imported block is not in the DB yet; fixed, tenth rig dispatched [no triage]

- A0: no CI verdict read this run (build for `4d6d0c7` in flight/queued); the box is a rig proof, not a build, so not blocked on it.
- Ninth rig (BIN `-v9`, 13:20:43Z–13:39:44Z). **B.log, 05:36:42.012 (23 such lines):**
  `import: fork block #92 (parent 0x3ec3…dc91, best #170) — branch_work=None chain_work=Some(7774021) (#228)`
  So the wrapper IS called; the measurement returned `None`. `branch_work` did `client.header(tip)` on the block **being imported**, which is not in the database yet — `None` for every block of the batch.
- Correction recorded: my run-8 claim that `PowBlockImport` set `fork_choice` before us was **wrong**; we are outside it. That paragraph in 0.3.65.md is now marked superseded. The code change it motivated is still correct.
- Ninth run's A side was also invalid for the branch question: A restarted at 05:36:40 (phase 3 restart), accept at `05:36:42.006`, no download, A stayed on its own #221 and B synced from A. Phase-3 restart raciness noted in the checklist.
- Fix 05:42 PT: `branch_work_from_parent(client, tip_number, tip_parent)`; `branch_work` delegates; `WorkFirstImport` passes the block's own number + parent hash. `cargo test --release --bin gpn-node 'service::'` → **27 passed**; fmt clean; release build 05:42 PT.
- **Tenth rig dispatched 13:42:21Z** — `BIN=/tmp/row228/gpn-node-ea10413-v10`, phase 3 ~05:58 PT. Not waited on. Box 228 unticked.
- No blocker, no decision needed.

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

## 13:12 PT — 0.3.65 closed; 0.3.66 bundled (230/231/232) and checklist generated [no triage]

- **A0**: `origin/main` == local HEAD `77ea397f1590538061332eb7a7e20ad5bdc09c87` (docs-only
  "loop log: 0.3.65 finished"); `GET /actions/runs?head_sha=<full 40>` ⇒ `total_count: 0` — expected
  under `ci.yml` paths-ignore. Nothing waited on, nothing in flight that blocks a bundle.
- **B satisfied → C executed**: 0.3.65 checklist is filed (`docs/releases/archive/0.3.65.md`, INDEX
  row present), `releases.json` serves 0.3.65 and the process reports `0.3.65` (§10 box 1, 12:53 PT),
  and rows 221–229 are `COMPLETE 0.3.65` (done in the 13:05 run).
- **Bundled** the only three `NEW` rows — **230** (confirm failure must name the child's cause),
  **231** (`register_provider` call_index 0 must be `Pays::No`), **232** (purge "paid registration"
  language). Marked `IN-PROGRESS`; nothing deleted.
- **Version**: `scripts/bump-version.py patch` → **0.3.66**; `--check` ⇒ `lockstep: 15 manifest(s)
  and 14 lockfile entr(ies) agree`; `network.json latest_version` → 0.3.66; CHANGELOG section
  written (no `_TODO_`).
- **Checklist**: `scripts/make-release-checklist.py 0.3.66` ⇒ `docs/releases/0.3.66.md` (3 in-progress).
- **Tooling fix found while generating**: the generator took the checklist's `done when:` line from
  the *discovery* column, so every box would have quoted "found 2026-11-21 …" instead of its
  acceptance criteria (0.3.66's 231/232 rows put the clause mid-description). Now it takes the
  clause from the description when present. Same commit.
- **Blocked by our own guard, then fixed it**: `scripts/git-hooks/pre-commit` (single-writer guard,
  added 2026-11-21) read `published` from `network.json latest_version` — which `bump-version.py`
  sets to the version being *bundled*. So at the exact commit that opens 0.3.66 the hook saw
  published==highest and refused it: "source changes with no open bundle". A guard that forbids
  bundling is wrong, not the bundle. `published` now comes from `docs/releases/INDEX.md` (a row is
  written only after the checklist is ticked and the manifest is live), with network.json as an
  offline fallback when INDEX has no rows. Verified: hook passes with 0.3.66 open and would refuse
  again once 0.3.66 is filed. Commit `f1db747`, one push (`77ea397..f1db747`).
- **Next run**: start at 0.3.66 box 1 (§1 of the checklist), one box per run.

## 12:20 PT — §8 stranger-download proof: both assets match the signed manifest byte-for-byte  [no triage]

- **A0**: `origin/main` was `3ee5cb3d386e1992e7a564b9b1c5dd0861b6df23` ("release 0.3.65: §7b capability
  boxes ticked"), a docs-only commit (`docs/LOOP-LOG.md`, `docs/releases/0.3.65.md`), so
  `GET /actions/runs?head_sha=3ee5cb3…` ⇒ `total_count: 0` — expected under `ci.yml` paths-ignore,
  not a missing run. Local checkout was 1 commit behind; pulled `--ff-only` before touching anything.
- **Box worked: §8 "Verify as a stranger would" — 3/3, now DONE 12:20 PT.** Anonymous (no token, no
  cookies, no account) `curl -sL` of the two URLs `releases.json` publishes:
  linux `200` / **340,776,960 B** / `c620fa767b7eeebd879fa16617d990989d67c3cc0c1b6936569db093c8b458d2`;
  windows `200` / **248,752,159 B** / `38d9ee82e15c3263d8e93913e00f1639894ab05681a136d4a116d806dc40bede`.
  All four values equal `https://gpncoin.ai/releases.json` (0.3.65, published 18:57:29Z) exactly, and the
  linux hash equals the `release_id` the fleet's own 11:19 self-update recorded — a stranger's bytes and
  the running fleet's bytes are the same bytes.
- **Corrected a stale instruction, not just ticked it:** §8's third box names `downloads.md`; that path
  is **404** live and has never existed. The stamp lives at `docs/downloads.html` ⇒ 200, line 50
  "Verified against: chain spec 52 / release 0.3.65 — checked 2026-11-21 at block 340,878", and
  `/download` says "Current signed release: 0.3.65". Polled live, not read from the repo copy.
- **Remaining in 0.3.65**: §1 one box left unticked by design (no pre-fix failing run archived for
  221–227 — gap recorded, not papered over), §10 Prove the behaviour (3), §10b timing (2), §0b filing (2,
  gated on §10). Next run starts at §10.
# LOOP-LOG

## 21:40 PT — the fix run was queued behind a doomed run; freed, it now runs. §1 behaviour box ticked

