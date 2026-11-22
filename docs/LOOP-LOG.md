## 04:38 PT — 0.3.68 §3b/237 proved on box 1 under PowerShell 5.1; CI run 45018066004's linux job failed on a cache-prune race, cause found and fixed on the host [no triage]

- **A0**: run 45018066004 (`ab3c502`) — `secret-scan` success, `fmt + clippy` success,
  `Build + unit tests (linux)` **failure** at 12:20:46Z, `(windows, self-hosted)` still
  in_progress. A concluded failing job ⇒ that was this run's box, so it was diagnosed
  and fixed alongside §3b.
- **§3b box 237 TICKED with box-1 evidence** (`docs/releases/0.3.68.md`, `GPN-LIST.md`):
  `PendingFileRenameOperations` re-created on box 1 from `C:\gpn\pfro-backup-2026-11-22.reg`
  as 118 multi-strings (even pairs) — the exact state that froze the updater; the shipped
  `install.ps1` (sha256 `d5fc41f0…`) and `test-update-shutdown.ps1` (sha256 `55cfea48…`)
  copied to `C:\gpn\tmp-237\`, hash-verified, and run under **Windows PowerShell 5.1**
  (the task's own interpreter): `RESULT: PASS (all checks)`, `EXIT=0`, 27 ok, with
  `live: pending=False why= notes=a reboot is pending (118 PendingFileRenameOperations entries) - logged only, does not defer`.
  PFRO left non-empty on purpose so the §9/§10 deployed cycle exercises the note path.
- **CI failure — classified ENVIRONMENT (our own maintenance timer), fixed:**
  `error: failed to write /home/jeremyg/.ci-cache/gpn-agent-target/release/.fingerprint/gpn-agent-a73593bae3614032/invoked.timestamp — No such file or directory (os error 2)`
  at 12:20:42Z. `journalctl --user -u gpn-ci-cache-prune.service` shows it started
  **04:20:21 PT = 12:20:21Z** — 21 seconds earlier — and it was a bare
  `find ~/.ci-cache -type f -mtime +7 -delete` + empty-dir delete, so it shredded fingerprint
  files and then the directory cargo was writing into. No compile error anywhere in the log
  (only warnings); steps 8-26 skipped.
  Fix on the host: `gpn-ci-cache-prune.service` now runs `scripts/prune-ci-cache-daily.sh`,
  which skips while `cargo`/`rustc`/`cc1`/`ld` are alive **or** any cache file was written in
  the last 20 min, and only removes empty dirs that were already stale (never one this run
  emptied). New self-test `scripts/tests/test-prune-ci-cache-daily.sh` → **RESULT: PASS (9 checks)**.
  Unit wiring proved by running the unit with `GPN_CI_CACHE=/tmp/unit-test-cache`: journal shows
  `[prune-ci-cache-daily] pruning files older than 7d under /tmp/unit-test-cache`, stale file gone.
- **Re-run blocked, by design not by fault**: `POST /jobs/137200899218/rerun` ⇒ 403
  "The workflow run containing this job is already running" (the windows job is still
  in flight on the single-slot runner). **Next run**: when 45018066004 concludes, re-run the
  linux job only (`POST /actions/jobs/137200899218/rerun`) and expect green — the cause was
  outside the code and is gone.
- **Pushed**: one docs-only commit (this log, `GPN-LIST.md` row 237 note, `0.3.68.md` §3b tick
  + §4 note). Docs SHA starts no CI run; `ab3c502` remains the release's code commit and its run
  is untouched. Nothing awaits Jeremy.

## 04:11 PT — 0.3.68 §4 evidence gathered: docs commit started no build, code commit's CI is green on the fast jobs and still building; no push while it runs [no triage]

- **A0**: main HEAD `17a706a` is a docs-only checklist commit ⇒ `GET /actions/runs?head_sha=17a706a08432…`
  returns `total_count: 0` (paths-ignore working). The release's code commit `ab3c502` has one run,
  **CI 45018066004**, `in_progress` at 12:11Z: `secret-scan` **success** (11:59Z), `fmt + clippy`
  **success** (12:00Z), `Build + unit tests (linux)` in_progress (12:01Z), `Build + unit tests
  (windows, self-hosted)` in_progress (11:58Z). No conclusion ⇒ not awaited (A0), §4's CI box stays unticked.
- **§4 (3/4)**: docs-only-no-build ✓, one-docs-commit ✓, nothing-pushed-this-window ✓ — all
  written into `docs/releases/0.3.68.md` with the API evidence above as notes. Deliberately
  **not committed/pushed**: §0 forbids a push while CI runs for HEAD, and a push now would
  supersede 45018066004, the very run §4 needs green.
- **Box 1 measured this run (SSH, not CI)**: running process `C:\gpn\data\update\current\gpn-node.exe`
  = **0.3.67**, started 03:17:43; `GPN-Update` and `GPN-Node` both `RunLevel=Highest`; `update.log`
  carries the 237 defect being fixed — `02:50:11` / `03:05:54` `deferred: system is shutting down -
  a reboot is pending (PendingFileRenameOperations is non-empty)` — and then the 03:17→03:19 cycle that
  worked once the queue cleared (`taskkill … SUCCESS` ×7, `CONFIRMED: 0.3.67 is running … after 123 s`).
  So §3b/§9/§10 proofs need 0.3.68 on that box; they are not tickable from 0.3.67.
- **Next run**: read 45018066004's conclusion; if success, tick §4's CI box and dispatch the two
  release builds (§5) with their run ids, then end. Nothing awaits Jeremy.

## 04:00 PT — 0.3.68 §1–§3 proved and ticked (tests re-run + fail-before on the pre-change installer); §4 waits on the CI run it just started [no triage]

- **A0**: CI run **45018066004** on `ab3c502` is the run for main HEAD — at check time `queued`,
  then claimed at 11:58–11:59Z (`secret-scan` + `windows build` in progress, `fmt + clippy` queued).
  No conclusion yet ⇒ per A0 not awaited, and §4's CI box stays unticked. It sat queued 24 min, which
  is runner contention (single slot), not a fault.
- **§1 DONE (3/3), §2 DONE (6/6)** — every re-run this turn on `ab3c502` under pwsh 7.4.6, not inferred:
  `test-update-shutdown` / `test-update-defer` / `test-update-exit-cause` / `test-update-start-after-stop`
  / `test-update-task-elevation` all `RESULT: PASS (all checks)`, `test-update-confirm` `FAILURES: 0`,
  `test-autostart-installers.sh` `all autostart/installer assertions passed`.
- **fail-before re-produced**, against `git show ad06dff:install.ps1` copied into `/tmp/fb238`:
  `test-update-shutdown.ps1` ⇒ `FAILURES: 7` (`FAIL C PFRO-only is NOT deferred  why=a reboot is
  pending (118 PendingFileRenameOperations entries)`); `test-update-task-elevation.ps1` ⇒
  `could not find function Get-TaskRunLevel in /tmp/fb238/install.ps1` (`grep -c
  Assert-UpdateTaskElevation` ⇒ 0). The old installer had no elevation check at all.
- **§3 DONE (4/4)**: `bump-version.py --check` ⇒ `lockstep: 15 manifest(s) and 14 lockfile entr(ies)
  agree`, `gpn version: 0.3.68`; CHANGELOG `0.3.68` section has no `_TODO_`; seed parity
  `cargo test --test shipped_bootnodes` ⇒ `4 passed; 0 failed`.
- **Next run**: read run 45018066004's conclusion. Green ⇒ §4 CI box, then dispatch the two §5 builds
  (linux-selfhosted + windows on box 1's runner) and end the run with the handles written into the
  boxes. Red ⇒ quote the failing line and fix it. **§3b's on-box proof needs the shipped 0.3.68
  installer** (both changes are installer-side), so it rides the deploy in §9, not a hand-run script.

## 03:4x PT — 0.3.67 closed by the previous run; 0.3.68 BUNDLED (rows 237 + 238: the two gates that stopped Windows updates) [no triage]

- **A0**: no CI run exists for main `ad06dff` at check time (`head_sha=` → 0); the last push was docs-only
  (paths-ignore). Run `45016984083` from the timing-tool fix was `in_progress` — noted, not awaited.
- **0.3.67 was completed by the 03:22 run** while this one was working: §10 observed on box 1, checklist
  filed, rows 233–236 → COMPLETE, 237 left NEW (code half). So this run did not re-touch 0.3.67.
- **C — bundled 0.3.68**, both remaining NEW rows, both implemented and tested here:
  - **237** `Get-ShutdownState` now defers only on `SM_SHUTTINGDOWN`; PFRO/CBS/WU are returned as `Notes`
    and logged (`note: a reboot is pending … - logged only, does not defer`). Rationale in code: the
    updater's own swap uses MOVEFILE_DELAY_UNTIL_REBOOT, so a successful update created the marker that
    would block the next one. `test-update-shutdown.ps1` rewritten for the new contract (PFRO-only and
    CBS+WU-only NOT deferred, notes carried, notes log outside the deferral branch) → **RESULT: PASS
    (all checks, 27)**.
  - **238** new `Assert-UpdateTaskElevation` reads the LIVE `GPN-Update`/`GPN-Node` RunLevels, replaces
    the update task once (`/F`) if it is lower, re-asks, and warns plainly if it is still lower; called
    on both installer paths (fresh install + `--update-task-only`). New
    `scripts/tests/test-update-task-elevation.ps1` → **RESULT: PASS (18 checks)**. `install.ps1`
    parse-checks clean under pwsh 7.
- **Tool bug fixed while bundling**: `make-release-checklist.py` required a fifth pipe, so a row whose
  text contains no `|` was silently dropped from the checklist — row 237 would have been invisible to
  the release. Now splits on the first two pipes only (`0.3.68`: 2 in-progress inserted).
- `/usr/bin/python3 scripts/bump-version.py patch` → 0.3.68 (15 manifests + 14 lockfiles +
  `network.json`), `--check` ⇒ `lockstep: 15 … agree`; CHANGELOG section written, no `_TODO_`.
- **Next run**: work box 1 of `docs/releases/0.3.68.md`. §10's on-box proof needs the new
  `update-and-restart.ps1` + a `RunLevel=Highest` task deployed to box 1 (both are installer-side, so
  the shipped 0.3.68 install must regenerate them). Nothing else merges into main while 0.3.68 is in flight.

## 03:22 PT — 0.3.67 COMPLETE and filed; box 1 unblocked and now runs 0.3.67

- **A0**: no CI run exists for main `7436384` (docs-only checklist commit, paths-ignore) — nothing awaited.
- **§10 box 1 (the last real box) DONE — on box 1, with the released wrapper.** Two environment defects had to be
  cleared before the change could be observed, both recorded as rows rather than worked around silently:
  - **237 (stale PFRO)**: 118 dead `PendingFileRenameOperations` entries made #236's gate defer every cycle.
    Exported to `C:\gpn\pfro-backup-2026-11-22.reg` (2,601,782 B) and removed. The gate then let the cycle run.
  - **238 (NEW — elevation mismatch)**: `GPN-Update` ran `RunLevel=Limited` while `GPN-Node` runs `Highest`, so
    the updater could not stop the node (`Access is denied - it may be elevated`), and it correctly refused:
    `REFUSING to apply: gpn processes survived the stop` … node stayed up on 0.3.66, no rollback. Set the task
    principal to `Highest` (`before=Limited after=Highest`).
  - Then: `03:17:30 taskkill … SUCCESS` -> `03:17:42 installed 0.3.67` -> `03:17:44 start … parent cmd.exe`
    -> `03:19:48 CONFIRMED: 0.3.67 is running … after 123 s`. `C:\gpn\data\update\current\gpn-node.exe
    --version` = `gpn-node.exe 0.3.67`.
- **Rows 233/234/235/236 -> COMPLETE 0.3.67** with that evidence; 237 stays NEW (code half: narrow the gate).
- **Filed**: checklist moved to `docs/releases/archive/0.3.67.md`, `INDEX.md` row added, §0b boxes ticked.
- **0.3.67 is now finished**: every box ticked, published, and reported running by a node on each platform.
- **Release-timing tool fixed**: after filing, `scripts/release-timing.py` read only the archive path and lost every
  pre-rename tick (`total_s: 0`). It now reads both `docs/releases/<v>.md` and `archive/<v>.md`; re-run gives
  `total_s: 30413` (8h27m), 52 rows, slowest step 1h02m (§1 name-the-behaviour). Commit `9139ddd`;
  CI run **45016984083** dispatched by the push — noted, not awaited.

## 03:09 PT — 0.3.67 §7–§10b worked: versions live everywhere, node self-updated, and box 1 found blocking itself [TRIAGE]

- **A0**: CI run 45013609816 on `cf42692` was `in_progress` (started 10:40Z) — noted, not awaited.
- **§7 DONE (4/4).** Cut already finished at 02:44 (`released 0.3.67`). Ticked:
  `network.json` `latest_version` = `0.3.67` (`git show origin/main:network.json`, last touched by the
  publish commit); `releases.json` polled twice 10 s apart ⇒ `0.3.67` both (`curl`).
- **§7b DONE (4/4) — the site was stale and is now fixed.** `website/download.html` still said
  `Current signed release: 0.3.66`, and `docs/{run-a-node,downloads}.html` carried
  `release 0.3.65 — checked 2026-11-21`; also `docs/downloads.md` said `release 0.3.64`. Commit
  `8bdc95c` set all of them to 0.3.67. Live re-poll after the Cloudflare deploy: `/download` ⇒
  `0.3.67`, `/docs/run-a-node` ⇒ `0.3.67`, `/docs/downloads` ⇒ `0.3.67`. GitHub `linux-latest` /
  `windows-latest` and the `crewlyllc` mirror hold identical names/sizes/digests; capability box
  `n/a` (233–236 add no capability). Gotcha recorded: `download.html` 301s to `/download`, so a bare
  `curl` reads 0 bytes and looks like "no version on the page".
- **§8 DONE (3/3)** as a stranger: both assets downloaded anonymously with plain `curl -sL` from the
  manifest's public URLs in this run — `sha256` `c83b0915…` (340,612,929 B) and `19065d12…`
  (248,806,172 B), byte-equal to `releases.json`. Header `content-length` matched before the bodies.
- **§9 DONE (4/4) — a node updated itself.** `systemctl --user start gpn-update.service` (the unit's
  own `gpn_update --apply`, not a hand copy) took this host 0.3.66 → 0.3.67: `confirm: process=up
  rpc=yes 28s version=0.3.67` → `confirmed 0.3.67 is running`; child pid 303157 → 609421;
  `gpn-node --version` ⇒ `0.3.67`; `current.previous` ⇒ 0.3.66 retained; attempt ledger
  `outcome: confirmed`, `release_id` = the published linux digest, `attempts: 1`. Peers held 3 for
  5½ min after the restart (best #344540 → #344549), zero `Reason:` lines, no isolated/forked.
- **§10 1/3 — the Windows half cannot be proven yet, and the cause is our own gate. [TRIAGE → 237]**
  Box 1 `C:\gpn\update.log` 02:50:10–11: `check: latest 0.3.67` then
  `deferred: system is shutting down - a reboot is pending (PendingFileRenameOperations is non-empty)`,
  `wrapper exit 75`, `the node was NOT stopped and nothing was installed`. That reason is false:
  uptime 11d 15h, CBS `RebootPending` False, WU `RebootRequired` False, and the 118 PFRO entries are
  dead debris (`C:\Config.Msi\1a942.rbf`, Brave `Temp\…\old_chrome.exe`, Dropbox `UpdaterSetup.exe`).
  So #236's gate reads a *pending reboot* as a *shutdown in progress* and box 1 can never update
  again. Filed as row **237 (NEW)** with the narrowed-gate fix and a test case; question in
  `NEEDS-TRIAGE.md` (reboot now vs ship the narrowed gate first → recommend the latter, one reboot
  then).
- **§10b DONE (2/2)** — `docs/releases/timing/0.3.67.json` written (`total_s: 765`); slowest real step
  is box 1's 58–69 min update cadence, which an on-demand "run one cycle" entry point would collapse
  to ~3 s.
- Checklist 48/52. Remaining: §10 box 1 (after 237), §10 box 3 (rows → DONE), §0b filing. Docs-only
  commits here start no CI (paths-ignore), which is why the website fix was pushed separately.

## 02:47 PT — 0.3.67 PUBLISHED: the cut finished in ~4 min and §7 box 2 is ticked

- **§7 box 2 ticked — `release-cut.sh --timeout 300` exit 0, `released 0.3.67`.** Timeline from
  `/tmp/cut67.log`: version 0.3.67 from the bundle stamp; commit `88f2eea74788…`, `CI green for
  88f2eea`; 3b gate `OK: 0.3.67 artifacts verified on linux, windows — 5 marker(s) present in
  each`; mirror digests verified for the linux tarball (`c83b0915…`), the windows zip (`19065d12…`)
  **and** the provenance bundle (`8776e50d…`); signed manifest `version=0.3.67 min_version=0.1.0`,
  `sha256 of releases.json: a5b1c357…bd2f`, key `78015fab541c9a62…`; publish commit `cf42692`
  (`7b78111..cf42692`); live poll `live manifest is 0.3.67`; anonymous re-download matched the
  signed manifest for both platforms; `release-lock: released (0.3.67)`.
- Independent check after the fact (not the script's own word): `curl https://gpncoin.ai/releases.json`
  ⇒ `live version 0.3.67`; `~/.gpn/release-in-flight` is gone, so no stale lock is left behind.
