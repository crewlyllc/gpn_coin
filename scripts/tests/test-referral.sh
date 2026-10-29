#!/usr/bin/env bash
# test-referral.sh — offline tests for referral capture (data collection only).
#
# Covers:
#   1. install-arg parsing: --ref CODE / --ref=CODE / GPN_REF, --help text,
#      missing value, unknown flag (all without running an install)
#   2. write-once storage: first value wins, later values are ignored, invalid
#      values never create the file and never fail the caller
#   3. absent code: no file, so a node behaves exactly as before
#   4. Windows launcher: carries the same rules (text-level; no pwsh here)
#   5. snapshot attribution: referred_by per account + referral_summary
#
# No network, no cargo, no chain.
#
# Usage: scripts/tests/test-referral.sh
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
HELPER="$REPO/scripts/gpn-referral.sh"
INSTALL="$REPO/install.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  \033[32mok\033[0m   %s\n' "$*"; }
bad()  { fail=$((fail+1)); printf '  \033[31mFAIL\033[0m %s\n' "$*"; }
check() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/gpn-ref-test.XXXXXX")"
cleanup() { rm -rf "$ROOT"; }
trap cleanup EXIT

[ -f "$HELPER" ] || { echo "FATAL: $HELPER missing" >&2; exit 2; }
[ -f "$INSTALL" ] || { echo "FATAL: $INSTALL missing" >&2; exit 2; }

# ── 1. install.sh argument parsing ────────────────────────────────────────────
# A fake PATH with no cargo/git stops install.sh right after argument parsing
# (step 1 = prerequisites), so nothing is built, cloned or installed.
mkdir -p "$ROOT/emptybin"

install_run() { # install_run <env-assignments…> -- <args…>
  PATH="$ROOT/emptybin" HOME="$ROOT/home" /bin/bash "$INSTALL" "$@" 2>&1
}
mkdir -p "$ROOT/home"

out="$(install_run --ref)"
if printf '%s' "$out" | grep -q -- '--ref needs a code'; then
  ok "--ref without a value is a clear error"
else
  bad "--ref without a value is a clear error: $out"
fi

out="$(install_run --bogus)"
if printf '%s' "$out" | grep -q 'unknown argument: --bogus'; then
  ok "an unknown flag is still rejected"
else
  bad "an unknown flag is still rejected: $out"
fi

for spec in "--ref team-alpha" "--ref=team-alpha"; do
  # shellcheck disable=SC2086
  out="$(install_run $spec)"
  if printf '%s' "$out" | grep -q 'cargo not found' \
     && ! printf '%s' "$out" | grep -q 'unknown argument'; then
    ok "install.sh accepts '$spec' (proceeds past arg parsing)"
  else
    bad "install.sh accepts '$spec': $out"
  fi
done

out="$(GPN_REF=team-alpha install_run)"
if printf '%s' "$out" | grep -q 'cargo not found'; then
  ok "install.sh accepts GPN_REF"
else
  bad "install.sh accepts GPN_REF: $out"
fi

out="$(/bin/bash "$INSTALL" --help 2>&1)"   # exits before any install step; needs a real PATH for sed
if printf '%s' "$out" | grep -q -- '--ref CODE'; then
  ok "--help documents --ref"
else
  bad "--help documents --ref: $out"
fi

# install.sh must delegate to the shared helper (one implementation, tested here).
if grep -q 'gpn-referral.sh' "$INSTALL"; then
  ok "install.sh sources scripts/gpn-referral.sh"
else
  bad "install.sh sources scripts/gpn-referral.sh"
fi

# ── 2. write-once storage (the code install.sh actually calls) ────────────────
# shellcheck source=/dev/null
. "$HELPER"

P="$ROOT/prefix1"
if referral_write_once "$P" "Team-Alpha"; then ok "valid code is written"; else bad "valid code is written"; fi
if [ "$(cat "$P/referral" 2>/dev/null)" = "Team-Alpha" ]; then
  ok "code is stored as given (case preserved), one line"
else
  bad "code storage: $(cat "$P/referral" 2>/dev/null)"
fi

referral_write_once "$P" "second-team" >/dev/null 2>&1
rc=$?
if [ "$rc" = 2 ]; then ok "a second --ref is refused (rc=2)"; else bad "second --ref rc=$rc (want 2)"; fi
if [ "$(cat "$P/referral")" = "Team-Alpha" ]; then
  ok "the first value is never overwritten"
else
  bad "write-once violated: $(cat "$P/referral")"
fi
if [ "$(referral_read "$P")" = "Team-Alpha" ]; then ok "referral_read returns the stored code"; else bad "referral_read"; fi

# Case-insensitive comparison helper (what the AP/self-referral check uses).
if [ "$(referral_normalize 'Team-Alpha')" = "team-alpha" ]; then
  ok "normalize is lowercase (comparison only)"
else
  bad "normalize is lowercase"
fi

