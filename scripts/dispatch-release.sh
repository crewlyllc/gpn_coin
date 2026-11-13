#!/usr/bin/env bash
# dispatch-release.sh — build a release from an EXPLICIT commit, not from `main`.
#
# Why (2026-11-06): the queue driver commits to `main` every 30 minutes. A
# `workflow_dispatch` with `ref: main` therefore builds "whatever main is when a
# runner picks the job up", which is how last night's release ended up chasing a
# moving tree (and why I had to pause the driver by hand — a human workaround for
# a mechanical problem).
#
# Pin the commit instead. Then the driver can keep working while a release is in
# flight: the artifact is bound to a SHA, and `release-cut` already refuses to
# publish a pair whose bundle stamps disagree.
#
# Usage:
#   scripts/dispatch-release.sh              # current HEAD
#   scripts/dispatch-release.sh <sha>        # a specific commit
#   scripts/dispatch-release.sh --force <sha>  # ignore an in-flight cut/build
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

# --force is the documented escape hatch for the in-flight guard below; keep it
# out of the positional-argument slot so `dispatch-release.sh --force` still
# means "dispatch HEAD".
FORCE="${GPN_RELEASE_FORCE:-0}"
if [ "${1:-}" = "--force" ]; then FORCE=1; shift; fi
sha="${1:-$(git -C "$repo_root" rev-parse HEAD)}"

# Must be a full 40-hex sha: `ref` accepts branches, tags and SHAs, and a typo
# would silently build the wrong thing.
if ! printf '%s' "$sha" | grep -Eq '^[0-9a-f]{40}$'; then
  echo "REFUSED: '$sha' is not a full commit sha" >&2
  exit 2
fi

tok_file="${GPN_GITHUB_TOKEN_FILE:-$HOME/.openclaw/credentials/github-gpn-pat.txt}"
[ -f "$tok_file" ] || { echo "REFUSED: no token at $tok_file" >&2; exit 2; }
tok="$(tr -d '\r\n' < "$tok_file")"

repo="${GPN_REPO:-jeremyzguo/gpn_coin}"
# Same override release-cut.sh takes, so the guard below can be exercised against
# a fake API instead of the real one.
GPN_GH_API="${GPN_GH_API:-https://api.github.com}"
api="$GPN_GH_API/repos/$repo/actions/workflows"

# ── one release at a time ─────────────────────────────────────────────────
# W145 (2026-11-12): the flock in release-cut.sh only protects the *signing*
# step, and release.lock does not exist until a cut has actually started — so
# two dispatchers could each read "no builds running", both dispatch, and queue
# ~40 minutes of duplicate runner time for the same tag (that is what W142's
# stand-down only avoided by luck). Three independent checks, all of them
# strictly before the first `workflow_dispatch` call:
#
#   1. the flock itself — race-free, and a cut holds it from `acquire_release_lock`
#      onwards even when the JSON lock has not been rewritten yet;
#   2. a release-cut process on this host — catches a cut started by a wrapper
#      that has not reached the lock yet;
#   3. an in-flight build run for THIS commit on GitHub — covers the case (1) and
#      (2) cannot see, a dispatch already made from another session or machine.
#
# A clean dispatch — no cut, no build — behaves exactly as before.
refuse() { echo "REFUSED: $*" >&2; exit 3; }

RELEASE_LOCK="${GPN_RELEASE_LOCK:-$HOME/.gpn/release.lock}"

inflight_builds_for() { # sha -> "file:status ..." (empty = none)
  curl -s --max-time 20 -H "Authorization: token $tok" \
    "$GPN_GH_API/repos/$repo/actions/runs?per_page=40" \
    | SHA="$1" python3 -c '
import os, sys, json
sha = os.environ["SHA"]
want = {"release-linux-selfhosted.yml", "release-windows.yml"}
try:
    runs = json.load(sys.stdin).get("workflow_runs", [])
except Exception:
    sys.exit(1)   # unreadable: caller decides, do not invent "none"
hits = []
for r in runs:
    if not (r.get("head_sha") or "").startswith(sha):
        continue
    path = (r.get("path") or "").rsplit("/", 1)[-1]
    if path not in want:
        continue
    if r.get("status") in ("queued", "in_progress"):
        hits.append("%s:%s" % (path, r["status"]))
print(" ".join(hits))
' 2>/dev/null
}

