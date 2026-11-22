## 07:05 PT AUDIT — fixed 1 (disk) / everything else clean [no triage]

**What I checked (commands, in order)**
- `systemctl --user list-units 'gpn*' --all` + `list-unit-files | grep -i gpn` — 23 units loaded.
  Enabled **and** running: gpn-fleet, gpn-accesspoint, gpn-provider, gpn-diffusion, gpn-finetune,
  gpn-explorer, gpn-faucet, gpn-oracle, gpn-provider-heartbeat, gpn-fleet-directive + 5 active timers
  (ap-topup, cadence-gate, ci-cache-prune, deploy-drift, prune-ci-cache, update). **No enabled-but-dead unit.**
  `gpn-node.service` is `disabled/dead` **by design** — gpn-fleet runs the node (pid confirms below).
- Ports: `9944 LISTEN, 8092 LISTEN, 8093 LISTEN, 8099 LISTEN, 11434 LISTEN` (ss).
- Chain: `system_version` = **0.3.67**; block **#345513** and advancing (#345508 → #345513 across the reads, ~10 s/block);
  `system_peers` = **3** (all AUTHORITY, matching best numbers); `system_chain` = "GPN Public Testnet".
  Observation, not a fault: `finalized #0` still reads 0 in the fleet footer — unchanged from previous days.
- `/v1/models` on :8092 answers with 5 ollama models (deepseek-r1:14b/7b, llama3.2:1b, qwen2.5:0.5b/7b).
  8093 `/health` = `{"status":"ok"}`, 8099 `/` = 200, 11434 `/api/tags` lists models.
- Fleet journal 24 h: `grep -Eci 'panic|Reason:|ban|Incompatible|update fail|taskkill'` = **0**.
  Journal is normal proposal/import/idle traffic; self-heal divergence check at 07:02:25 = `match` against signed checkpoint #344777.
- `gpn-cadence-gate.state` (15:01:22Z): **PASS** — blocks 345411..345510, max gap 79.27 s, 0 clause-1 violations, 7 suspect gaps all explained by `sealer: still searching template`.
- `gpn-deploy-drift.state` (15:01:21Z): `exit: 0`, empty message (no drift).
- Update: `~/.gpn/update/update-attempts.json` last record = `{"version":"0.3.67","outcome":"confirmed","release_id":"c83b0915…c4fe4","attempts":1}`.
  **Path drift worth noting:** the audit brief points at `~/.gpn/update.log` and `~/.gpn/update-attempts.json`;
  neither exists — the real locations are `~/.gpn/update/update-attempts.json` and `~/.gpn/update/current`.
- Release loop `feff1358…`: last 8 runs all `finished ok`, no timeouts. Latest (15:00Z) ticked 0.3.68 §4 on run 45024973751 (success, 4 jobs green).
- Release record consistency: **site `website/releases.json` version 0.3.67 == running node `system_version` 0.3.67 == INDEX row 0.3.67 == `docs/releases/archive/0.3.67.md` == GPN-LIST rows 233-236 COMPLETE 0.3.67**. No version missing any of the three records.
- Box 1 over SSH (`scripts/box1.sh`): `C:\gpn\data\update\current\gpn-node.exe`, `system_version` **0.3.67**, **2 peers**.
- Resources: `free -h` available **23 GiB**; load 4.76 (mining + 100 % GPU, normal); GPU 1445/8188 MiB.
- Heartbeat: two `push failed: no local libp2p identity yet` lines at 02:58 / 03:05 during the 0.3.67 restart window; **recovered** — zero journal entries since 03:10 and live `/status` now carries the peer identity, `gflops 824` (attested-idle), `disk_free_pct 20`.

**What was wrong: disk at 18.2 % free (80 G/439 G) — cleaned**
Owner rule is cleanup every run and stop at ≥ 20 %. `pgrep -x cargo` showed a **release build in flight**
(pids 744081/744748/744822, `CARGO_TARGET_DIR=~/.ci-cache/gpn-agent-target`, workdir the runner checkout),
so `~/.ci-cache`, every `*/target`, and no restarts were touched — correctly deferred.
Deleted only `/tmp` scratch whose newest file was ≥ 2 days old and which no process held open (`lsof`, `/proc/*/cwd` both clean):

