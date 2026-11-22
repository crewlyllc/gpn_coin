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