release_in_flight_guard() {
  if [ "$FORCE" = "1" ]; then
    echo "   ! --force: skipping the in-flight guard for $sha"
    return 0
  fi

  if command -v flock >/dev/null 2>&1; then
    mkdir -p "$(dirname "$RELEASE_LOCK")"
    # fd 8, deliberately not fd 9: release-cut owns 9 for its own lock.
    if ! exec 8>"$RELEASE_LOCK.flock"; then
      refuse "cannot open $RELEASE_LOCK.flock"
    fi
    if ! flock -n 8; then
      exec 8>&-
      refuse "another release-cut holds $RELEASE_LOCK.flock — wait for it to finish (or --force)"
    fi
    exec 8>&-
  fi

  # Bracket the dot so pgrep cannot match its own command line (TOOLS.md).
  cut_pids="$(pgrep -f 'release-cut[.]sh' 2>/dev/null | grep -vx "$$" | tr '\n' ' ' || true)"
  if [ -n "$cut_pids" ]; then
    refuse "release-cut.sh is already running (pid ${cut_pids% }) — wait for it to finish (or --force)"
  fi

  if builds="$(inflight_builds_for "$sha")"; then
    if [ -n "$builds" ]; then
      refuse "release builds are already running for $sha ($builds) — wait, do not queue duplicates (or --force)"
    fi
  else
    echo "   ! could not read run history for $sha — proceeding (GitHub will reject a duplicate)" >&2
  fi
}

release_in_flight_guard

# Linux lives in its own self-hosted workflow; release-unix.yml builds macOS
# only. Discovering that cost a release cycle on 2026-11-05 — keep them named.
workflows=(
  "release-linux-selfhosted.yml"
  "release-unix.yml"
  "release-windows.yml"
)

# GitHub's `workflow_dispatch` accepts a BRANCH or TAG, not a raw sha
# ("No ref found for: <sha>"). A lightweight tag gives the same guarantee --
# immutable, traceable, and it survives `main` moving under it. The `build/`
# prefix deliberately does not match the `v*` tag trigger, so this does not
# start a second round of builds.
tag="build/${sha:0:12}"
if ! git -C "$repo_root" rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  git -C "$repo_root" tag "$tag" "$sha" || { echo "REFUSED: cannot tag $sha" >&2; exit 2; }
fi
if ! git -C "$repo_root" ls-remote --exit-code --tags origin "$tag" >/dev/null 2>&1; then
  echo "== pushing tag $tag -> $sha"
  # `origin` alone authenticates as whichever account ~/.git-credentials lists
  # first (crewlyllc), which returns "Repository not found" for this private
  # repo. Push through the jeremyzguo credential line explicitly.
  cred="$(grep -m1 'jeremyzguo:' "$HOME/.git-credentials" 2>/dev/null | sed -e 's#^https://##' -e 's#/$##')"
  if [ -n "$cred" ]; then
    push_url="https://${cred}/jeremyzguo/gpn_coin.git"
  else
    push_url="$(git -C "$repo_root" config --get remote.origin.url)"
  fi
  git -C "$repo_root" push -q "$push_url" "refs/tags/$tag" || { echo "REFUSED: cannot push $tag" >&2; exit 2; }
fi

echo "== dispatch release builds for $sha (ref: $tag)"
rc=0
for wf in "${workflows[@]}"; do
  code="$(curl -s -o /tmp/dispatch-err.$$ -w '%{http_code}' -X POST \
    -H "Authorization: token $tok" \
    -H 'Accept: application/vnd.github+json' \
    "$api/$wf/dispatches" \
    -d "{\"ref\":\"$tag\"}")"
  if [ "$code" = "204" ]; then
    echo "   $wf -> dispatched"
  else
    # 422 here usually means "a run for this workflow is already in progress".
    echo "   $wf -> FAILED (HTTP $code): $(head -c 200 /tmp/dispatch-err.$$)"
    rc=1
  fi
done
rm -f /tmp/dispatch-err.$$

if [ "$rc" -eq 0 ]; then
  echo
  echo "next: wait for all three to finish on $sha, then scripts/release-cut.sh"
  echo "      (release-cut verifies CI is green for the same sha before signing)"
fi
exit "$rc"
