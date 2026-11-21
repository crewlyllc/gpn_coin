#!/bin/bash
# Row 228 live rig — an accepted heavier branch at depth > 32 must actually
# import and reorg (the 33..288 band that was dead before this change).
#
# Shape (the 2026-11-20 stage-3 construction, without needing `revert`):
#   * mine a shared prefix to $PREFIX on all cores;
#   * copy the data dir twice: branch A and branch B;
#   * A keeps mining SLOWLY (2 cores -> blocks slower than the 15 s target, so
#     each retarget eases its difficulty);
#   * B mines FAST (every core -> blocks faster than target, so each retarget
#     hardens its difficulty);
#   * B's chain then carries MORE cumulative work while staying SHORTER, which
#     is exactly `Decision::Reorg` with depth > 32;
#   * A and B are connected (reserved nodes, at startup: the node refuses to
#     dial loopback through the runtime API, row 195) and A must log
#     `reorg=accepted depth=..`, request the branch, import it and adopt B's tip.
#
# Loopback only, spare ports (3038x / 998x). The live node on 30333 / 9944 is
# never touched. Run under setsid; it logs to $ROOT/rig.log.
set -u
ROOT=/tmp/row228/rig
BIN=${BIN:-/home/jeremyg/.ci-cache/gpn-node-target/release/gpn-node}
SPEC=/tmp/row228/raw.json
PREFIX=${PREFIX:-90}          # blocks mined before the split
A_TARGET=${A_TARGET:-215}     # A's height when the branches are connected
B_MARGIN=${B_MARGIN:-40}      # required depth (A.height - B.height)
A_CPUS=${A_CPUS:-0-1}         # A is throttled so its difficulty eases
CPUS="0-$(( $(nproc) - 1 ))"

mkdir -p "$ROOT"
# kill a previous rig node (only gpn-node processes; never this shell)
for d in /proc/[0-9]*; do
  pid=${d#/proc/}
  [ "$pid" = "$$" ] && continue
  c=$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null) || continue
  case "$c" in *gpn-node*row228/raw.json*) kill -9 "$pid" 2>/dev/null;; esac
done
sleep 4
rm -rf "$ROOT"; mkdir -p "$ROOT"
exec >> "$ROOT/rig.log" 2>&1
echo "=== rig start $(date -u +%FT%TZ) bin=$BIN"

rpc() { curl -s -m 5 -H 'Content-Type: application/json' \
  -d "{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"$2\",\"params\":${3:-[]}}" \
  "http://127.0.0.1:$1"; }
