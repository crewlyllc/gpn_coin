# Update checklist — every release, in this order

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

## Must never regress

- Private/LAN addresses in shipped defaults (0.3.29 broke every remote box)
- Loopback in advertised listen addresses (caused box 1's self-dial storm)
- Launcher execing a stale `bin\` copy instead of `versions/current`
- Env flags that silently disable a feature (`GPN_SITE_ELECT`, `GPN_SELF_HEAL`)
- Mechanisms with no caller — grep for a call site, not a definition
- A version that moves in `Cargo.toml` but not in `releases.json`, or vice versa
