#!/usr/bin/env bash
# test-wallet-provision.sh — end-to-end checks for `gpn_node --ensure-wallet`
# (queue #96: the money identity must be per machine).
#
# Runs the REAL binaries (no mocks):
#   1. first run creates a keystore, prints a payout account, mode 0600
#   2. `gpn_wallet address <keystore>` derives the SAME address (same format,
#      same derivation — the launcher never invents a keystore shape)
#   3. second run preserves the file byte-for-byte and does not touch mtime
#   4. a corrupt keystore aborts (exit 3), file untouched, no replacement made
#   5. GPN_SIGNER overrides the keystore (and creates none)
#   6. GPN_ACCOUNT overrides the payout address
#   7. an invalid GPN_ACCOUNT aborts
#
# Usage: bash scripts/tests/test-wallet-provision.sh [BIN_DIR]
set -uo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${1:-$repo/gpn-agent/target/release}"
NODE="$BIN/gpn_node"
WALLET="$BIN/gpn_wallet"

[ -x "$NODE" ]   || { echo "missing $NODE (cargo build --release --bin gpn_node)"; exit 77; }
[ -x "$WALLET" ] || { echo "missing $WALLET (cargo build --release --bin gpn_wallet)"; exit 77; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export GPN_BASE_PATH="$tmp/data"
# A pinned base dir is only honoured when it looks like a real base; make it so
# (resolve_effective_base prefers a legacy DB otherwise).
mkdir -p "$GPN_BASE_PATH/chains/gpn_testnet/db"
KS="$GPN_BASE_PATH/keystore.json"

fail=0
ok()  { printf '  ok   %s\n' "$*"; }
bad() { printf '  FAIL %s\n' "$*"; fail=1; }
addr_of() { sed -n 's/.*payout account: \([1-9A-HJ-NP-Za-km-z]\{47,48\}\).*/\1/p' "$1" | head -1; }

echo "== 1. first run creates the wallet =="
"$NODE" --ensure-wallet > "$tmp/out1" 2>&1
rc=$?
a1="$(addr_of "$tmp/out1")"
[ "$rc" -eq 0 ] && ok "exit 0" || bad "exit $rc: $(cat "$tmp/out1")"
[ -f "$KS" ] && ok "keystore written: $KS" || bad "no keystore at $KS"
[ -n "$a1" ] && ok "printed payout account: $a1" || bad "no 'payout account:' line"
grep -q "this node mines and is paid to $a1" "$tmp/out1" \
  && ok "creation banner names the payee and the backup path" \
  || bad "creation banner missing/incorrect"
grep -q "back up the keystore at $KS" "$tmp/out1" || bad "no backup hint with the keystore path"
[ "$(stat -c %a "$KS")" = "600" ] && ok "mode 0600" || bad "mode $(stat -c %a "$KS")"

echo "== 2. address derivation matches gpn_wallet =="
d1="$("$WALLET" address "$KS")"
[ "$d1" = "$a1" ] && ok "gpn_wallet address == printed payout account" \
  || bad "gpn_wallet says '$d1', --ensure-wallet said '$a1'"

echo "== 3. second run preserves the keystore =="
m1="$(md5sum < "$KS")"; t1="$(stat -c %Y "$KS")"; sleep 1
"$NODE" --ensure-wallet > "$tmp/out2" 2>&1
a2="$(addr_of "$tmp/out2")"
m2="$(md5sum < "$KS")"; t2="$(stat -c %Y "$KS")"
[ "$a2" = "$a1" ] && ok "same payout account" || bad "address changed: $a1 -> $a2"
[ "$m1" = "$m2" ] && ok "bytes unchanged" || bad "keystore was rewritten"
[ "$t1" = "$t2" ] && ok "mtime unchanged (never overwritten)" || bad "keystore was rewritten (mtime moved)"
grep -q "preserved" "$tmp/out2" && ok "logged as preserved" || bad "not logged as preserved"

echo "== 4. corrupt keystore aborts instead of creating a new identity =="
cp "$KS" "$tmp/keep.json"
printf '{"address":"5FHne' > "$KS"
c1="$(md5sum < "$KS")"
"$NODE" --ensure-wallet > "$tmp/out3" 2>&1
rc=$?
c2="$(md5sum < "$KS")"
[ "$rc" -ne 0 ] && ok "non-zero exit ($rc)" || bad "corrupt keystore did not abort"
grep -qi "REFUSING" "$tmp/out3" && ok "error says REFUSING to replace it" || bad "error text: $(cat "$tmp/out3")"
grep -qi "payout address" "$tmp/out3" && ok "explains the payout-address consequence" || bad "no consequence explained"
[ "$c1" = "$c2" ] && ok "corrupt keystore untouched" || bad "corrupt keystore was modified"
cp "$tmp/keep.json" "$KS"

echo "== 5. explicit GPN_SIGNER overrides (and creates no keystore) =="
rm -f "$KS"
GPN_SIGNER=//Bob "$NODE" --ensure-wallet > "$tmp/out4" 2>&1
rc=$?
a4="$(addr_of "$tmp/out4")"
[ "$rc" -eq 0 ] && ok "exit 0" || bad "exit $rc: $(cat "$tmp/out4")"
[ "$a4" = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty" ] \
  && ok "derived the dev URI's account ($a4)" || bad "unexpected address '$a4'"
grep -qi "OVERRIDES" "$tmp/out4" && ok "logged as an override" || bad "override not logged"
[ ! -f "$KS" ] && ok "no keystore created by an override" || bad "override created a keystore"

echo "== 6. explicit GPN_ACCOUNT wins over the signer's address =="
GPN_SIGNER=//Bob GPN_ACCOUNT=5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY \
  "$NODE" --ensure-wallet > "$tmp/out5" 2>&1
a5="$(addr_of "$tmp/out5")"
[ "$a5" = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY" ] \
  && ok "GPN_ACCOUNT takes precedence" || bad "got '$a5'"

echo "== 7. invalid inputs abort =="
if GPN_ACCOUNT=nonsense "$NODE" --ensure-wallet > "$tmp/out6" 2>&1; then
  bad "invalid GPN_ACCOUNT was accepted"
else
  ok "invalid GPN_ACCOUNT rejected (exit $?)"
fi
if GPN_SIGNER="not a key" "$NODE" --ensure-wallet > "$tmp/out7" 2>&1; then
  bad "invalid GPN_SIGNER was accepted"
else
  ok "invalid GPN_SIGNER rejected"
fi
[ ! -f "$KS" ] && ok "no keystore created by failed overrides" || bad "a failed override wrote a keystore"

echo
if [ "$fail" -ne 0 ]; then echo "RESULT: FAIL"; exit 1; fi
echo "RESULT: PASS"