jid() { rpc "$1" system_localPeerId | python3 -c 'import sys,json
try: print(json.load(sys.stdin)["result"])
except Exception: print("")'; }
height() { rpc "$1" chain_getHeader | python3 -c 'import sys,json
try: print(int(json.load(sys.stdin)["result"]["number"],16))
except Exception: print(-1)'; }
besthash() { rpc "$1" chain_getHeader | python3 -c 'import sys,json
try: print(json.load(sys.stdin)["result"]["parentHash"])
except Exception: print("")'; }
peers() { rpc "$1" system_health | python3 -c 'import sys,json
try: print(json.load(sys.stdin)["result"]["peers"])
except Exception: print(0)'; }
workof() { python3 -c 'import json,sys
try:
  d=json.load(open(sys.argv[1])); print(d.get("work",0), d.get("height",0))
except Exception: print(0,0)' "$1/chain-work.json" 2>/dev/null; }

node() { # dir port rpcport keyfile cpus standby reserved-addr
  local dir=$1 port=$2 rpcp=$3 key=$4 cpus=$5 standby=$6 reserved=${7:-}
  mkdir -p "$dir"
  rm -f "$dir/chains/gpn_dev/network/secret_ed25519" 2>/dev/null
  local extra=()
  [ -n "$reserved" ] && extra+=(--reserved-nodes "$reserved")
  GPN_REQUIRE_DIALABLE=0 GPN_STANDBY="$standby" \
  taskset -c "$cpus" "$BIN" \
    --chain "$SPEC" --base-path "$dir" --name "r228-$(basename "$dir")" \
    --node-key-file "$key" --listen-addr "/ip4/127.0.0.1/tcp/$port" \
    --no-mdns --no-seed --no-discoverable --no-upnp --no-telemetry --no-prometheus \
    --rpc-port "$rpcp" --rpc-methods unsafe --rpc-cors all \
    --sync full --force-authoring --in-peers 8 --out-peers 8 --reserved-only \
    --database paritydb -l info,runtime::gpu-pow=debug,fork_choice=debug \
    "${extra[@]}" \
    > "$ROOT/$(basename "$dir").log" 2>&1 &
  echo $!
}

wait_height() { local rpcp=$1 target=$2 t=$3 i=0 h
  while [ "$i" -lt "$t" ]; do h=$(height "$rpcp"); [ "$h" -ge "$target" ] && return 0
    sleep 5; i=$((i+5)); done; return 1; }
wait_peer() { local rpcp=$1 t=$2 i=0
  while [ "$i" -lt "$t" ]; do [ "$(peers "$rpcp")" -ge 1 ] && return 0
    sleep 5; i=$((i+5)); done; return 1; }

for k in A B WA WB; do head -c 32 /dev/urandom > "$ROOT/key$k"; done
# A placeholder reserved entry, only so the node does NOT adopt the public
# testnet's oracle seeds (that path runs when the reserved set is empty, and the
# live chain's "different chain" error poisons this rig's sync service). The
# dial fails harmlessly; the real peer is added after both ids are known.
SELF_A="/ip4/127.0.0.1/tcp/30999/p2p/12D3KooWKiDgAJ53LdVn8vCJcwBPVZNfKEAfYV6QT3XYfLyiPGQe"
SELF_B="/ip4/127.0.0.1/tcp/30998/p2p/12D3KooWKiDgAJ53LdVn8vCJcwBPVZNfKEAfYV6QT3XYfLyiPGQe"

echo "--- phase 1: prefix to #$PREFIX (all cores, witness W connected)"
# A starts first with a (self) reserved entry so the node does NOT adopt the
# public testnet's oracle seeds — those reach the live chain and their
# "different chain" error poisons the rig's sync service.
PA=$(node "$ROOT/A" 30381 9981 "$ROOT/keyA" "$CPUS" 0 "$SELF_A")
sleep 10; PEERA=$(jid 9981); echo "A peer id: $PEERA"
PW=$(node "$ROOT/WA" 30382 9982 "$ROOT/keyWA" "$CPUS" 1 "/ip4/127.0.0.1/tcp/30381/p2p/$PEERA")
sleep 10; echo "W peer id: $(jid 9982)"
echo "connect: $(rpc 9981 system_addReservedPeer "[\"/ip4/127.0.0.1/tcp/30382/p2p/$(jid 9982)\"]")"
sleep 15
wait_peer 9981 120 && echo "A peers: $(peers 9981)" || echo "NO PEER on A"
wait_height 9981 "$PREFIX" 2400 && echo "prefix at #$(height 9981)" || echo "PREFIX TIMEOUT at #$(height 9981)"
grep -h ADJUST "$ROOT/A.log" | head -3
kill "$PA" "$PW" 2>/dev/null; sleep 8
read -r wA hA < <(workof "$ROOT/A"); echo "prefix end: #$hA work=$wA"

echo "--- split: B is a copy of A at #$hA"
cp -a "$ROOT/A" "$ROOT/B"

echo "--- phase 2: A slow ($A_CPUS) to #$A_TARGET, B fast, each with its own witness"
PA=$(node "$ROOT/A" 30381 9981 "$ROOT/keyA" "$A_CPUS" 0 "$SELF_A")
PB=$(node "$ROOT/B" 30383 9983 "$ROOT/keyB" "$CPUS" 0 "$SELF_B")
sleep 10
PW=$(node "$ROOT/WA" 30382 9982 "$ROOT/keyWA" "$CPUS" 1 "/ip4/127.0.0.1/tcp/30381/p2p/$(jid 9981)")
PWB=$(node "$ROOT/WB" 30384 9984 "$ROOT/keyWB" "$CPUS" 1 "/ip4/127.0.0.1/tcp/30383/p2p/$(jid 9983)")
sleep 10
rpc 9981 system_addReservedPeer "[\"/ip4/127.0.0.1/tcp/30382/p2p/$(jid 9982)\"]" >/dev/null
rpc 9983 system_addReservedPeer "[\"/ip4/127.0.0.1/tcp/30384/p2p/$(jid 9984)\"]" >/dev/null
sleep 20
echo "peers: A=$(peers 9981) B=$(peers 9983)"

i=0
while [ "$i" -lt 7200 ]; do
  sleep 30; i=$((i+30))
  hA=$(height 9981); hB=$(height 9983)
  read -r wA _ < <(workof "$ROOT/A"); read -r wB _ < <(workof "$ROOT/B")
  echo "t=${i}s A=#$hA work=$wA | B=#$hB work=$wB | depth=$((hA-hB))"
  [ "$hA" -ge "$A_TARGET" ] && break
  if [ "$hB" -gt $((hA - B_MARGIN + 2)) ] && [ "$hB" -gt 0 ]; then
    echo "   B too tall (#$hB vs A #$hA) — pausing B until A pulls ahead"
    kill "$PB" 2>/dev/null; sleep 8
    until [ "$(height 9981)" -ge $((hB + B_MARGIN + 10)) ]; do sleep 20; done
    PB=$(node "$ROOT/B" 30383 9983 "$ROOT/keyB" "$CPUS" 0 "/ip4/127.0.0.1/tcp/30384/p2p/$(jid 9984)")
    sleep 15
  fi
done
hA=$(height 9981); hB=$(height 9983)
read -r wA _ < <(workof "$ROOT/A"); read -r wB _ < <(workof "$ROOT/B")
echo "--- before connection: A=#$hA work=$wA  B=#$hB work=$wB  depth=$((hA-hB))"

echo "--- phase 3: connect A and B (restart both with each other as reserved node)"
kill "$PA" "$PB" 2>/dev/null; sleep 10
# both peer ids are fixed by the key files, so read them from the logs
PEERA=$(grep -ho "Local node identity is: 12D3[A-Za-z0-9]*" "$ROOT/A.log" | tail -1 | awk '{print $NF}')
PEERB=$(grep -ho "Local node identity is: 12D3[A-Za-z0-9]*" "$ROOT/B.log" | tail -1 | awk '{print $NF}')
echo "A id=$PEERA  B id=$PEERB"
PA=$(node "$ROOT/A" 30381 9981 "$ROOT/keyA" "$A_CPUS" 0 "/ip4/127.0.0.1/tcp/30383/p2p/$PEERB")
PB=$(node "$ROOT/B" 30383 9983 "$ROOT/keyB" "$CPUS" 0 "/ip4/127.0.0.1/tcp/30381/p2p/$PEERA")
sleep 120

echo "--- evidence from A's own log"
grep -hE "fork-choice|block-serving: asking|Imported #|chain-state: peer" "$ROOT/A.log" | tail -45
hA2=$(height 9981); hB2=$(height 9983)
echo "--- after: A=#$hA2 $(besthash 9981)  B=#$hB2 $(besthash 9983)"

python3 - "$hA2" "$hB2" <<'PY'
import json,sys,urllib.request
def rpc(port,method,params):
    req=urllib.request.Request("http://127.0.0.1:%d"%port,
        json.dumps({"id":1,"jsonrpc":"2.0","method":method,"params":params}).encode(),
        {"Content-Type":"application/json"})
    return json.load(urllib.request.urlopen(req,timeout=5))["result"]
ha=int(sys.argv[1]); hb=int(sys.argv[2])
top=min(ha,hb)
for h in range(max(1,top-6), top+1):
    try:
        a=rpc(9981,"chain_getBlockHash",[h]); b=rpc(9983,"chain_getBlockHash",[h])
    except Exception as e:
        print("  #%d rpc error %s"%(h,e)); continue
    print("  #%d A=%s B=%s %s"%(h,str(a)[:14],str(b)[:14],"SAME" if a==b else "DIFF"))
PY

kill "$PA" "$PB" 2>/dev/null
echo "=== rig end $(date -u +%FT%TZ)"
