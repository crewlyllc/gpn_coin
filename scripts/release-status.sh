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
# #190: a local HEAD that was never pushed has NO CI run, and the old catch-all
# arm reported that as "in flight: CI none" with exit 1 — so every maintainer
# tick on a diverged checkout started by believing a release was building (four
# consecutive ticks on 2026-11-15). Now `none` is resolved against origin: the
# verdict comes from origin/main's run and is labelled as such. A GitHub API
# read that fails is `unknown` — never success, never "in flight".
#
# Usage: scripts/release-status.sh [--quiet]
set -uo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
quiet=0
[ "${1:-}" = "--quiet" ] && quiet=1
say() { [ "$quiet" -eq 1 ] || printf '%s\n' "$*"; }

tree_ver=$(grep -m1 '^version' "$repo/gpn-agent/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
head_sha=$(git -C "$repo" rev-parse --short HEAD 2>/dev/null)
origin_sha=$(git -C "$repo" rev-parse --short origin/main 2>/dev/null || true)
lock="$HOME/.gpn/release.lock"

site_ver=$(curl -s --max-time 10 "https://gpncoin.ai/releases.json?cb=$(date +%s)" \
           | python3 -c 'import sys,json;print(json.load(sys.stdin).get("version",""))' 2>/dev/null)
# Row 290: the canary proof is what unblocks every non-canary node. A site that
# serves a version the proof does not name means the whole fleet is (silently)
# held — exactly what happened for 0.3.82 and 0.3.83.
canary_ver=$(curl -s --max-time 10 "https://gpncoin.ai/canary.json?cb=$(date +%s)" \
           | python3 -c 'import sys,json;print(json.load(sys.stdin).get("version",""))' 2>/dev/null)

tok_file="${GPN_GITHUB_TOKEN_FILE:-$HOME/.openclaw/credentials/github-gpn-pat.txt}"
runs=""
if [ -f "$tok_file" ]; then
  runs=$(curl -s --max-time 20 -H "Authorization: token $(tr -d '\r\n' < "$tok_file")" \
    "https://api.github.com/repos/jeremyzguo/gpn_coin/actions/runs?per_page=20" 2>/dev/null)
fi

# ci_of <sha> — "status/conclusion" of the CI workflow for that sha.
# `none`   = the API answered and has no CI run for that sha (not pushed / no run).
# `unknown`= the API could not be read at all (no token, network, bad JSON).
ci_of() {
  printf '%s' "$runs" | SHA="$1" python3 -c '
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
' 2>/dev/null || echo unknown
}

# builds_of <sha> — the published-artifact workflows (linux + windows) for that sha.
# Only the workflows whose artifacts the signed manifest actually carries
# (linux + windows) may gate the pipeline. `release-unix.yml` builds macOS,
# which has never been published (releases.json has always been linux +
# windows only), and since 2026-11-06 its GitHub-hosted runner dies during
# provisioning (started+completed in 3 s, no steps, no log blob) because
# hosted-runner minutes on this account are billing-blocked. Counting that
# red made every HEAD with a build tag read "PROBLEM" forever, which is the
# one shape this script exists to prevent: a check that can never go green.
# It is reported separately as `other` instead.
builds_of() {
  printf '%s' "$runs" | SHA="$1" python3 -c '
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
' 2>/dev/null || echo unknown
}

ci=$(ci_of "${head_sha:-none}")
builds=$(builds_of "${head_sha:-none}")
resolved_sha="$head_sha"
origin_note=""

# A local HEAD that is not on origin has no run of its own. Resolve the verdict
# from origin/main's run and SAY SO, instead of calling an absent run "in flight".
if [ "$ci" = "none" ] && [ -n "$origin_sha" ] && [ "${head_sha:-}" != "$origin_sha" ]; then
  origin_ci=$(ci_of "$origin_sha")
  if [ "$origin_ci" != "none" ]; then
    ci="$origin_ci"
    builds=$(builds_of "$origin_sha")
    resolved_sha="$origin_sha"
    origin_note="local HEAD ${head_sha:-?} has no CI run (not on origin); origin/main $origin_sha is $origin_ci"
  fi
fi

say "tree      v$tree_ver @ ${head_sha:-unknown}"
say "site      ${site_ver:-unreachable}"
say "canary    ${canary_ver:-unreachable}"
say "ci        $ci"
printf 'builds    %s\n' "$(printf '%s\n' "$builds" | head -1)"
case "$builds" in *$'\n'*) say "$(printf '%s\n' "$builds" | tail -n +2)" ;; esac
say "lock      $([ -f "$lock" ] && cat "$lock" || echo absent)"
[ -n "$origin_note" ] && say "note      $origin_note"

rc=0
case "$ci" in
  success) : ;;
  *"/success") : ;;
  *"/failure"|*"/cancelled"|*"/timed_out")
    say "PROBLEM: CI is $ci for $resolved_sha — no release can be cut"; rc=2 ;;
  *"/null"|*"/None"|*"/queued"|*"/in_progress"|*"/requested"|*"/pending"|*"/waiting")
    say "in flight: CI $ci for $resolved_sha"; [ $rc -eq 0 ] && rc=1 ;;
  none)
    say "UNKNOWN: no CI run for HEAD ${head_sha:-?}${origin_sha:+ or origin/main $origin_sha} — nothing to read a verdict from"; [ $rc -eq 0 ] && rc=1 ;;
  unknown|"")
    say "UNKNOWN: the GitHub API could not be read — cannot certify whether a release is in flight"; [ $rc -eq 0 ] && rc=1 ;;
  *)
    say "UNKNOWN: unrecognised CI status '$ci' for $resolved_sha"; [ $rc -eq 0 ] && rc=1 ;;
esac

case "$(printf '%s\n' "$builds" | head -1)" in
  *"/failure"*) say "PROBLEM: a build workflow failed for $resolved_sha ($(printf '%s\n' "$builds" | head -1))"; rc=2 ;;
  *"/in_progress"*|*"/queued"*) say "in flight: builds $(printf '%s\n' "$builds" | head -1)"; [ $rc -eq 0 ] && rc=1 ;;
esac

if [ -n "$site_ver" ] && [ "$site_ver" != "$tree_ver" ]; then
  say "PROBLEM: site serves $site_ver but the tree is $tree_ver (release not published)"
  rc=2
fi

if [ -n "$site_ver" ] && [ "$canary_ver" != "$site_ver" ]; then
  say "PROBLEM: site serves $site_ver but canary.json proves ${canary_ver:-nothing} — the fleet is HELD on ${canary_ver:-an unproven release} (row 290: scripts/canary-auto.sh publishes it)"
  rc=2
fi

[ -f "$lock" ] && { say "in flight: release lock present"; [ $rc -eq 0 ] && rc=1; }

case $rc in
  0) say "OK — nothing stuck" ;;
  1) say "NOT CERTIFIED — a release may be building, or the state could not be read (see above)" ;;
  2) say "ATTENTION — the release pipeline needs a fix before anything else" ;;
esac
exit $rc
