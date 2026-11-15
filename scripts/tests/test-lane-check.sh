#!/usr/bin/env bash
# test-lane-check.sh — W172 guard: the lane check must not count a timestamp as
# a fault, and must still count a real one.
#
# Each case is a whole journal fed through `--file`, so the assertions are about
# behaviour, not about the pattern looking right.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
script="$here/../lane-check.sh"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
fail=0
ok()   { printf 'ok   %s\n' "$1"; }
bad()  { printf 'FAIL %s\n' "$1"; fail=$((fail+1)); }

run() { bash "$script" --file "$1" >"$tmp/out" 2>&1; echo $?; }

# 1. A millisecond timestamp ending in .503 is NOT a fault (the exact live shape).
cat >"$tmp/t1" <<'EOF'
Nov 14 23:16:34 host gpn_fleet[1]: [block] sealed #304150 at 23:16:34.503 hash=0xabc
Nov 14 23:16:49 host gpn_fleet[1]: [block] sealed #304151 at 23:16:49.503 hash=0xdef
Nov 14 23:17:05 host gpn_provider[2]: job 6554 completed in 12.503 s
EOF
[ "$(run "$tmp/t1")" = 0 ] && ok "timestamps ending .503 do not count" || bad "timestamp .503 counted as a fault"

# 2. A real HTTP 503 does.
cat >"$tmp/t2" <<'EOF'
Nov 14 23:20:00 host gpn_accesspoint[3]: POST /jobs -> HTTP 503 (no compatible provider online)
EOF
[ "$(run "$tmp/t2")" = 1 ] && ok "HTTP 503 counts" || bad "HTTP 503 not counted"

# 3. The updater's `-> 504 Gateway Timeout` shape counts.
cat >"$tmp/t3" <<'EOF'
Nov 14 23:21:00 host gpn_update[4]: download https://example/gpn-linux.tar.gz -> HTTP 504 Gateway Timeout
EOF
[ "$(run "$tmp/t3")" = 1 ] && ok "HTTP 504 counts" || bad "HTTP 504 not counted"

# 4. The other real faults still count (regression guard on the pattern list).
for word in 'bad signature' 'UNREADABLE' 'thread panicked' 'invalid seal' 'unknown parent' 'pull rejected'; do
    printf 'Nov 14 23:22:00 host gpn_oracle[5]: %s happened\n' "$word" >"$tmp/tw"
    [ "$(run "$tmp/tw")" = 1 ] && ok "counts: $word" || bad "missed: $word"
done

# 5. Ordinary healthy lines stay quiet — block numbers, hashes, ports, sizes.
cat >"$tmp/t5" <<'EOF'
Nov 14 23:23:00 host gpn_fleet[1]: [chain-state] best #304160 0x2107f3c5d503 peers=4
Nov 14 23:23:00 host gpn_accesspoint[3]: [health] ok providers=7 latency_ms=202
Nov 14 23:23:01 host gpn_provider[2]: [assign] cursor since=6554 wait=25000
Nov 14 23:23:02 host gpn_update[4]: up to date (0.3.47), next check in 3600 s
EOF
[ "$(run "$tmp/t5")" = 0 ] && ok "healthy lines stay quiet" || bad "healthy line counted as a fault"

# 6. Empty journal: zero lines, zero faults, exit 0 (never a false alarm).
: >"$tmp/t6"
[ "$(run "$tmp/t6")" = 0 ] && ok "empty input is clean" || bad "empty input is not clean"

# 7. Unreadable file is an error (2), not a clean bill of health.
[ "$(run "$tmp/does-not-exist")" = 2 ] && ok "missing file exits 2" || bad "missing file did not exit 2"

echo
if [ "$fail" -ne 0 ]; then echo "$fail assertion(s) failed"; exit 1; fi
echo "all lane-check assertions passed"