# Invalid values: rejected, nothing written, caller not failed hard.
i=0
for badcode in "abc" "x y z" "ab!d" "$(printf 'a%.0s' $(seq 1 33))"; do
  i=$((i+1))
  P2="$ROOT/bad$i"
  referral_write_once "$P2" "$badcode" >/dev/null 2>&1
  rc=$?
  if [ "$rc" = 1 ] && [ ! -e "$P2/referral" ]; then
    ok "invalid code '$badcode' is refused and writes nothing"
  else
    bad "invalid code '$badcode' rc=$rc file=$([ -e "$P2/referral" ] && echo present || echo absent)"
  fi
done

# Boundaries are accepted.
P3="$ROOT/bounds1"; referral_write_once "$P3" "abcd" >/dev/null
[ "$(referral_read "$P3")" = "abcd" ] && ok "4-char code accepted" || bad "4-char code accepted"
P4="$ROOT/bounds2"; referral_write_once "$P4" "$(printf 'a%.0s' $(seq 1 32))" >/dev/null
[ "$(referral_read "$P4")" = "$(printf 'a%.0s' $(seq 1 32))" ] && ok "32-char code accepted" || bad "32-char code accepted"

# ── 3. absent code: no file at all (a node with no ref is unchanged) ──────────
P5="$ROOT/prefix-noref"
mkdir -p "$P5"
if ! referral_read "$P5" >/dev/null 2>&1 && [ ! -e "$P5/referral" ]; then
  ok "no code -> no file (backward compatible)"
else
  bad "no code -> no file"
fi

# ── 4. Windows launcher parity (text-level) ───────────────────────────────────
WIN="$REPO/run-windows-provider.ps1"
[ -f "$WIN" ] || { echo "FATAL: $WIN missing" >&2; exit 2; }
for needle in '$env:GPN_REF' 'CreateNew' '\.gpn' 'already set to'; do
  if grep -q -- "$needle" "$WIN"; then
    ok "Windows launcher carries '$needle'"
  else
    bad "Windows launcher carries '$needle'"
  fi
done
if grep -qi 'referral' "$WIN"; then ok "Windows launcher documents referral"; else bad "Windows launcher documents referral"; fi

# ── 5. snapshot attribution + summary ────────────────────────────────────────
cat > "$ROOT/registry.json" <<'JSON'
{
  "5AAA": { "ref": "team-alpha", "first_seen": 1700000000 },
  "0xBBBB": { "ref": "team-beta", "first_seen": 1700000001 },
  "5CCC": { "ref": "team-alpha", "first_seen": 1700000002 },
  "5DDD": { "ref": "   ", "first_seen": 1700000003 }
}
JSON

/usr/bin/python3 - "$REPO/gpn-snapshot.py" "$ROOT/registry.json" <<'PY'
import importlib.util, json, sys
snap_path, reg_path = sys.argv[1], sys.argv[2]
spec = importlib.util.spec_from_file_location("gpn_snapshot", snap_path)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

refs = m.load_referrals(reg_path)
assert refs["5AAA"]["ref"] == "team-alpha", refs
assert "5DDD" not in refs, "blank/unknown codes must not be attributed"
assert m.load_referrals("/nonexistent/x.json") == {}, "a missing registry is empty, never fatal"

records = [
    {"ss58": "5AAA", "hex": "0xaaaa", "blocks_authored": 10, "jobs_served": 3,
     "jobs_created": 1, "observed_uptime_bps": 8000, "active_seconds": 100},
    {"ss58": "5CCC", "hex": "0xcccc", "blocks_authored": 5, "jobs_served": 1,
     "jobs_created": 0, "observed_uptime_bps": 6000, "active_seconds": 50},
    {"ss58": "0xBBBB", "hex": "0xbbbb", "blocks_authored": 2, "jobs_served": 0,
     "jobs_created": 9, "observed_uptime_bps": 10000, "active_seconds": 25},
    {"ss58": "5EEE", "hex": "0xeeee", "blocks_authored": 1, "jobs_served": 0,
     "jobs_created": 0, "observed_uptime_bps": 0, "active_seconds": 0},
]
n = m.attribute_referrals(records, refs)
assert n == 3, n
assert records[0]["referred_by"] == "team-alpha"
# hex-keyed registry entries attribute by hex too
assert records[2]["referred_by"] == "team-beta", records[2]
assert "referred_by" not in records[3], "unattributed accounts get no key"

s = m.referral_summary(records)
assert set(s) == {"team-alpha", "team-beta"}, s
a = s["team-alpha"]
assert a["referred_addresses"] == 2, a
assert a["blocks_authored"] == 15 and a["jobs_served"] == 4 and a["jobs_created"] == 1, a
assert a["mean_uptime_bps"] == 7000, a
assert a["active_seconds_sum"] == 150, a
assert s["team-beta"]["referred_addresses"] == 1
print("python-snapshot-ok")
PY
if [ $? = 0 ]; then ok "snapshot attributes accounts + aggregates per code"; else bad "snapshot attribution"; fi

# ── summary ──────────────────────────────────────────────────────────────────
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ] || exit 1
