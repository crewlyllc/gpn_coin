# Update checklist — every release, in order

Nothing here is optional. Each line exists because skipping it caused a specific failure this
month; the failure is named so the line reads as a reason, not a ritual.

## Before the build

- [ ] **The tree contains the change.** `git log --oneline -3 -- <file>` and confirm the file is
      actually there. *Failure it prevents: `fork_choice.rs` released in 0.3.50 while the shipped
      binaries contained none of its strings.*
- [ ] **Bump in the same commit as the change.** `scripts/bump-version.py patch` then write
      CHANGELOG (no `_TODO_`) then `--check` → `lockstep: 15 manifest(s) agree`.
      *A fix without a bump is invisible to every version comparison.*
- [ ] **The changelog says what a user would notice**, not what files changed.
- [ ] **Tests exist and fail without the change.** Paste the before/after output in the row.

## Push

- [ ] **One push.** Never push while CI is running for HEAD — it cancels the run and wastes 40–75
      minutes of the Windows job.
- [ ] **CI green on that exact SHA:** fmt, clippy `-D warnings`, secret-scan, Linux build,
      **Windows build on box 1's runner**.
- [ ] If CI is red, fix the cause; do not release around it.

## Build

- [ ] Dispatch **both**: `release-linux-selfhosted.yml` and `release-windows.yml`.
- [ ] Confirm both workflows report `success` **for the same SHA you pushed**.

## Verify the artifact — before it goes anywhere

- [ ] **Scan the built binary for the strings the change added.** PowerShell:
      `$s=[IO.File]::ReadAllText($p); [regex]::Matches($s,'reorg=').Count`
      Use literals that appear in **code**, never in comments — `reorg=`, `work ours=`,
      `gpn-forked`, not the constant or module name.
      *This check is the one that caught a release shipping a tree without the code.*
- [ ] Confirm the version the binary reports matches the manifest.

## Cut

- [ ] `scripts/release-cut.sh --timeout 300` — mirrors, signs, publishes, and re-downloads both
      assets anonymously to verify.
- [ ] Poll `releases.json` **twice** for the new version (defeats cache).

## Deploy and confirm

- [ ] Let **one** node update itself; do not copy files by hand.
- [ ] The **running process** reports the new version:
      `(Get-Process gpn-node).Path` → the versioned path, then `--version` from that path.
- [ ] Its log shows `check → apply → copied N binaries → CONFIRMED <version> is running`.
- [ ] If the update fails, it **rolls back and stays up**; a down node is a failed release.

## Prove it works

- [ ] The observable behaviour of the change, on a real machine, pasted with timestamps —
      not "should now work".
- [ ] Peers stay connected for >5 minutes after the deploy.
- [ ] `grep -c "Reason:"` over the deploy window shows no new bans.
- [ ] No new `gpn-isolated` / `gpn-forked` markers where there were none.

## Record

- [ ] Mark the task row `DONE (date)` with the version string, the commit sha, and the log line.
- [ ] If something could not be tested, say so in the row and tell Jeremy — before calling it done.

## Things that must never regress

- Private/LAN addresses in shipped defaults (0.3.29 broke every remote box)
- Loopback advertised in listen addresses (caused box 1's self-dial storm)
- The launcher execing a stale `bin\` copy instead of `versions/current`
- Unset env flags that silently disable a feature (`GPN_SITE_ELECT`, `GPN_SELF_HEAL`)
- A mechanism with no caller (heal, election) — grep for a call site, not just a definition
