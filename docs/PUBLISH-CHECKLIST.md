# Publishing a GPN release — checklist and stall points

Written 2026-11-15 after a day in which seven fixes were committed and **none were released**.
The pattern was always the same: fix → report → fix again, with the release step never running.

## The checklist (in order, no skipping)

1. **Is the tree ahead of the site?**
   `curl -s https://gpncoin.ai/releases.json | grep version` vs `grep -m1 '^version' gpn-agent/Cargo.toml`
   If they differ *and* the tree has commits after the published one → **a release is owed. Stop everything else.**

2. **Is CI green on HEAD?**
   `scripts/release-status.sh`. A red HEAD is fixed first, never released around.

3. **Bump in the same commit as the change.**
   `scripts/bump-version.py patch`, then write CHANGELOG (no `_TODO_`), then `--check`.
   Verified: `lockstep: 15 manifest(s) agree`.

4. **Commit and push once.** Not per fix. A push while CI runs cancels the running verification
   and wastes 40–75 minutes of the Windows job.

5. **Dispatch both builds.**
   `release-linux-selfhosted.yml` and `release-windows.yml` (Windows runs on box 1's runner).

6. **Cut.** `scripts/release-cut.sh --timeout 300`. It mirrors, signs, publishes, and downloads
   the artifacts anonymously to verify them.

7. **Verify the site serves the new version**, not a cached copy: poll `releases.json` twice,
   ten seconds apart.

8. **Watch one node update itself** (not a manual install). This is the only proof the update
   path works: the RUNNING process must report the new version. Record the before/after.

## Where this system stalls — in the order they actually bit us

| # | Stall | Why it happened | Guard |
|---|---|---|---|
| 1 | **Never bumps** | Fixes land, nobody bumps, `site == tree` so every automated comparison says "in sync" | Step 1 is mandatory; a red flag when HEAD has commits newer than the published release |
| 2 | **Many writers** | Queue driver + me both pushed; each push cancelled the other's CI | One writer (maintainer); everyone else proposes |
| 3 | **Release owed but rung 4 wins** | The ladder put "queue work" above "unpublished tree" in practice | Rung 2 must be checked *before* rung 4 on every run, and logged when it fires |
| 4 | **Stuck CI run with 0 jobs** | Queued run that never materialised blocked rung 1 for 20 minutes | Only release-relevant runs block; a run with 0 jobs for >10 min is stuck |
| 5 | **Windows job is 40–75 min** | Cold cache, 300-crate workspace | Persist `CARGO_TARGET_DIR` on box 1; it is the critical path for every release |
| 6 | **Disk full mid-link** | 100 GB of dev `target/` dirs + 36 GB debug cache on one volume | Fail before building under 40 GB free |
| 7 | **Staged but never applied** | Updater wrote `versions/`, launcher ran `bin/` | Launcher execs `versions/current`; update confirms the running version |
| 8 | **Momentum stops at turn end** | Nothing happens between messages unless a cron picks it up | The cron must *act* (rung 2 = run release-cut), not report |

## Rules that came out of the above

- **An unpublished tree outranks every other task.** No queue item, no issue, no investigation.
- **One push per release.** Batch commits; never push while CI is running for HEAD.
- **"Staged" is not "released", and "released" is not "running".** Verify the version from the
  running process, not from a file on disk.
- **If the release cannot be completed, say so immediately** — silence is what let seven fixes
  sit unreleased for a day.
