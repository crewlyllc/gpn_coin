#!/usr/bin/env bash
# release-status.sh — one screen that answers "is anything shipping, and is it stuck?"
#
# Why: the queue driver reports "0 takeable" from the task table alone, which
# reads as "nothing to do". On 2026-11-07 it said exactly that while 0.3.29 was
# blocked on a red CI run for two hours — the task table cannot see the release
# pipeline. This script is the missing input: run it FIRST, and a red/drifted
# release outranks any queued task.
#
# Exit codes:
#   0  consistent — site serves the tree's version, CI green for HEAD, no lock
#   1  in flight   — a release is building/signing (lock present or CI running)
#   2  PROBLEM     — CI red for HEAD, or the site lags the tree, or a build failed
#
# Usage: scripts/release-status.sh [--quiet]
set -uo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
quiet=0
[ "${1:-}" = "--quiet" ] && quiet=1
say() { [ "$quiet" -eq 1 ] || printf '%s\n' "$*"; }

tree_ver=$(grep -m1 '^version' "$repo/gpn-agent/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
head_sha=$(git -C "$repo" rev-parse --short HEAD)
lock="$HOME/.gpn/release.lock"

site_ver=$(curl -s --max-time 10 "https://gpncoin.ai/releases.json?cb=$(date +%s)" \
           | python3 -c 'import sys,json;print(json.load(sys.stdin).get("version",""))' 2>/dev/null)

tok_file="${GPN_GITHUB_TOKEN_FILE:-$HOME/.openclaw/credentials/github-gpn-pat.txt}"
ci="unknown"; builds="unknown"
if [ -f "$tok_file" ]; then
  runs=$(curl -s --max-time 20 -H "Authorization: token $(tr -d '\r\n' < "$tok_file")" \
    "https://api.github.com/repos/jeremyzguo/gpn_coin/actions/runs?per_page=20" 2>/dev/null)
  ci=$(printf '%s' "$runs" | SHA="$head_sha" python3 -c '
import os, sys, json
sha = os.environ["SHA"]
try:
    runs = json.load(sys.stdin)["workflow_runs"]
except Exception:
    print("unknown"); raise SystemExit
for r in runs:
    if r.get("name") == "CI" and r.get("head_sha", "").startswith(sha):
        print(str(r.get("status")) + "/" + str(r.get("conclusion"))); break
else:
    print("none")
' 2>/dev/null || echo unknown)
  # Only the workflows whose artifacts the signed manifest actually carries
  # (linux + windows) may gate the pipeline. `release-unix.yml` builds macOS,
  # which has never been published (releases.json has always been linux +
  # windows only), and since 2026-11-06 its GitHub-hosted runner dies during
  # provisioning (started+completed in 3 s, no steps, no log blob) because
  # hosted-runner minutes on this account are billing-blocked. Counting that
  # red made every HEAD with a build tag read "PROBLEM" forever, which is the
  # one shape this script exists to prevent: a check that can never go green.
  # It is reported separately as `other` instead.
  builds=$(printf '%s' "$runs" | SHA="$head_sha" python3 -c '
import os, sys, json
sha = os.environ["SHA"]
try:
    runs = json.load(sys.stdin)["workflow_runs"]
except Exception:
    print("unknown"); raise SystemExit
released, other = [], []
for r in runs:
    name = (r.get("name") or "")
    if not r.get("head_sha", "").startswith(sha) or "binaries" not in name.lower():
        continue
    tag = name.split()[0].lower() + "=" + str(r.get("status")) + "/" + str(r.get("conclusion"))
    (other if "unix" in name.lower() else released).append(tag)
print(" ".join(sorted(set(released))) or "none")
if other:
    print("other    " + " ".join(sorted(set(other))))
' 2>/dev/null || echo unknown)
fi

say "tree      v$tree_ver @ $head_sha"
say "site      ${site_ver:-unreachable}"
say "ci        $ci"
printf 'builds    %s\n' "$(printf '%s\n' "$builds" | head -1)"
case "$builds" in *$'\n'*) say "$(printf '%s\n' "$builds" | tail -n +2)" ;; esac
say "lock      $([ -f "$lock" ] && cat "$lock" || echo absent)"

rc=0
case "$ci" in
  *"/failure"|*"/cancelled"|*"/timed_out") say "PROBLEM: CI is $ci for HEAD — no release can be cut"; rc=2 ;;
  *"/null"|*"/None") say "in flight: CI $ci"; [ $rc -eq 0 ] && rc=1 ;;
  *"/success") : ;;
  *) say "in flight: CI $ci"; [ $rc -eq 0 ] && rc=1 ;;
esac

case "$(printf '%s\n' "$builds" | head -1)" in
  *"/failure"*) say "PROBLEM: a build workflow failed for HEAD ($(printf '%s\n' "$builds" | head -1))"; rc=2 ;;
  *"/in_progress"*|*"/queued"*) say "in flight: builds $(printf '%s\n' "$builds" | head -1)"; [ $rc -eq 0 ] && rc=1 ;;
esac

if [ -n "$site_ver" ] && [ "$site_ver" != "$tree_ver" ]; then
  say "PROBLEM: site serves $site_ver but the tree is $tree_ver (release not published)"
  rc=2
fi

[ -f "$lock" ] && { say "in flight: release lock present"; [ $rc -eq 0 ] && rc=1; }

case $rc in
  0) say "OK — nothing stuck" ;;
  1) say "IN FLIGHT — a release is being built or signed" ;;
  2) say "ATTENTION — the release pipeline needs a fix before anything else" ;;
esac
exit $rc
