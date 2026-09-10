#!/usr/bin/env bash
# generate-treasury-roster.sh — generate the 25 governance roster accounts.
#
# Produces two files:
#   treasury-roster-seeds.txt   ⚠️ SECRET — 25 seed phrases. Store offline; never share/commit.
#   treasury-roster-addrs.txt   ✅ PUBLIC — 25 SS58 addresses. Safe to share.
#   treasury-roster.env         ready-to-use `GPN_TREASURY_ROSTER=...` line.
#
# Usage:
#   ./generate-treasury-roster.sh [COUNT] [NODE_BIN]
#     COUNT     number of accounts (default 25)
#     NODE_BIN  path to gpn-node (default: ./target/release/gpn-node)
set -euo pipefail

COUNT="${1:-25}"
NODE_BIN="${2:-./target/release/gpn-node}"

if [ ! -x "$NODE_BIN" ]; then
  echo "error: gpn-node binary not found at '$NODE_BIN'." >&2
  echo "build it first:  cd gpn-node && cargo build --release -p gpn-node" >&2
  exit 1
fi

SEEDS="treasury-roster-seeds.txt"
ADDRS="treasury-roster-addrs.txt"
ENVF="treasury-roster.env"

: > "$SEEDS"
: > "$ADDRS"

addrs=()
for i in $(seq 1 "$COUNT"); do
  # `key generate` prints the phrase and the SS58 address; capture both.
  out="$("$NODE_BIN" key generate --scheme sr25519 --words 24 2>/dev/null)"
  phrase="$(printf '%s\n' "$out" | sed -n 's/^[[:space:]]*Secret phrase:[[:space:]]*//p' | sed 's/[[:space:]]*$//')"
  ss58="$(printf '%s\n' "$out" | sed -n 's/^[[:space:]]*SS58 Address:[[:space:]]*//p' | sed 's/[[:space:]]*$//')"
  if [ -z "$phrase" ] || [ -z "$ss58" ]; then
    echo "error: failed to parse key generate output for #$i" >&2
    exit 1
  fi
  printf '%02d\t%s\t%s\n' "$i" "$phrase" "$ss58" >> "$SEEDS"
  printf '%02d\t%s\n' "$i" "$ss58" >> "$ADDRS"
  addrs+=("$ss58")
  printf '  generated %02d/%s  %s\n' "$i" "$COUNT" "$ss58"
done

# Build the comma-separated env line.
joined="$(IFS=,; echo "${addrs[*]}")"
printf 'GPN_TREASURY_ROSTER=%s\n' "$joined" > "$ENVF"

echo
echo "Done."
echo "  SECRET seeds  -> $SEEDS    (⚠️ keep offline, chmod 600, never commit/share)"
echo "  PUBLIC addrs  -> $ADDRS"
echo "  Env line      -> $ENVF"
echo
echo "Next: send the PUBLIC addresses ($ADDRS) to your dev, or run:"
echo "  chmod 600 $SEEDS"
