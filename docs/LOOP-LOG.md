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