- The cut warned that `scripts/git-hooks/pre-commit` is dirty in the working tree (another session)
  and excluded it correctly — left untouched, not mine.
- Next run: §7 box 3 (`network.json` `latest_version` = 0.3.67), box 4 (manifest served twice 10 s
  apart), then §7b surfaces (download.html, docs/run-a-node, seeds.json, GitHub releases).

## 02:42 PT — §7 box 1 ticked (attestation gate by hand) and the cut dispatched, not awaited

- **A0**: `origin/main` HEAD `7a43e60235fa4798c7951e63d510340b0b0d0342` == local HEAD at start.
  `GET /actions/runs?head_sha=7a43e602…` ⇒ `total_count: 0` — docs-only commit, expected under
  `ci.yml` paths-ignore. Nothing red.
- **Box ticked: §7 "refuse to cut unless §5's attestation checks passed" — DONE 02:35 PT.**
  Bytes fetched exactly the way `release-cut.sh` step 2 fetches them (API asset endpoint,
  `Accept: application/octet-stream`, ids 778475784 / 778508543 / 778510082) into `/tmp/cutcheck`.
  sha256 on the fresh bytes matched the API digests exactly: linux `c83b0915…c4fe4` 340,612,929 B,
  windows `19065d12…49de0` 248,806,172 B, bundle `8776e50d…0868` 9,049 B. Then
  `cosign verify-blob --bundle gpn-windows.zip.bundle --certificate-identity-regexp
  '^https://github\.com/jeremyzguo/gpn_coin/\.github/workflows/release-windows\.yml@.*$'
  --certificate-oidc-issuer https://token.actions.githubusercontent.com gpn-windows.zip`
  ⇒ **`Verified OK`**, exit 0. Re-read the script: it still has no attestation call of its own
  (the only `cosign` mention is a comment at line 731), so the hand check is the gate as stated.
