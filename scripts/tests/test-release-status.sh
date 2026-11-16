#!/usr/bin/env bash
# test-release-status.sh — self-test for scripts/release-status.sh (#190)
#
# The script decides in one shot whether a release is in flight, so its
# interpretation of "no CI run for HEAD" is load-bearing: the old catch-all
# treated `ci none` as "in flight" (exit 1) and four consecutive maintainer
# ticks started by believing a release was building on a diverged checkout.
#
# Hermetic: a throwaway repo dir (the script derives its root from $0), a fake
# `git` for the two rev-parses, a fake `curl` for the site + API payloads, and
# $HOME pointed at a temp dir so no real lock is consulted. No network.
#
# Cases:
#   success              completed/success for HEAD on origin        -> 0, OK
#   in_progress          in_progress/None for HEAD                   -> 1, in flight
#   none-origin-green    HEAD not on origin, origin CI success       -> 0, note names both
#   none-origin-red      HEAD not on origin, origin CI failure       -> 2, PROBLEM
#   api-down             API unreadable                              -> 1, UNKNOWN, never OK
#   none-no-origin       HEAD == origin, no CI run at all            -> 1, UNKNOWN
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/../release-status.sh"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   %s\n' "$*"; }
bad() { fail=$((fail+1)); printf '  FAIL %s\n' "$*"; }
check() { # check <desc> <condition-as-string 0/1>
  if [ "$2" = 0 ]; then ok "$1"; else bad "$1"; fi
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

R="$TMP/repo"; mkdir -p "$R/scripts" "$R/gpn-agent" "$TMP/bin" "$TMP/home"
cp "$SCRIPT" "$R/scripts/release-status.sh"
printf '[package]\nname = "gpn-agent"\nversion = "0.3.49"\n' > "$R/gpn-agent/Cargo.toml"
printf 'token\n' > "$TMP/token"

# fake git: only the two calls the script makes
cat > "$TMP/bin/git" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *"rev-parse --short HEAD"*)         printf '%s\n' "${FAKE_HEAD:-abc1234}" ;;
  *"rev-parse --short origin/main"*)  printf '%s\n' "${FAKE_ORIGIN:-abc1234}" ;;
  *) exit 1 ;;
esac
EOF

# fake curl: site JSON or the runs payload, or a hard failure
cat > "$TMP/bin/curl" <<'EOF'
#!/usr/bin/env bash
url=""
for a in "$@"; do case "$a" in http*) url="$a" ;; esac; done
case "$url" in
  *"gpncoin.ai"*) printf '{"version":"%s"}\n' "${FAKE_SITE_VERSION:-0.3.49}" ;;
  *"api.github.com"*)
    if [ "${FAKE_API_DOWN:-0}" = 1 ]; then exit 6; fi
    cat "${FAKE_RUNS:-/dev/null}" ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$TMP/bin/git" "$TMP/bin/curl"

runs_json() { # runs_json <file> <ci sha/status/conclusion> [build sha/...]
  local f="$1"; shift
  python3 - "$f" "$@" <<'PY'
import json, sys
out = sys.argv[1]; runs = []
for spec in sys.argv[2:]:
    sha, status, concl = spec.split("/")
    runs.append({"name": "CI", "head_sha": sha, "status": status, "conclusion": concl})
open(out, "w").write(json.dumps({"workflow_runs": runs}))
PY
}

run_case() { # run_case <name> <expected_rc> ; env already set
  out=$(PATH="$TMP/bin:$PATH" HOME="$TMP/home" GPN_GITHUB_TOKEN_FILE="$TMP/token" \
        bash "$R/scripts/release-status.sh" 2>&1); rc=$?
  printf '%s\n' "--- $1 (rc=$rc)" >&2
}

# 1. success on HEAD
runs_json "$TMP/r1.json" "abc1234/completed/success"
export FAKE_RUNS="$TMP/r1.json" FAKE_API_DOWN=0 FAKE_HEAD=abc1234 FAKE_ORIGIN=abc1234
run_case success 0
check "success -> exit 0" "$([ "$rc" = 0 ] && echo 0 || echo 1)"
check "success -> says OK" "$(contains "$out" 'OK' && echo 0 || echo 1)"
check "success -> no false 'in flight'" "$(contains "$out" 'in flight' && echo 1 || echo 0)"

# 2. CI still running for HEAD
runs_json "$TMP/r2.json" "abc1234/in_progress/None"
export FAKE_RUNS="$TMP/r2.json"
run_case in_progress 1
check "in_progress -> exit 1" "$([ "$rc" = 1 ] && echo 0 || echo 1)"
check "in_progress -> says in flight" "$(contains "$out" 'in flight' && echo 0 || echo 1)"

# 3. HEAD not on origin, origin/main green — the #190 regression
runs_json "$TMP/r3.json" "9999999/completed/success"
export FAKE_RUNS="$TMP/r3.json" FAKE_HEAD=def5678 FAKE_ORIGIN=9999999
run_case none-origin-green 0
check "none-origin-green -> exit 0 (was 1)" "$([ "$rc" = 0 ] && echo 0 || echo 1)"
check "none-origin-green -> names local HEAD" "$(contains "$out" 'def5678' && echo 0 || echo 1)"
check "none-origin-green -> names origin/main" "$(contains "$out" 'origin/main 9999999' && echo 0 || echo 1)"
check "none-origin-green -> not reported as in flight" "$(contains "$out" 'in flight' && echo 1 || echo 0)"

# 4. HEAD not on origin, origin/main red
runs_json "$TMP/r4.json" "9999999/completed/failure"
export FAKE_RUNS="$TMP/r4.json"
run_case none-origin-red 2
check "none-origin-red -> exit 2" "$([ "$rc" = 2 ] && echo 0 || echo 1)"
check "none-origin-red -> says PROBLEM" "$(contains "$out" 'PROBLEM' && echo 0 || echo 1)"

# 5. API unreadable — unknown, never success
export FAKE_API_DOWN=1
run_case api-down 1
check "api-down -> exit 1" "$([ "$rc" = 1 ] && echo 0 || echo 1)"
check "api-down -> says UNKNOWN" "$(contains "$out" 'UNKNOWN' && echo 0 || echo 1)"
check "api-down -> never says OK" "$(contains "$out" 'OK —' && echo 1 || echo 0)"
export FAKE_API_DOWN=0

# 6. HEAD == origin and no CI run at all
runs_json "$TMP/r6.json"
export FAKE_RUNS="$TMP/r6.json" FAKE_HEAD=abc1234 FAKE_ORIGIN=abc1234
run_case none-no-origin 1
check "no run anywhere -> exit 1 UNKNOWN" "$([ "$rc" = 1 ] && contains "$out" 'UNKNOWN' && echo 0 || echo 1)"

# 7. release lock present forces exit 1 even on a green tree
runs_json "$TMP/r7.json" "abc1234/completed/success"
export FAKE_RUNS="$TMP/r7.json"; mkdir -p "$TMP/home/.gpn"; printf 'pid 1\n' > "$TMP/home/.gpn/release.lock"
run_case lock-present 1
check "lock present -> exit 1" "$([ "$rc" = 1 ] && echo 0 || echo 1)"
check "lock present -> says lock" "$(contains "$out" 'release lock present' && echo 0 || echo 1)"
rm -f "$TMP/home/.gpn/release.lock"

printf '\nPASS=%d FAIL=%d\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
