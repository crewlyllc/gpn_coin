# Update checklist — every release, in this order

## Release policy (owner, 2026-11-16)

**A task is done only when every box in its checklist is ticked.** Each release gets its own
copy of this file at `docs/releases/<version>.md`; boxes are ticked only when genuinely
satisfied, and an item that does not apply is ticked as `n/a` with the reason. The task row
stays `IN-PROGRESS (blocked on <step>)` until the last box is ticked — and no new row starts
while one is unfinished.



- **A version combines everything that is finished but unreleased.** Never cut a release for a
  single change while others are sitting in the tree — the fleet then has to update twice for
  what is one batch, and each update is a restart, a risk and a wait.
- **Anything designed or started after that point goes into the NEXT version.** Once a release
  is cut, the batch is closed: new work accumulates for the following one. Do not reopen a
  release that is already published.
- **"Finished" means tested** (see the gate below), not merely committed. A change that cannot be
  tested stays out of the batch and is reported, not smuggled in.
- **One bump, one changelog entry, one push, one cut per batch.**


Order matters: **test first, then implement, then build, then publish, then prove.** Each line
names the failure it prevents. Copy this file to `docs/releases/<version>.md` when a release
starts and tick the boxes as they are actually satisfied — an untickable box means the release
is not ready.

---

## 0a. Sections get ticked, not just boxes

Every `##` section is itself an item. When **every box under a section is ticked**, rename the
section header to mark it done:

```
```
```text
## 2. Implement  DONE 2026-11-18  (3/3)
```

and when it is partially done, record the count so partial progress is visible without counting:

```
## 2. Implement  (2/3 - call-site unchecked)
```
```
```

Rules:
- A section is **not** done because its boxes look done — it is done when each box carries evidence.
- Never tick a section to make the file look finished. A half-ticked section is information.
- `scripts/checklist-status.py <file>` prints the per-section tally; use it rather than eyeballing.
- The final gate is still §9/§10: version published and a node reporting it. A fully ticked file
  with no published release is not done.

## 0c. BLOCKED is only for an owner decision (owner rule, 2026-11-19)

**An item may be marked `BLOCKED` for exactly one reason: it needs a design decision that is
Jeremy's to make.** Nothing else blocks an item.

- A missing tool, a red build, a failed test, a permissions problem, a flaky runner, a broken
  script, a wrong path, a missing credential on this machine: **solve it.** Install it, fix it,
  work around it, or replace the approach.
- If a design question arises in the middle of work, decide whether it is genuinely the owner's
  (a product, economic or trust decision) or an implementation choice (mine). Implementation
  choices do not block; make one, note it, move on.
- When something really is the owner's: state the question in one line, mark the item `BLOCKED`
  with that question as the reason, and carry on with other work. Do not stop the bundle.
- `BLOCKED` with any other reason ("no time", "tool missing", "needs a decision from me") is a
  mis-use of the state and must be converted into work.

## 0b. Every tick carries notes; every finished checklist is filed

**Notes.** A tick is not just a checkmark: append what happened, in this shape —

```
    [x] The test passes — 105 passed, 0 failed   (example, indented on purpose)
      notes: the confirm gate needed a 30 s start grace; without it a restart that was
             still spawning was declared dead at t=0.
```

Rules for notes:
- **One line after the box**, indented, prefixed `notes:` — why it passed, what was surprising,
  what was deferred, what broke and was fixed on the way.
- Keep it to the things a reader six months from now could not infer from the diff.
- Never leave a tick with no note at all — `— <command or value>` inline is the minimum.

**Filing.** When §10's last box is ticked and the version is published:
1. move the file to `docs/releases/archive/<version>.md`
2. add a row to `docs/releases/INDEX.md` — version, publish date, the item ids it carried
3. leave a one-line pointer in the list row (`COMPLETE 0.3.60 — archive/0.3.60.md`)

An unfiled checklist is a finished release nobody can find later.

## 0. Sequencing — the rule that binds the rest

- **Work the checklist in order.** You may not tick item N+1 while item N is unticked, and you
  may not start a new task shecklist while this one is unfinished.
- **While this checklist is open, nothing else may be committed.** Not docs, not fixes, not a
  small correction. The only permitted push is this release's own — every other commit cancels
  a queued CI run for the release that is in flight.