| removed | size |
|---|---|
| /tmp/rel0360, /tmp/rel060, /tmp/dlcheck (0.3.60 verification trees) | 557M + 557M + 561M |
| /tmp/anon063 | 561M |
| /tmp/rel061, /tmp/v061 (0.3.61 trees) | 1.3G + 1.3G |
| /tmp/gpn064 (0.3.64 tree) | 1.3G |
| /tmp/gpnlx, /tmp/gpnscan | 960M + 960M |
| /tmp/attest066, /tmp/win065 (0.3.65/66 zips) | 752M + 752M |
| /tmp/row228 | 860M |
| /tmp/rel67-target (cargo scratch, not the live target dir) | 1.6G |
| `/tmp/*.tar.gz|*.zip` older than 3 d (incl. gpn-testnet-snapshot-20261114, stranger-linux) | ~600M |

**Free before → after: 80 G (18.2 %) → 92 G (21.0 %)**, `df -h /` = `325G used, 92G avail, 78 %`.

**Deliberately left, and why**
- `~/.ci-cache` **79 G** — owner rule says keep, *and* the release build is writing to it right now.
- `~/.cache/huggingface` **53 G**, `~/ollama` **8.1 G** — models, owner rule: never delete.
- `gpn-agent/target` **16 G**, `gpn-node/target` **8.5 G** — cargo/rustc live; deleting would break the in-flight build. Revisit next run when idle.
- `~/.openclaw/workspace/shared` **14 G**, `plugin-runtime-deps` **1.5 G**, credentials — owner rule.
- `~/.openclaw/agents/*/sessions` **374 M** (150 files / 150 M older than 30 d) and `~/.openclaw/cron/runs` **33 M** (16 files / 7.5 M older than 30 d) — **report only**, these are the conversation record.
- `/tmp/cutcheck`, `/tmp/rel67-lin`, today's tarballs — today's release-loop scratch, may be in use.

**Top five disk consumers after cleanup (hidden paths included)**
1. `~/.ci-cache` 79 G (build cache, owner-protected)
2. `~/.cache/huggingface` 53 G (models)
3. `~/.openclaw/workspace/projectmanager07/gpn-agent/target` 16 G (debug+release build tree)
4. `~/.openclaw/workspace/shared` 14 G (data)
5. `gpn-node/target` 8.5 G + `~/ollama` 8.1 G
`state/bin-snapshots` **does not exist** (nothing to prune there); `/tmp` now 8 G (was 21 G).

**Remains / next run**
- Runners `openclawassist` and `DESKTOP-USBUADL` are both **busy=True** on runs 45035777994 (linux) / 45035780136 (windows), tag `build/1bc9488bb7fc` — 0.3.68 publish in flight. No restart, no cache prune, no target dir deletion until those conclude.
- Reclaim `*/target/{debug,incremental}` (~24 G combined) the first run after cargo is idle, if free is still < 25 %.
- GPN-LIST 237/238 IN-PROGRESS are bundled in 0.3.68 (expected, not a fault); 231 AWAITING-OWNER on the offline sudo key (docs/NEEDS-TRIAGE.md §231) — unchanged, not escalated again.

## 06:58 PT — 0.3.68 §4 TICKED (run 45024973751 concluded `success`); both release builds dispatched, handles written [no triage]

- **A0**: `origin/main` HEAD `d6a187717d77` (docs-only) ⇒ `head_sha` query `total_count: 0`, `paths-ignore` intact, no run expected.
- **§4 ticked on evidence, not optimism**: `GET /actions/runs/45024973751` ⇒ `status=completed conclusion=success`,
  `head_sha=1bc9488bb7fc9a06dd4790b867178b907f0b301a`, created 12:57:57Z, updated 14:33:59Z. Jobs:
  `fmt + clippy` 137220923136 success · `secret-scan` 137220923314 success · `Build + unit tests (linux)` 137221062985 success ·
  `Build + unit tests (windows, self-hosted)` 137220923278 success. GPU/macOS/audit skipped by design.
