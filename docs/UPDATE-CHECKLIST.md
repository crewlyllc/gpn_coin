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

## Both platforms, every time

- [ ] **Linux build** — `release-linux-selfhosted.yml` → `gpn-linux.tar.gz` uploaded to the
      `linux-latest` release of the build repo.
- [ ] **Windows build** — `release-windows.yml` on box 1's self-hosted runner → `gpn-windows.zip`
      uploaded to `windows-latest`.
- [ ] **Same SHA on both.** Two builds of different commits is a broken release even if each
      succeeds; check `head_sha` on both workflow runs.
- [ ] **macOS is not required** (billing-blocked) and is not in the manifest. Do not wait for it,
      and do not treat its failure as a release failure.

## Mirrored and published

- [ ] `release-cut` **mirrors both assets** into the public repo `crewlyllc/gpn_coin`, renaming
      them to the versioned filenames the manifest points at.
- [ ] **`website/releases.json` is signed** with the release key (`78015fab…`) and the signature
      verifies — the updater refuses an unsigned manifest, so a bad signature ships nothing.
- [ ] **`network.json`'s `latest_version` matches** the published version (it is `include_str!`d
      into the node; a stale value makes "UPGRADE REQUIRED" unreachable).
- [ ] If the change touched seed lists: `seeds.json`, `network.json`, `install.ps1`,
      `install.sh`, `run-windows.cmd`, the `start-node.cmd` template and the release workflow
      template **all agree** — the parity test enforces this; run it.

## Downloadable — test as a stranger would

- [ ] **Anonymous download of both assets from `gpncoin.ai`** succeeds (no auth, no redirect to a
      login): `curl -fL -o /tmp/x https://gpncoin.ai/dl/<file> && sha256sum /tmp/x`.
- [ ] **The sha256 matches the manifest** for both platforms.
- [ ] **The size matches** the manifest for both platforms.
- [ ] **`releases.json` serves the new version twice**, ten seconds apart (cache check).
- [ ] The **docs stamp** (`/docs/run-a-node`) and `downloads.md` show the same version.
- [ ] Site deploy actually ran — Cloudflare Git integration, not a manual push that skipped it;
      poll the live URL, never the repo file.

## Version consistency, everywhere at once

- [ ] `gpn-agent/Cargo.toml` version
- [ ] the binary's `--version` output and its embedded build stamp
- [ ] `releases.json` `version` and both asset names
- [ ] `network.json` `latest_version`
- [ ] `CHANGELOG.md` section for that version, no `_TODO_`
- [ ] `scripts/bump-version.py --check` → `lockstep: 15 manifest(s) agree`

## Update path (the part users see)

- [ ] `current` pointer written to the new version (a 1-byte empty file has happened before).
- [ ] Launcher execs `versions/current`, **not** a stale `bin\` copy.
- [ ] Update cycle: `check → stop → download → verify → swap → start → CONFIRMED <version> running`.
- [ ] **Rollback retained**: the previous version stays in `versions/` so a failed update can go back.
- [ ] A failed update leaves the node **running the old build**, never down.

## After the fact

- [ ] One node updated itself and reported the new version from the **running process**.
- [ ] The change's own behaviour observed live, with timestamps.
- [ ] Peer count recovered and held >5 minutes; `grep -c "Reason:"` shows no new bans.
- [ ] Task row marked `DONE (date)` with version, commit sha and the evidence line.