- **Mechanical, not a promise:** `release-cut` and the driver take `~/.gpn/release-in-flight`;
  a `pre-commit` hook refuses any commit while it exists. If the hook is missing on a machine,
  say so rather than relying on discipline.
- **A tick is a claim with evidence**: version string, log line, or commit sha next to it.
  A box ticked without evidence is not ticked.

## 1. Test first (before any code is written)

- [ ] Name the behaviour in one sentence, and the machine-observable thing that proves it.
- [ ] **Write the test that fails now.** Paste the failing output. No fix starts without this.
- [ ] If the change cannot be tested, **stop and tell Jeremy** — what it is, why it cannot be
      tested, and what would be needed. Never proceed silently.

## 2. Implement

- [ ] The test passes.
- [ ] `cargo fmt --check` clean; clippy with CI's `-D warnings` clean.
- [ ] Full affected suites run, not just the new test.
- [ ] **A call site exists** for anything new — grep the caller, not the definition.
      *Election and self-heal were both "implemented" with no caller.*
- [ ] Nothing new is off by default behind a flag nobody sets.
- [ ] The change is in the tree you are about to release:
      `git log --oneline -3 -- <file>` *(`fork_choice.rs` shipped in 0.3.50 while the binaries
      contained none of its strings.)*

## 3b. Items in this release  <!-- generator inserts them here, do NOT hand-edit -->

<!-- make-release-checklist.py replaces this comment with one box per IN-PROGRESS row from
     GPN-LIST.md. Nothing else goes in this section: no blocked rows, no notes, no samples. -->

## 3. Bump and record

- [ ] `scripts/bump-version.py patch`
- [ ] CHANGELOG section written — what a user notices, no `_TODO_`
- [ ] `scripts/bump-version.py --check` → `lockstep: 15 manifest(s) agree`
- [ ] Seed lists still agree across `network.json`, `seeds.json`, `install.ps1`, `install.sh`,
      `run-windows.cmd`, the launcher template and the release workflow (parity test green).

## 4. Push (once)

- [ ] **A docs-only commit must not start a build** — `ci.yml` ignores `docs/**`, `**/*.md`, the
      campaign queues and `brand/**`; anything that can change behaviour is *not* ignored. If a
      documentation push starts a CI run, `paths-ignore` has been broken and the runner is being
      spent on prose. (Added 2026-11-19: five docs commits in 40 minutes kept the runner busy and
      finished nothing, because each push superseded the previous run.)
- [ ] Batch documentation edits into ONE commit. One commit per question is how a single-slot
      runner spends an hour verifying nothing.

- [ ] Nothing else pushed in the same window; no push while CI runs for HEAD.
- [ ] CI green on that exact SHA: fmt, clippy, secret-scan, Linux build, **Windows build**.

## 5. Build both platforms

**The rule, in one line (owner, 2026-11-19):** the final build of every version for Windows is
done **through the runner** (so it carries an attestation); **everything else Windows is done over
SSH on box 1.** No per-attempt dispatches, no install tests through CI, no queue in the loop.

**Ordering when both land on box 1:** SSH verification work **yields** while a release job is in
flight — that job builds, verifies, publishes, attests and then replaces `C:\gpn\bin`, so an
install test running at that moment would race the deploy. Check the runners API (`busy`) before
any mutating SSH step; read-only checks (firewall rules, `Get-MpPreference`, logs) are always safe.

Two different jobs use Windows, and they must not be confused (owner question, 2026-11-19):

| What | Route | Why |
|---|---|---|
| **Per-item Windows proof** (a box in §3b: firewall rules, Defender, install) | **SSH to box 1** — `scripts/box1.sh "<powershell>"` | seconds, not a queue. One single-slot runner means every dispatch waits behind whatever build is running there |
| **Release artifact build** (`gpn-windows.zip`) | **the runner**, once, at cut time | the artifact strangers download must come from the process that signs and publishes it |

- [ ] Per-item Windows evidence was collected **over SSH** (paste the PowerShell output), not by
      dispatching the runner for each iteration.