- **§5 dispatched and the run ended immediately** (per the dispatch rule — no waiting in-run):
  tag `build/1bc9488bb7fc` created (workflow_dispatch refuses a raw sha), then
  `release-linux-selfhosted.yml` ⇒ run **45035777994** (06:59:30 PT), `release-windows.yml` ⇒ run **45035780136** (06:59:31 PT),
  both `head_sha=1bc9488bb7fc`, both `in_progress`. Handles written into the two §5 boxes.
- No blockers, no decisions needed. Next run: read those two run ids once each; success ⇒ tick the build/publish boxes in order
  (§6 artifact scan, §7 cut+publish), failure ⇒ quote the failing step's line and classify.

## 06:32 PT — 0.3.68 §4: Windows job verified compiling (two rustc spawned at 06:32 PT), run age 94 min but NOT stale — nothing cancelled [no triage]

- **A0**: `origin/main` HEAD `d3051e03f235` (docs-only) ⇒ `head_sha` query `total_count: 0`, `paths-ignore` intact, no run expected for it.
  Release SHA `1bc9488bb7fc` run **45024973751**: `in_progress`, created `12:57:57Z`, API `updated_at 13:18:26Z`.
- **Why the 90-minute stale rule did not fire**: run age is 94 min, but staleness is a claim about progress, not age.
  Read-only SSH to box 1 at 06:32:17 PT (`scripts/box1.sh`) returned live compilers: `cargo 48112` started `6:16:24 AM`,
  `rustc 43884` started `6:32:09 AM`, `rustc 46116` started `6:32:16 AM`, `Runner.Listener` count `1`.
  Job `137220923278` step 7 `Build + test gpn-agent (release)` began `13:55:16Z`; a cold Windows agent release build is 40-75 min,
  so 13:55+75 min ≈ `15:10Z` is its normal finish. Cancelling a build that spawned two rustc 8 seconds before the check would have thrown away ~40 min of work.
- **Lanes on `1bc9488`**: `fmt + clippy` `137220923136` success · `secret-scan` `137220923314` success ·
  `Build + unit tests (linux)` `137221062985` success (26/26 steps, incl. step 7 `Test gpn-agent (non-GPU)`) ·
  Windows step 6 `Build + test gpn-node (release)` success `13:55:16Z`. GPU/macOS/audit skipped by design.
- **No box ticked.** First unticked box is §4's green-on-exact-SHA line; 4 of 5 required lanes concluded. A tick on four of five is a claim without evidence.
- **Next run**: read 45024973751's conclusion once. `success` ⇒ tick §4, dispatch `release-linux-selfhosted.yml` + `release-windows.yml` for
  `1bc9488`, write both run ids into the §5 boxes, end the run. `failure` ⇒ quote the failing step's exact line, classify, fix the cause.

## 06:12 PT — 0.3.68 §4: four lanes green, Windows job alive and progressing (rustc at 6:12 AM on box 1); no box ticked [no triage]

- **A0**: `origin/main` HEAD `0490257` (docs-only) ⇒ no run expected. Release SHA `1bc9488bb7fc` run **45024973751**
  still `in_progress` (created 12:57:57Z, 74 min at read time), `updated_at 13:18:26Z`.
- **Lanes**: `fmt + clippy` success, `secret-scan` success, `Build + unit tests (linux)` success (26/26 steps incl. step 7
  `Test gpn-agent (non-GPU)`), GPU/macOS/audit skipped by design. **Windows `137220923278`**: step 6 `Build + test gpn-node (release)`
  **success**, now at step 7 `Build + test gpn-agent (release)`.
- **Liveness proven, not assumed** (read-only SSH to box 1, safe during a build job): `cargo` running since 05:55:16,
  two `rustc` processes started 06:11:49 and 06:12:01, runner `DESKTOP-USBUADL` `online busy=True`. The job is compiling, not hung.
- **No box ticked** — the first unticked box is §4 "CI green on that exact SHA: … **Windows build**"; 4 of 5 concluded.
  Run age 74 min < 90-min stale threshold: nothing cancelled, nothing re-dispatched, per the dispatch rule nothing waited on.
- **Next run**: read 45024973751 once. `success` ⇒ tick §4, then dispatch `release-linux-selfhosted.yml` + `release-windows.yml`
  for `1bc9488`, write run ids into §5 boxes, end the run. `failure` ⇒ quote the Windows failing step's line, classify, fix.

