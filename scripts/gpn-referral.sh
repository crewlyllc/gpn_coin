#!/usr/bin/env bash
# gpn-referral.sh — referral-code helpers shared by install.sh and the tests.
#
# Data collection only: no rewards, no chain writes. See
# gpn-agent/src/referral.rs (the Rust side reads this same file) and
# shared/artifacts/project7/airdrop/REFERRAL-AND-MIGRATION-POLICY.md.
#
# STORAGE (the one and only place):  $GPN_PREFIX/referral   (default ~/.gpn/referral)
#   one line, the code exactly as given. Written ONCE; never rewritten.
#
# Usage (source, do not execute):
#   . "$(dirname "$0")/gpn-referral.sh"
#   referral_validate "$code"     # rc=0 valid, prints nothing; rc=1 prints reason
#   referral_write_once "$PREFIX" "$code"
#                                 # rc=0 written | rc=2 already set | rc=1 invalid
#
# Format: 4–32 chars from [A-Za-z0-9_-]. Comparison is case-insensitive; the
# value is stored as given.

GPN_REFERRAL_MIN=4
GPN_REFERRAL_MAX=32

# referral_validate CODE -> 0 valid, 1 invalid (reason on stdout)
referral_validate() {
  local code="${1-}"
  # trim leading/trailing whitespace the way the Rust side does
  code="${code#"${code%%[![:space:]]*}"}"
  code="${code%"${code##*[![:space:]]}"}"
  local len=${#code}
  if [ "$len" -eq 0 ]; then
    echo "empty referral code"
    return 1
  fi
  if [ "$len" -lt "$GPN_REFERRAL_MIN" ]; then
    echo "referral code too short (need ${GPN_REFERRAL_MIN}-${GPN_REFERRAL_MAX} chars)"
    return 1
  fi
  if [ "$len" -gt "$GPN_REFERRAL_MAX" ]; then
    echo "referral code too long (need ${GPN_REFERRAL_MIN}-${GPN_REFERRAL_MAX} chars)"
    return 1
  fi
  case "$code" in
    *[!A-Za-z0-9_-]*)
      echo "referral code may only contain letters, digits, '-' and '_'"
      return 1
      ;;
  esac
  return 0
}

# referral_normalize CODE -> lowercase (comparison only)
referral_normalize() {
  printf '%s' "${1-}" | tr '[:upper:]' '[:lower:]'
}

# referral_write_once PREFIX CODE
#   0 = written now | 2 = already set (new value ignored) | 1 = invalid (nothing written)
# Never fails the caller: an invalid code is a warning, not an install error.
referral_write_once() {
  local prefix="${1-}" raw="${2-}"
  local file="$prefix/referral"
  local code
  code="${raw#"${raw%%[![:space:]]*}"}"
  code="${code%"${code##*[![:space:]]}"}"
  if ! referral_validate "$code" >/dev/null; then
    return 1
  fi
  mkdir -p "$prefix" 2>/dev/null || return 1
  if [ -e "$file" ]; then
    return 2
  fi
  # noclobber makes the redirect an O_EXCL create: two concurrent installs
  # cannot both win, exactly like `create_new` on the Rust side.
  if ( set -o noclobber; printf '%s\n' "$code" > "$file" ) 2>/dev/null; then
    return 0
  fi
  # Lost the race (or the file appeared): treat as already set.
  [ -e "$file" ] && return 2
  return 1
}

# referral_read PREFIX -> prints the stored code (rc=1 when there is none)
referral_read() {
  local file="${1-}/referral"
  [ -f "$file" ] || return 1
  local line
  line="$(sed -n '/^[[:space:]]*$/!{p;q;}' "$file" 2>/dev/null)"
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line%"${line##*[![:space:]]}"}"
  [ -n "$line" ] || return 1
  printf '%s\n' "$line"
}