- [ ] `release-linux-selfhosted.yml` → `gpn-linux.tar.gz` on `linux-latest`
- [ ] `release-windows.yml` on box 1's runner → `gpn-windows.zip` on `windows-latest`
- [ ] **Same SHA on both runs.** macOS is not required and is not in the manifest.
- [ ] **The Windows build carries a GitHub build attestation** (owner requirement 2026-11-19):
      `actions/attest-build-provenance@v2` runs in the release workflow with `id-token: write`
      and `attestations: write`, subject = the released zip. Verify with
      `gh attestation verify gpn-windows.zip --repo jeremyzguo/gpn_coin` and record the output.
      Reason: an SSH build produces a binary but no provenance; the runner is kept **for this**,
      not for convenience.

## 6. Verify the artifact BEFORE publishing

- [ ] **Scan the built binaries for strings the change added.** Use literals from *code*:
      `reorg=`, `work ours=`, `gpn-forked` — never module or constant names.
- [ ] The binary's `--version` matches the version being released.

## 7. Cut and publish

- [ ] `scripts/release-cut.sh --timeout 300` — mirrors both assets into `crewlyllc/gpn_coin`,
      signs `website/releases.json`, publishes, re-downloads to verify.
- [ ] `network.json` `latest_version` equals the published version.
- [ ] `releases.json` serves the new version **twice**, ten seconds apart.

## 7b. Every surface that names a version must be updated

- [ ] **gpncoin.ai serves the new version** — `releases.json` version equals the crate version, polled twice; `download.html`, `docs/run-a-node` and `seeds.json` show the same version. The site is the version a stranger sees first.
- [ ] **GitHub carries the same version** — the `linux-latest` and `windows-latest` releases on `jeremyzguo/gpn_coin` hold the new assets, the mirror repo `crewlyllc/gpn_coin` holds the renamed copies the manifest points at, and `network.json`'s `latest_version` matches.
- [ ] **Both surfaces agree with each other and with the binaries** — site version == GitHub release version == `gpn-node --version` of the built artifact. A mismatch here is how "0.3.52 is live" and "the fleet runs 0.3.50" both end up true.

## 8. Verify as a stranger would

- [ ] Anonymous download of **both** assets from `gpncoin.ai` succeeds.
- [ ] **sha256 and size match** the manifest for both.
- [ ] Docs stamp and `downloads.md` show the same version; the live URL was polled, not the repo.

## 9. Deploy and confirm

- [ ] One node updates **itself** — no hand-copying.
- [ ] The **running process** reports the new version (check `(Get-Process gpn-node).Path` first).
- [ ] Log shows `check → stop → download → swap → start → CONFIRMED <version> running`.
- [ ] Previous version retained for rollback; a failed update leaves the node **running**, not down.

## 10. Prove the behaviour

- [ ] The change observed on a real machine, with timestamps and the actual output.
- [ ] Peers hold >5 minutes; no new `Reason:` bans; no unexpected `gpn-isolated`/`gpn-forked`.
- [ ] Mark the task row `DONE (date)` with version, commit sha and the evidence line.

---

## 10b. Record how long the steps took

- [ ] `scripts/release-timing.py <version> --json docs/releases/timing/<version>.json` and commit
      `docs/releases/timing/<version>.md`. It replays the checklist's git history and reports, per
      box, when it was ticked, elapsed since the checklist existed, and the gap since the previous
      tick — so the next estimate is measured rather than guessed.
- [ ] Name the slowest step and say what would shorten it. A step that is slow because it waits on
      a machine is not the same as one that is slow because it was re-done three times.

## 11. If a step is stuck — escalate, do not wait

Every step has a budget. **Exceeding it is a signal to stop and review that step, not to keep
waiting.** Waiting is what turned a 20-minute CI run into eight hours of silence.

| Step | Budget | When exceeded |
|---|---|---|
| 1-2 tests/implement | 2 h | the change is bigger than the row; split it or report |
| 3 bump/lockstep | 15 min | a manifest disagrees — find which one, do not bypass |
| 4 CI green | 60 min | read the failing job, or check the runner is online; a queued run with 0 jobs for 10 min is stuck |
| 5 both builds | 40 min | check the runner claimed the job; Windows cold build is 40-75 min, so re-check before declaring stuck |
| 6 artifact scan | 5 min | if the strings are missing, **stop the release** — do not publish a build without the change |
| 7 cut | 15 min | `release-cut` is idempotent; re-run with `--dry-run` first and read what it refuses |
| 8 anonymous download | 10 min | Cloudflare cache or a missing mirror; poll again, then check the asset exists |
| 9 deploy/confirm | 15 min | read `update.log` and `last-exit.json`; a failed update must have rolled back |
| 10 prove behaviour | 60 min | if it cannot be observed, say so and mark the row BLOCKED with the reason |