## 05:51 PT — 0.3.68 §4: linux CI green on 1bc9488 (the prune-timer failure does not reproduce); Windows job still building [no triage]

- **A0**: `origin/main` HEAD `52a36c5e` (docs-only) ⇒ `head_sha` query `total_count: 0` — `paths-ignore` intact.
  Release SHA `1bc9488bb7fc` run **45024973751**: `in_progress`, created 12:57:57Z (54 min at read time). Not waited on.
- **New since last run — the linux half concluded green.** `Build + unit tests (linux)` `137221062985`
  **success**, 13:18:25→13:41:43Z, all 26 functional steps green including step 7 **`Test gpn-agent (non-GPU)`** —
  the exact step that died on `ab3c502` with `failed to write …/invoked.timestamp` / `No such file or directory` (exit 101)
  at 04:20:42 PT. That failure was the prune timer. It does not reproduce under `prune-ci-cache-daily.sh`. Environment cause confirmed, not our code.
- `fmt + clippy` `137220923136` **success**; `secret-scan` `137220923314` **success**; GPU/macOS/audit lanes **skipped** by design.
- **Still open**: `Build + unit tests (windows, self-hosted)` `137220923278` in_progress, started 13:16:37Z, at step 6/14
  (`Build + test gpn-node (release)`) — a cold Windows release build is 40-75 min, so ~13:56-14:31Z is its normal finish.
- **No box ticked.** The first unticked box is §4 "CI green on that exact SHA: fmt, clippy, secret-scan, Linux build, **Windows build**" —
  four of five are now green, the fifth is in flight. Ticking it on four of five would be a claim without evidence.
- Run age 54 min < the 90-minute stale-release threshold — nothing to cancel, nothing to re-dispatch.
- **Next run**: read 45024973751's conclusion. `success` ⇒ tick §4 and go to §5 (dispatch `release-linux-selfhosted.yml` and
  `release-windows.yml` for `1bc9488`, write the run ids into the boxes, end the run). `failure` ⇒ quote the Windows job's failing
  step line and classify before touching anything.

## 05:31 PT — 0.3.68 §4 still waiting on a conclusion; 1bc9488's CI half is green, both build jobs in flight [no triage]

- **A0**: `origin/main` HEAD `a07ee2f` (docs-only) ⇒ `head_sha` query `total_count: 0` — `paths-ignore` intact.
  Release SHA `1bc9488bb7fc` run **45024973751**: `in_progress`, created 12:57:57Z (~34 min at read time). Not waited on.
- **Job-level read at 13:31Z**: `fmt + clippy` **success** (`137220923136`, 13:16:37→13:17:06);
  `secret-scan` **success** (`137220923314`, 13:17:07→13:17:18);
  `Build + unit tests (windows, self-hosted)` `137220923278` in_progress;
  `Build + unit tests (linux)` `137221062985` in_progress since 13:18:25.
  Runners API: `DESKTOP-USBUADL busy=true`, `openclawassist busy=true` — single-slot contention, not a fault.
- **No box ticked this run, deliberately.** The first unticked box is §4 "CI green on that exact SHA",
  which can only be answered by that run's conclusion; §5–§7 are ordered behind it (the release build must
  follow green CI on the same SHA), so dispatching a release now would violate §0 sequencing. Run is 34 min
  old, well under the 90-minute stale-release threshold — nothing to cancel, nothing to re-dispatch.
- **Next run**: read 45024973751's conclusion. If `failure` — quote the failing step's own error line and
  classify (code / test / environment / upstream) before touching any box.

## 05:12 PT — 0.3.68 §5 SSH-evidence box ticked; 45018066004's red explained (prune timer), its successor still queued [no triage]

- **A0**: main HEAD `180e227` (docs-only) ⇒ `total_count: 0` runs, `paths-ignore` intact.
  The release SHA `1bc9488` has run `45024973751` in `pending` — noted, not waited on.
- **Diagnosed 45018066004 instead of parking it.** Linux job `137200899218` died in *Test gpn-agent*
  with `error: failed to write …/.fingerprint/gpn-agent-a73593bae3614032/invoked.timestamp` /
  `No such file or directory`, `exit code 101`, at `12:20:42Z` = **04:20:42 PT**. Cause: the prune
  timer, at `04:20:21` that morning, still running the old bare `find ~/.ci-cache -mtime +7 -delete`.
  Not our code. The unit now runs `scripts/prune-ci-cache-daily.sh` (build-in-flight + 20-min-quiet
  guards) and ran clean at `04:36:11`. Same run: `secret-scan` and `fmt + clippy` both green on `ab3c502`.
