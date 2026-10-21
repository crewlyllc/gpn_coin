#!/usr/bin/env bash
# gov-upgrade-rehearsal.sh — rehearse the **mainnet governance upgrade path** on a
# throwaway dev chain.
#
# What this proves (on a live chain, with a real 500 KB runtime artifact):
#   stage  → artifact validated (spec_name / spec_version gate), hashed, planned
#   propose→ GpnGovernance.propose(System.set_code{code}) from committee member A
#   approve→ GpnGovernance.approve(hash)                  from committee member B
#   status → 2/2 approvals decoded on-chain, timelock pending
#   execute→ refused by the tool *and* by the pallet (TimelockNotElapsed)
#   verify → polls, reports pending, and times out with a diagnosis
#
# What it deliberately does NOT do: wait 17 280 blocks (3 days) for the timelock
# to expire, or enact a runtime change on any shared chain. Enactment
# (execute → Root → System.set_code → :code + CodeUpdated) is covered by
#   cargo test -p gpn-runtime --test gov_upgrade_rehearsal
# which runs the same pallet against the real `Runtime` and fast-forwards the
# block number instead. The timelock is a runtime *constant*, so no CLI trick can
# shorten it: `System.set_storage` on `System.Number` trips frame_system's
# "Block number must be strictly increasing" assertion and halts the chain.
#
# Usage:  bash scripts/gov-upgrade-rehearsal.sh
# Env:    ARTIFACT=<wasm>   RPC_PORT=9955   P2P_PORT=30355   KEEP=1
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"
BASE=/tmp/gpn-gov-rehearsal
ARTIFACT="${ARTIFACT:-$REPO/gpn-node/target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm}"
RPC_PORT="${RPC_PORT:-9955}"
P2P_PORT="${P2P_PORT:-30355}"
EP="ws://127.0.0.1:$RPC_PORT"
BIN="$REPO/gpn-agent/target/debug/gpn_gov_upgrade"

say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

command -v gpn-node >/dev/null || { echo "gpn-node not on PATH" >&2; exit 1; }
[ -x "$BIN" ] || { echo "build the tool first: (cd gpn-agent && cargo build --bin gpn_gov_upgrade)" >&2; exit 1; }
[ -f "$ARTIFACT" ] || { echo "artifact not found: $ARTIFACT" >&2; exit 1; }

NODE_PID=""
cleanup() {
  if [ -n "$NODE_PID" ] && [ "${KEEP:-0}" != "1" ]; then
    kill "$NODE_PID" 2>/dev/null || true
    wait "$NODE_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

say "1/6 start a throwaway dev chain (spec_version 6 from the canonical WASM)"
rm -rf "$BASE/chain"; mkdir -p "$BASE"
NK=$(head -c 32 /dev/urandom | xxd -p -c 64)
gpn-node --chain dev --base-path "$BASE/chain" --alice --force-authoring \
  --node-key "$NK" --port "$P2P_PORT" --rpc-port "$RPC_PORT" \
  --rpc-external --rpc-methods unsafe >"$BASE/node.log" 2>&1 &
NODE_PID=$!
for _ in $(seq 1 30); do
  if curl -sf -m 2 -H 'Content-Type: application/json' \
      -d '{"id":1,"jsonrpc":"2.0","method":"chain_getHeader","params":[]}' \
      "http://127.0.0.1:$RPC_PORT" >/dev/null; then break; fi
  sleep 2
done
"$BIN" surface --endpoint "$EP"

say "2/6 stage the artifact (this is where a stale/mismatched WASM is rejected)"
"$BIN" stage "$ARTIFACT" --endpoint "$EP" --out "$BASE/plan.json"
PLAN="$BASE/plan.json"
HASH=$(python3 -c "import json;print(json.load(open('$PLAN'))['code_hash'])")

say "3/6 propose as committee member //Alice (this is the dev chain's own key)"
PROPOSE_OUT=$("$BIN" propose --plan "$PLAN" --endpoint "$EP" --signer //Alice)
echo "$PROPOSE_OUT"
PHASH=$(echo "$PROPOSE_OUT" | sed -n 's/^proposal hash: \(0x[0-9a-f]*\).*/\1/p' | head -1)
[ -n "$PHASH" ] || { echo "could not parse the proposal hash" >&2; exit 1; }

say "4/6 approve as //Bob (2-of-2 on a dev chain; 2-of-3 on mainnet)"
"$BIN" approve "$PHASH" --endpoint "$EP" --signer //Bob

say "5/6 status: 2/2 approvals, timelock still running"
"$BIN" status "$PHASH" --endpoint "$EP"

say "6/6 execute is refused until the timelock elapses (3 days of blocks)"
if "$BIN" execute "$PHASH" --endpoint "$EP" --signer //Charlie; then
  echo "unexpected: execute was accepted" >&2; exit 1
else
  echo "(refused, as designed)"
fi

say "verify: polls and reports the pending state, then times out with a diagnosis"
"$BIN" verify --plan "$PLAN" --endpoint "$EP" --timeout 12 --interval 4 || true

say "rehearsal complete"
echo "artifact code hash: $HASH"
echo "proposal hash:      $PHASH"
echo "(enactment itself: cargo test -p gpn-runtime --test gov_upgrade_rehearsal)"
