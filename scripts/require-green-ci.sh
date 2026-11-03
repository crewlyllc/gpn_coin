#!/usr/bin/env bash
# Refuse to build a release artifact for a commit whose CI is not green.
#
# Why (2026-11-03): the release workflows dispatched on push regardless of CI
# state. Five consecutive red commits still produced artifacts, and 0.3.12 ended
# up shipped with Linux built from 7bf3c4a and Windows from 0d2c116 -- same
# version number, different code. A version number that does not identify one
# codebase is worse than no version number.
#
# Usage: scripts/require-green-ci.sh [sha] [max_wait_seconds]
#   sha        default: $GITHUB_SHA
#   max_wait   default: 3600 (waits while CI is still running)
#
# Requires: GITHUB_TOKEN (or GH_TOKEN) with actions:read, repo owner/name from
# GITHUB_REPOSITORY (defaults to jeremyzguo/gpn_coin).
set -euo pipefail

sha="${1:-${GITHUB_SHA:-$(git rev-parse HEAD)}}"
max_wait="${2:-3600}"
repo="${GITHUB_REPOSITORY:-jeremyzguo/gpn_coin}"
token="${GITHUB_TOKEN:-${GH_TOKEN:-}}"

if [ -z "$token" ]; then
  echo "::error::require-green-ci: no GITHUB_TOKEN available" >&2
  exit 1
fi

api="https://api.github.com/repos/$repo"
deadline=$(( $(date +%s) + max_wait ))

while :; do
  runs=$(curl -sS -H "Authorization: Bearer $token" -H "Accept: application/vnd.github+json" \
    "$api/actions/runs?head_sha=$sha&per_page=100") || { echo "::error::GitHub API request failed"; exit 1; }

  verdict=$(printf '%s' "$runs" | python3 -c '
import json,sys
d=json.load(sys.stdin)
ci=[r for r in d.get("workflow_runs",[]) if r.get("name")=="CI"]
if not ci:
    print("missing"); raise SystemExit
# The newest CI run for this SHA decides.
ci.sort(key=lambda r: r["created_at"])
latest=ci[-1]
print(latest["status"] + ":" + str(latest.get("conclusion")))
')

  case "$verdict" in
    completed:success)
      echo "CI is green for $sha -- proceeding"
      exit 0 ;;
    completed:*)
      echo "::error::CI for $sha finished as ${verdict#completed:} -- refusing to build a release from a red commit" >&2
      exit 1 ;;
    missing)
      echo "::error::no CI run found for $sha -- refusing to release an unverified commit" >&2
      exit 1 ;;
    *)
      if [ "$(date +%s)" -ge "$deadline" ]; then
        echo "::error::CI for $sha still '${verdict%%:*}' after ${max_wait}s -- giving up" >&2
        exit 1
      fi
      echo "CI for $sha is ${verdict%%:*} -- waiting 30s"
      sleep 30 ;;
  esac
done