- **Box ticked — §5 "Per-item Windows evidence over SSH"**: `scripts/box1.sh` read-only probe on box 1
  ⇒ `VERSION=gpn-node.exe 0.3.67`, `UPDATE_RUNLEVEL=Highest`, `NODE_RUNLEVEL=Highest`, `PFRO_COUNT=118`,
  `NODE_PATH=C:\gpn\data\update\current\gpn-node.exe`. Per-item proofs are the §3b PowerShell runs (237:
  `RESULT: PASS`, `EXIT=0`, 27 ok under 5.1.22621.4249; 238: fail-before XML parse error → pass-after).
- **Still open**: §4 CI-green box tracks `1bc9488` (run `45024973751`), whose Windows job is the one
  currently building on box 1. Not ticked without a conclusion.

## 05:10 PT — 0.3.68 §3b/238 ticked: the update task's XML was never well-formed; registration had been failing rc=1 on every box [no triage]

- **A0**: `origin/main` HEAD `c8e1e7de…` == local; `GET /actions/runs?head_sha=c8e1e7de…` ⇒ `total_count: 0`
  (docs-only, `paths-ignore` intact). Nothing red to work.
- **§3b box 238 worked for real, and it was not a checkbox exercise — the feature had never worked.**
  `install.ps1`'s `$UpdateTaskXml` comment contained
  `Box 1, 2026-11-17 07:11:39 -- every gpn process answered` and
  `this XML is elevated -- it registers GPN-Node with HIGHEST --`. **`--` is illegal inside an XML
  comment**, so `schtasks /Create /XML` answered **rc=1 on every box**, `Register-UpdateTask` returned
  `$false`, the installer fell back to the Startup-shortcut loop, and `Assert-UpdateTaskElevation`
  re-registered the *same broken XML* — the one mechanism written to heal "updater at Limited" could
  never fire. That is why 238's field state survived installs.
- **Fix (commit `1bc9488`, `install.ps1` sha256 `a322cc4f4c13…5d4334`):** comment rewritten without
  `--`; `Register-UpdateTask` now parses the rendered task XML with `System.Xml` **before** calling
  schtasks and names the parser's error instead of an opaque rc=1; test H added.
- **Evidence, box 1, Windows PowerShell 5.1.22621.4249 (the task's own interpreter):**
  fail-before on `HEAD~1:install.ps1` ⇒ `FAIL H the rendered update-task XML is well-formed  parse error:
  An XML comment cannot contain '--' … Line 38, position 39`, `FAILURES: 3`;
  pass-after ⇒ `RESULT: PASS (all checks)`, 21 ok, `EXIT=0`.
- **Live self-heal on the real box, no manual step:** regressed the task ⇒ `before: update=Limited
  node=Highest`; shipped `install.ps1 -UpdateTaskOnly` ⇒ `scheduled task GPN-Update registered …` +
  `GPN-Update runs at RunLevel=Highest (can stop GPN-Node, RunLevel='Highest')` ⇒
  `after: update=Highest node=Highest`, `EXIT=0`. Scheduled path then ran at that level:
  `LastRunTime 2026-11-22 04:56:56`, `LastTaskResult 0`, `update.log [04:56:14] check: -> up to date (0.3.67)`.
  The stop-under-elevation half was observed 03:16 PT (`CONFIRMED: 0.3.67 … after 123 s`); the deployed
  0.3.68 cycle lands at §9/§10.
- **Class swept, not just the instance:** no `<!--` block in any `.ps1`/`.xml`/`.cmd` in the tree
  contains `--` (`bad_comment_files 0`); `install.ps1` is 0 non-ASCII bytes (the WS5.1 ANSI-decode guard).
- **§4 CI box note updated:** the code commit for this box is `1bc9488`, which supersedes `ab3c502` for
  the "CI green on that exact SHA" box; CI run **45024973751** was created `pending` for it. Next run reads
  its conclusion — not waiting here, per the dispatch rule.

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

