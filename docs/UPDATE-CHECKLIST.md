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

## 3. Bump and record

- [ ] `scripts/bump-version.py patch`
- [ ] CHANGELOG section written — what a user notices, no `_TODO_`
- [ ] `scripts/bump-version.py --check` → `lockstep: 15 manifest(s) agree`
- [ ] Seed lists still agree across `network.json`, `seeds.json`, `install.ps1`, `install.sh`,
      `run-windows.cmd`, the launcher template and the release workflow (parity test green).

## 4. Push (once)

- [ ] Nothing else pushed in the same window; no push while CI runs for HEAD.
- [ ] CI green on that exact SHA: fmt, clippy, secret-scan, Linux build, **Windows build**.

## 5. Build both platforms

- [ ] `release-linux-selfhosted.yml` → `gpn-linux.tar.gz` on `linux-latest`
- [ ] `release-windows.yml` on box 1's runner → `gpn-windows.zip` on `windows-latest`
- [ ] **Same SHA on both runs.** macOS is not required and is not in the manifest.

## 6. Verify the artifact BEFORE publishing

- [ ] **Scan the built binaries for strings the change added.** Use literals from *code*:
      `reorg=`, `work ours=`, `gpn-forked` — never module or constant names.
- [ ] The binary's `--version` matches the version being released.

## 7. Cut and publish

- [ ] `scripts/release-cut.sh --timeout 300` — mirrors both assets into `crewlyllc/gpn_coin`,
      signs `website/releases.json`, publishes, re-downloads to verify.
- [ ] `network.json` `latest_version` equals the published version.
- [ ] `releases.json` serves the new version **twice**, ten seconds apart.

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
