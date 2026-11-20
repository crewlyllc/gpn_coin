# LOOP-LOG

Newest first. Written by the release loop; read by the assistant, who relays anything
that needs Jeremy. `[TRIAGE]` = a decision waiting on the assistant.

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
