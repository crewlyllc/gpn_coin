#!/usr/bin/env bash
# lane-check.sh — count REAL fault lines in the GPN journals.
#
# Why this exists (W172): the standing sweep grepped bare `503`, so a millisecond
# timestamp like `...:46.503` counted as a fault — 438 "hits" in one 60-minute
# window with 0 real HTTP 503s. Clean bills that rest on a pattern that matches
# the clock are luck, not health. Every pattern here is shaped so that a
# timestamp cannot match it, and `scripts/tests/test-lane-check.sh` plants both
# kinds of line and asserts which way they land.
#
# Usage:
#   scripts/lane-check.sh [--since "-40 min"] [--file <path>] [unit ...]
#
#   --file   read a saved journal instead of asking journalctl (used by tests,
#            and for pasting evidence into a summary)
#   units    default: the services the maintainer sweep watches
#
# Exit: 0 = no fault line matched, 1 = at least one matched, 2 = usage/IO error.
set -uo pipefail

SINCE="-40 min"
FILE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --since) SINCE="${2:?--since needs a value}"; shift 2 ;;
        --file)  FILE="${2:?--file needs a path}"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) break ;;
    esac
done

UNITS=("$@")
if [ ${#UNITS[@]} -eq 0 ]; then
    UNITS=(gpn-fleet gpn-accesspoint gpn-provider gpn-provider-heartbeat gpn-oracle gpn-diffusion gpn-finetune)
fi

# Fault patterns. Each one is anchored on prose, never on a bare number:
#   * an HTTP status only counts when it is introduced as HTTP/status/`-> 503`
#   * `:46.503` (a millisecond timestamp) matches none of them
PATTERNS=(
    'HTTP[ /]?5[0-9][0-9]\b'
    '\b(status|Status|code|Code)[ =:]+5[0-9][0-9]\b'
    '-> 5[0-9][0-9] '          # the updater's `download ... -> HTTP 504` shape
    '\b(Service Unavailable|Internal Server Error|Bad Gateway|Gateway Timeout)\b'
    'bad signature'
    'UNREADABLE'
    '\bpanic(ked|s)?\b'
    'invalid seal'
    'unknown parent'
    'dispatch reverted'
    'pull rejected'
)

PATTERN=$(IFS='|'; echo "${PATTERNS[*]}")

if [ -n "$FILE" ]; then
    [ -r "$FILE" ] || { echo "lane-check: cannot read $FILE" >&2; exit 2; }
    SRC=$(cat "$FILE")
else
    ARGS=()
    for u in "${UNITS[@]}"; do ARGS+=(-u "$u"); done
    SRC=$(journalctl --user "${ARGS[@]}" --since "$SINCE" --no-pager 2>/dev/null) || true
fi

tmpf=$(mktemp)
trap 'rm -f "$tmpf"' EXIT
if [ -n "$SRC" ]; then printf '%s\n' "$SRC" > "$tmpf"; else : > "$tmpf"; fi
TOTAL=$(wc -l < "$tmpf" | tr -d ' ')
HITS=$(grep -Ei -- "$PATTERN" "$tmpf")
NHITS=$(grep -Eic -- "$PATTERN" "$tmpf")

printf 'lane-check: %s lines, %s fault line(s) \u2014 units: %s\n' "$TOTAL" "$NHITS" "${UNITS[*]}"
if [ "$NHITS" -gt 0 ]; then
    printf '%s\n' "$HITS"
    exit 1
fi
exit 0
