#!/usr/bin/env bash
# auto-release-if-behind.sh — close the "done but never published" gap.
#
# Why (2026-11-09): #154 and #155 were implemented and committed, and then sat in
# `main` at the *previous* version for hours. Publishing needs two manual steps —
# `bump-version.py` and `dispatch-release.sh` — and neither fires by itself, so
# finished work looked to the owner exactly like no work happening. That happened
# three times in one day.
#
# This script makes publishing self-driving. Run it every 30 min:
#
#   1. tree version > site version?
#   2. CI green for HEAD?
#   3. no release in flight (lock) and no build already running for HEAD?
#   → dispatch the three build workflows against a tag pinned to HEAD
#
# It never bumps the version (that is a human decision about what a release is);
# it only ships what is already cut but unpublished.
set -uo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
lock="$HOME/.gpn/release.lock"
tok_file="${GPN_GITHUB_TOKEN_FILE:-$HOME/.openclaw/credentials/github-gpn-pat.txt}"

say() { echo "[auto-release] $*"; }

[ -f "$tok_file" ] || { say "no token at $tok_file — nothing to do"; exit 0; }
[ -f "$lock" ] && { say "release lock present — a cut is in flight"; exit 0; }

tree=$(grep -m1 '^version' "$repo/gpn-agent/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
head_sha=$(git -C "$repo" rev-parse HEAD)

site=$(curl -s --max-time 10 "https://gpncoin.ai/releases.json?cb=$(date +%s)" \
       | python3 -c 'import sys,json;print(json.load(sys.stdin).get("version",""))' 2>/dev/null)
[ -n "$site" ] || { say "site manifest unreachable — will retry next tick"; exit 0; }

# version compare: only act when the tree is strictly ahead
if ! python3 - "$tree" "$site" <<'PY'
import sys
def t(v): return tuple(int(x) for x in v.split(".") if x.isdigit())
sys.exit(0 if t(sys.argv[1]) > t(sys.argv[2]) else 1)
PY
then
  say "in sync (tree $tree, site $site)"
  exit 0
fi

api() { curl -s --max-time 20 -H "Authorization: token $(tr -d '\r\n' < "$tok_file")" "https://api.github.com/repos/jeremyzguo/gpn_coin/$1"; }

runs=$(api "actions/runs?per_page=30")
[ -n "$runs" ] || { say "GitHub API unreachable"; exit 0; }

read -r ci_state builds_state <<<"$(printf '%s' "$runs" | SHA="$head_sha" python3 -c '
import os, sys, json
sha = os.environ["SHA"]
runs = json.load(sys.stdin)["workflow_runs"]
ci = "none"; b = []
for r in runs:
    if not r.get("head_sha","").startswith(sha): continue
    name = (r.get("name") or "")
    st = str(r.get("status")) + "/" + str(r.get("conclusion"))
    if name == "CI": ci = st
    elif "binaries" in name.lower(): b.append(st)
print(ci, ",".join(sorted(set(b))) if b else "none")
')"

say "tree $tree > site $site · ci=$ci_state · builds=$builds_state"

case "$ci_state" in
  *"/failure"*|*"/cancelled"*|*"/timed_out"*) say "CI is red for HEAD — not releasing"; exit 2 ;;
  *"/success") : ;;
  *) say "CI not green yet ($ci_state) — waiting"; exit 0 ;;
esac

case "$builds_state" in
  none)
    say "no builds for HEAD — dispatching release builds"
    "$repo/scripts/dispatch-release.sh" "$head_sha" 2>&1 | tail -4
    ;;
  *"/failure"*) say "a build failed for HEAD — not releasing (needs a human)"; exit 2 ;;
  *) say "builds in progress ($builds_state) — waiting"; exit 0 ;;
esac
