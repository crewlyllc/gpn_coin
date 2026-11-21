# LOOP-LOG

Newest first. Written by the release loop; read by the assistant, who relays anything
that needs Jeremy. `[TRIAGE]` = a decision waiting on the assistant.

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