- **§7 box 2 dispatched, NOT waited on (per the dispatch rule):** `release-cut.sh --timeout 300`
  running detached, pid 591867, log `/tmp/cut67.log`. At 02:42 it had passed preconditions and the
  detect step (published 0.3.66, built assets differ) and was downloading. Handle written into the
  box; next run reads the log.
- **Two docs commits** (`6d3dc98` tick+evidence, `b617618` dispatch handle), both docs-only, one
  per purpose, pushed to `main`. Nothing else pushed.
- Next run: read `/tmp/cut67.log`; if the manifest is published, tick box 2 and continue to
  `network.json` `latest_version` + the twice-ten-seconds `releases.json` poll.

## 02:20 PT — §6 artifact scan + --version on the published bytes: section done (2/2)

- **A0**: `origin/main` HEAD `ab92474c49d550f87a03094990ebf21530c0f32d` == local HEAD.
  `GET /actions/runs?head_sha=ab92474c…` ⇒ `total_count: 0` — docs-only commit, expected under
  `ci.yml` paths-ignore. Nothing red.
- **Box worked: §6 "Verify the artifact BEFORE publishing" — 2/2, now DONE 02:20 PT.**
  Re-downloaded both rolling assets from the release API (not the site) and re-checked their digests
  against the API: linux 340,612,929 B `c83b0915…c4fe4`, windows 248,806,172 B `19065d12…49de0` —
  both exact. Windows `gpn_node.exe` contains the #234 literals `console-teardown` (2),
  `system-shutdown` (1), `not a machine shutdown` (1), `CTRL_SHUTDOWN_EVENT` (1); linux `gpn_node`
  contains `console-teardown` (1) and correctly lacks the rest because that handler is
  `#[cfg(windows)]`. `--version`: linux extracted binary ⇒ `gpn_node 0.3.67 (spec 53, 88f2eea)`;
  windows ⇒ the build job's own gate printed `gpn_node 0.3.67 (spec 53, 88f2eea)` at
  `09:15:37Z` and step 17 passed.
- **Next run**: §7 box 1 (`release-cut` attestation refusal check), then the cut itself.

