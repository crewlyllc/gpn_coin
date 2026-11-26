#!/usr/bin/env bash
# test-canary-publish.sh — the publishing half of the canary gate (row 275).
#
# What must be true:
#   * a report that is not green (missing / not confirmed / no job served) is
#     REFUSED, exits 2, and writes nothing — a half-proven release must never
#     unblock the fleet;
#   * a green report is copied to website/canary.json byte-for-byte;
#   * publishing twice is a no-op, not a second commit.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/../canary-publish.sh"
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); echo "  ok   — $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL — $1"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

git init -q "$TMP/repo"
git -C "$TMP/repo" config user.name t
git -C "$TMP/repo" config user.email t@t
mkdir -p "$TMP/repo/website"
echo '{}' > "$TMP/repo/website/canary.json"
git -C "$TMP/repo" add -A
git -C "$TMP/repo" commit -qm init
cp "$TMP/repo/website/canary.json" "$TMP/canary.before"

run() {  # run <report-path> [extra args...]
    local rep="$1"; shift
    bash "$SCRIPT" --root "$TMP/repo" --report "$rep" --no-push "$@" 2>&1
}

echo "== a report that does not exist is refused"
set +e; out="$(run "$TMP/missing.json")"; rc=$?; set -e
[ "$rc" -eq 2 ] && ok "exit 2 with no report" || bad "expected exit 2, got $rc: $out"
echo "$out" | grep -q "has not confirmed" && ok "says the canary has not confirmed" || bad "no explanation: $out"

echo "== ok:true but no job served is refused"
cat > "$TMP/njob.json" <<'JSON'
{"version":"0.3.80","ok":true,"job_served":false,"confirmed_at":1700000000}
JSON
set +e; out="$(run "$TMP/njob.json")"; rc=$?; set -e
[ "$rc" -eq 2 ] && ok "exit 2 when no job has been served" || bad "expected exit 2, got $rc: $out"
echo "$out" | grep -q "served no job" && ok "names the missing job" || bad "no job named: $out"
cmp -s "$TMP/canary.before" "$TMP/repo/website/canary.json" && ok "the published file was left untouched" || bad "the published file changed"
echo "== a failed canary is refused, quoting its reason"
cat > "$TMP/failed.json" <<'JSON'
{"version":"0.3.80","ok":false,"job_served":false,"failed_at":1700000500,"reason":"confirm: process=down rpc=no 10s"}
JSON
set +e; out="$(run "$TMP/failed.json")"; rc=$?; set -e
[ "$rc" -eq 2 ] && ok "exit 2 for a failed canary" || bad "expected exit 2, got $rc: $out"
echo "$out" | grep -q "process=down" && ok "quotes the canary's failure" || bad "reason not quoted: $out"

echo "== garbage is refused, not published"
echo 'not json' > "$TMP/junk.json"
set +e; out="$(run "$TMP/junk.json")"; rc=$?; set -e
[ "$rc" -eq 2 ] && ok "exit 2 for unreadable JSON" || bad "expected exit 2, got $rc: $out"

echo "== a green report is published, byte for byte, in one commit"
cat > "$TMP/green.json" <<'JSON'
{"version":"0.3.80","ok":true,"job_served":true,"confirmed_at":1700000123,"failed_at":0,"reason":""}
JSON
set +e; out="$(run "$TMP/green.json")"; rc=$?; set -e
[ "$rc" -eq 0 ] && ok "exit 0 for a green report" || bad "expected exit 0, got $rc: $out"
cmp -s "$TMP/green.json" "$TMP/repo/website/canary.json" && ok "published file is the report" || bad "published file differs"
[ "$(git -C "$TMP/repo" rev-list --count HEAD)" -eq 2 ] && ok "exactly one new commit" || bad "unexpected commit count"
git -C "$TMP/repo" log -1 --format=%s | grep -q "canary: 0.3.80 proven" && ok "commit message names the version" || bad "commit message wrong"

echo "== publishing the same proof twice is a no-op"
set +e; out="$(run "$TMP/green.json")"; rc=$?; set -e
[ "$rc" -eq 0 ] && ok "exit 0" || bad "expected exit 0, got $rc: $out"
echo "$out" | grep -q "already published" && ok "says it is already published" || bad "no idempotence message: $out"
[ "$(git -C "$TMP/repo" rev-list --count HEAD)" -eq 2 ] && ok "no second commit" || bad "commit count moved"

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
