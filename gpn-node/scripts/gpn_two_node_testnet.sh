#!/usr/bin/env bash
#
# GPN two-node local p2p testnet.
#
# Boots TWO `gpn-node` processes on the `local` chain spec (Alice + Bob as
# authorities), connects them over libp2p via --bootnodes, and verifies:
#
#   1. BOTH nodes are mining with the REAL GPU (OpenCL) Ethash miner
#      ("GPU Ethash miner online: <device>") — not the CPU fallback.
#   2. BOTH nodes connect as peers ("1 peers").
#   3. BOTH nodes converge on the SAME longest chain (identical best-block
#      hash) — i.e. blocks mined by either peer are imported by the other.
#
# This is the longest-chain + GPU-mining-across-peers integration test for the
# Phase-3 prototype.
#
# Usage:
#   ./gpn_two_node_testnet.sh [--binary path/to/gpn-node] [--keep]
#
#   --binary   Path to the gpn-node binary (default: ../target/release/gpn-node).
#              NOTE: must be a build that includes the GPU miner (cycle 11+);
#              a stale release binary predates it and will silently use the
#              CPU fallback. Use target/debug/gpn-node to be safe.
#   --keep     Leave the two nodes running on exit (for manual poking).
#              Otherwise they are killed and tmp dirs removed.
#
# Exit code 0 = all checks pass. Non-zero = a check failed.
#
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/gpn-node}"
KEEP=0

while [ $# -gt 0 ]; do
  case "$1" in
    --binary) BIN="${2?--binary needs a path}"; shift 2 ;;
    --keep)   KEEP=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

WORK="${TMPDIR:-/tmp}/gpn_two_node"
LOG_A="$WORK/a.log"
LOG_B="$WORK/b.log"
PORT_A=30333; PORT_B=30334
RPC_A=9944;   RPC_B=9945

[ -x "$BIN" ] || { echo "binary not found/executable: $BIN" >&2; exit 2; }

# Deterministic-ish fixed node keys (so peer IDs are stable across runs).
KEY_A="4ecac9dd3ce8f61ac2216dec9c366fb490a0d946db37947e05a4dc4de6ab543c"
KEY_B="fa430efe2bdb815b886454769ac9c4961d79df0eeb3a54e4c9e97001e5aef466"
PEER_A="12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"

PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); echo "  ✅ $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  ❌ $1"; }

cleanup() {
  if [ "$KEEP" -eq 1 ]; then
    echo "[keep] leaving nodes running (A: rpc $RPC_A, B: rpc $RPC_B)"
    return
  fi
  pkill -f "gpn-node --chain local --alice" 2>/dev/null
  pkill -f "gpn-node --chain local --bob"   2>/dev/null
  rm -rf "$WORK/a" "$WORK/b"
}
trap cleanup EXIT

rm -rf "$WORK"; mkdir -p "$WORK"

echo "== Binary: $BIN =="
echo "== Starting node A (Alice, bootnode, GPU mining) on :$PORT_A =="
"$BIN" --chain local --alice --force-authoring \
  --node-key "$KEY_A" --port "$PORT_A" --rpc-port "$RPC_A" \
  --base-path "$WORK/a" --no-telemetry --no-prometheus --rpc-cors all \
  > "$LOG_A" 2>&1 &
PID_A=$!
echo "   A pid=$PID_A"

echo "== Starting node B (Bob, GPU mining) on :$PORT_B, bootnodes->A =="
"$BIN" --chain local --bob --force-authoring \
  --node-key "$KEY_B" --port "$PORT_B" --rpc-port "$RPC_B" \
  --base-path "$WORK/b" \
  --bootnodes "/ip4/127.0.0.1/tcp/$PORT_A/p2p/$PEER_A" \
  --no-telemetry --no-prometheus --rpc-cors all \
  > "$LOG_B" 2>&1 &
PID_B=$!
echo "   B pid=$PID_B"

# --- Wait for GPU miner + first block on A -----------------------------------
echo "== Waiting for node A: GPU miner online + block production =="
A_GPU=0; A_BLOCK=0
for i in $(seq 1 60); do
  grep -q "GPU Ethash miner online" "$LOG_A" 2>/dev/null && A_GPU=1
  grep -q "Imported #1\b\|Imported #2\b\|Imported #3\b\|Imported #4\b\|Imported #5\b" "$LOG_A" 2>/dev/null && A_BLOCK=1
  [ "$A_GPU" -eq 1 ] && [ "$A_BLOCK" -eq 1 ] && break
  sleep 2
done
[ "$A_GPU"  -eq 1 ] && ok "node A GPU miner online"      || bad "node A GPU miner did NOT come online"
[ "$A_BLOCK" -eq 1 ] && ok "node A produced blocks"      || bad "node A produced no blocks"

# --- Wait for node B: GPU miner + peers + sync -------------------------------
echo "== Waiting for node B: GPU miner online + peer + sync =="
B_GPU=0; B_PEER=0; B_BLOCK=0
for i in $(seq 1 90); do
  grep -q "GPU Ethash miner online" "$LOG_B" 2>/dev/null && B_GPU=1
  grep -q "(1 peers)\|(2 peers)" "$LOG_B" 2>/dev/null && B_PEER=1
  grep -q "Imported #1\b\|Imported #2\b\|Imported #3\b\|Imported #4\b\|Imported #5\b" "$LOG_B" 2>/dev/null && B_BLOCK=1
  [ "$B_GPU" -eq 1 ] && [ "$B_PEER" -eq 1 ] && [ "$B_BLOCK" -eq 1 ] && break
  sleep 2
done
[ "$B_GPU"  -eq 1 ] && ok "node B GPU miner online"  || bad "node B GPU miner did NOT come online"
[ "$B_PEER" -eq 1 ] && ok "node B connected as a peer" || bad "node B never connected as a peer"
[ "$B_BLOCK" -eq 1 ] && ok "node B imported blocks (synced)" || bad "node B imported no blocks"

# --- Verify both nodes converge on the same best block -----------------------
# Require a NON-ZERO block number so we prove longest-chain convergence on
# blocks actually mined by the peers (not just the shared genesis).
echo "== Verifying both nodes share the same longest chain =="
best_of() { grep -oE "best: #[0-9]+ \(0x[0-9a-f]{4}" "$1" | tail -1; }
blocknum() { echo "$1" | grep -oE "#[0-9]+" | tr -d '#'; }
BEST_A=""; BEST_B=""; CONV=0
for i in $(seq 1 60); do
  BEST_A="$(best_of "$LOG_A")"
  BEST_B="$(best_of "$LOG_B")"
  N_A="$(blocknum "$BEST_A")"
  N_B="$(blocknum "$BEST_B")"
  if [ -n "$BEST_A" ] && [ -n "$BEST_B" ] \
     && [ "$BEST_A" = "$BEST_B" ] && [ "${N_A:-0}" -ge 1 ]; then
    CONV=1; break
  fi
  sleep 2
done
if [ "$CONV" -eq 1 ]; then
  ok "nodes converged on same mined block: $BEST_A"
else
  bad "nodes did NOT converge on a mined block: A='${BEST_A:-none}' B='${BEST_B:-none}'"
fi

echo
echo "==== RESULT: $PASS passed, $FAIL failed ===="
echo "logs: $LOG_A , $LOG_B"
[ "$FAIL" -eq 0 ]
