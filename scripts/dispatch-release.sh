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
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
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
api="https://api.github.com/repos/$repo/actions/workflows"

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