**When a budget is exceeded, do this — in order, and no guessing:**

1. **Read that step's own logs** (job log, `update.log`, `release-cut` output, node log). The
   step is failing for a reason it has already recorded.
2. **Check the version history around it** — did this step ever work, and what changed since?
3. **State the blocker in one line** in the release's checklist copy, with the timestamp.
4. **Choose a path**: retry once, work around it, or escalate to Jeremy with that one line.
5. **Never repeat the same attempt twice without new information.** A second identical try is a
   guess wearing a uniform.

**Escalate to Jeremy when**: a step is stuck past its budget twice, a decision is needed
(publish anyway? accept a degraded artifact?), or a step cannot be performed at all. One line:
what is stuck, since when, what it needs.

---

## Must never regress

- Private/LAN addresses in shipped defaults (0.3.29 broke every remote box)
- Loopback in advertised listen addresses (caused box 1's self-dial storm)
- Launcher execing a stale `bin\` copy instead of `versions/current`
- Env flags that silently disable a feature (`GPN_SITE_ELECT`, `GPN_SELF_HEAL`)
- Mechanisms with no caller — grep for a call site, not a definition
- A version that moves in `Cargo.toml` but not in `releases.json`, or vice versa

## Persistent working rules (carried into every release, and into the cron)

These live here, in the repo, so they survive any job being recreated. The cron reads this file;
it does not carry its own private copy of the rules.

### 0a. Check CI for HEAD before any box
Ask GitHub for the CI run on `origin/main`'s HEAD.
- **failure** → that is the box: read the failing job's log, **quote the exact error line**,
  classify it (our code / our test / environment / upstream), fix the cause, run the narrowest
  local check, push once. Nothing else proceeds while HEAD is red.
- **queued / in progress** → wait, say nothing.
- **success** → continue.

Reporting "CI is red" without the failing line is noise, not a status.

### 0c. An empty query result is not evidence of absence (added 2026-11-19)
A `Get-Service` that returns nothing, a `grep` with no matches, a `journalctl -u` that prints
nothing: each of these means *that command found nothing*, not *the thing is absent*. Before
claiming something is missing, ask it a different way:

- Is a Windows process running? `Get-Process`, `Get-CimInstance Win32_Process`, `Get-ScheduledTask`
  — not `Get-Service` (a per-user runner has no service entry).
- Is a CI runner available? the **GitHub API** (`/actions/runners`) — not anything local.
- Is a unit logging? `journalctl --user -u <unit>` — a non-`--user` query returns zero matches for
  a *healthy* user unit, which reads identically to a dead one.

**A queued job behind a busy single-slot runner is contention, not a fault.** Say which it is, with
the field that proves it (`status`, `busy`), before proposing a repair.

### 0b. When something fails, diagnose and fix — do not park it
A red CI, a failed build, a failed start, a failed update, a failed test, a failed publish: each
has a cause recorded somewhere.
1. **Read the failure itself** — job log, node log, `last-exit.json`, command output, the test's own
   assertion. Quote the line.
2. **Classify it** — our code, our test, environment (disk, offline runner, missing tool), upstream.
3. **Fix the cause** — change the code; correct or name the environment problem; add a guard on our
   side for upstream behaviour.
4. **Re-run the narrowest thing that proves it.**
5. Only if it genuinely cannot be fixed here: one line saying what it needs, then the next box.

### 0c. Long steps are dispatched, not waited on
A build or deploy outlasts one run. Start it, **write the handle (run id / commit sha) into the
checklist box**, stop. The next run polls it once and continues. State lives in the checklist,
never in a run's memory.

### 0d. Evidence must be re-derivable
Tick with something a machine can re-run:
- good: `releases.json version=0.3.61 (polled 23:41Z)`, `Get-Process gpn* → MainWindowTitle blank`, a commit sha
- not a tick: "verified", "confirmed working", "looks right"

### 0e. Self-check the previous tick
Before starting a new box, re-run the previous tick's evidence line. If it no longer holds,
un-tick it and say which claim failed.
