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
B_BLOCKS=${B_BLOCKS:-60}      # blocks B must accrue after the split before it may be frozen
CPUS="0-$(( $(nproc) - 1 ))"
NPROC=$(nproc)
# B must be genuinely faster than A. Torch GPU: the node mines on the GPU, so
# CPU affinity does NOT change hashrate (measured 02:31: both branches hashed at
# the same rate). Force A onto the CPU light-cache miner by pointing the OpenCL
# ICD loader at an empty directory, and leave the GPU to B.
B_CPUS=${B_CPUS:-2-$(( NPROC - 1 ))}
mkdir -p /tmp/row228/no-icd
CPU_ONLY="OCL_ICD_VENDORS=/tmp/row228/no-icd"

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
# RPC is not up the moment the process is spawned (substrate binds it well after
# the miner starts). Sampling `jid` after a fixed 10 s returned EMPTY on 2026-11-21
# 02:21, which built an "invalid multihash" reserved address, the witness never
# connected, the peer-watch saw 0 peers and PAUSED SEALING at #45 for 40 minutes.
# Poll until the RPC answers instead of guessing a sleep.
wait_jid() { local rpcp=$1 t=${2:-180} i=0 id=""
  while [ "$i" -lt "$t" ]; do id=$(jid "$rpcp")
    [ -n "$id" ] && { echo "$id"; return 0; }
    sleep 3; i=$((i+3)); done; echo ""; return 1; }
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

node() { # dir port rpcport keyfile cpus standby reserved-addr [extra-env] [logfilter]
  local dir=$1 port=$2 rpcp=$3 key=$4 cpus=$5 standby=$6 reserved=${7:-} xenv=${8:-} logf=${9:-}
  mkdir -p "$dir"
  rm -f "$dir/chains/gpn_dev/network/secret_ed25519" 2>/dev/null
  local extra=()
  [ -n "$reserved" ] && extra+=(--reserved-nodes "$reserved")
  # `$xenv` is a word-split "K=V ..." prefix. Used to force the CPU light-cache
  # miner on one branch: the node mines on the GPU, so core affinity cannot make
  # one branch faster than the other — the 02:31 run proved that (both branches
  # hashed at the same rate and `shorter but heavier` was unreachable).
  env $xenv GPN_REQUIRE_DIALABLE=0 GPN_STANDBY="$standby" \
  taskset -c "$cpus" "$BIN" \
    --chain "$SPEC" --base-path "$dir" --name "r228-$(basename "$dir")" \
    --node-key-file "$key" --listen-addr "/ip4/127.0.0.1/tcp/$port" \
    --no-mdns --no-seed --no-discoverable --no-upnp --no-telemetry --no-prometheus \
    --rpc-port "$rpcp" --rpc-methods unsafe --rpc-cors all \
    --sync full --force-authoring --in-peers 8 --out-peers 8 --reserved-only \
    --database paritydb -l "${logf:-info,runtime::gpu-pow=debug,fork_choice=debug}" \
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
PA=$(node "$ROOT/A" 30381 9981 "$ROOT/keyA" "$CPUS" 0 "$SELF_A" "$CPU_ONLY")
sleep 5; PEERA=$(wait_jid 9981 180); echo "A peer id: $PEERA"
PW=$(node "$ROOT/WA" 30382 9982 "$ROOT/keyWA" "$CPUS" 1 "/ip4/127.0.0.1/tcp/30381/p2p/$PEERA")
PEERWA=$(wait_jid 9982 180); echo "W peer id: $PEERWA"
echo "connect: $(rpc 9981 system_addReservedPeer "[\"/ip4/127.0.0.1/tcp/30382/p2p/$PEERWA\"]")"
sleep 15
wait_peer 9981 120 && echo "A peers: $(peers 9981)" || echo "NO PEER on A"
# A witness is not decoration: with 0 connected peers the peer-watch pauses
# sealing (observed 02:21-02:30: stuck at #45). If the witness is not connected,
# stop now rather than spend 40 minutes looking like it is mining.
if [ "$(peers 9981)" -lt 1 ]; then
  echo "--- NO PEER on A: sealing would pause — aborting (check the rig's reserved dial)"
  kill "$PA" "$PW" 2>/dev/null; echo "=== rig end $(date -u +%FT%TZ)"; exit 4
fi
wait_height 9981 "$PREFIX" 2400 && echo "prefix at #$(height 9981)" || echo "PREFIX TIMEOUT at #$(height 9981)"
grep -h ADJUST "$ROOT/A.log" | head -3
kill "$PA" "$PW" 2>/dev/null; sleep 8
read -r wA hA < <(workof "$ROOT/A"); echo "prefix end: #$hA work=$wA"

echo "--- split: B is a copy of A at #$hA"
cp -a "$ROOT/A" "$ROOT/B"

echo "--- phase 2: A slow ($A_CPUS) to #$A_TARGET, B fast until it is heavier but >=$B_MARGIN shorter"
# The 01:59 run failed at exactly this step: B was "paused until A pulls ahead"
# by killing and restarting it. Every restart gave B a window of slow blocks,
# which the retarget read as "too slow" and EASED, so B ended both shorter and
# lighter (A work/block 6.05M vs B 1.63M). A rig that keeps re-easing the branch
# it wants heavier can never build one. Freeze B with SIGSTOP instead: its chain
# and its difficulty stay exactly where they were, and A keeps mining.
PA=$(node "$ROOT/A" 30381 9981 "$ROOT/keyA" "$A_CPUS" 0 "$SELF_A" "$CPU_ONLY")
PB=$(node "$ROOT/B" 30383 9983 "$ROOT/keyB" "$B_CPUS" 0 "$SELF_B")
PEERA=$(wait_jid 9981 180); PEERB=$(wait_jid 9983 180); echo "phase2 ids: A=$PEERA B=$PEERB"
PW=$(node "$ROOT/WA" 30382 9982 "$ROOT/keyWA" "$A_CPUS" 1 "/ip4/127.0.0.1/tcp/30381/p2p/$PEERA")
PWB=$(node "$ROOT/WB" 30384 9984 "$ROOT/keyWB" "$A_CPUS" 1 "/ip4/127.0.0.1/tcp/30383/p2p/$PEERB")
PEERWA=$(wait_jid 9982 180); PEERWB=$(wait_jid 9984 180)
sleep 10
rpc 9981 system_addReservedPeer "[\"/ip4/127.0.0.1/tcp/30382/p2p/$PEERWA\"]" >/dev/null
rpc 9983 system_addReservedPeer "[\"/ip4/127.0.0.1/tcp/30384/p2p/$PEERWB\"]" >/dev/null
sleep 20
echo "peers: A=$(peers 9981) B=$(peers 9983)"
if [ "$(peers 9981)" -lt 1 ] || [ "$(peers 9983)" -lt 1 ]; then
  echo "--- phase 2 has a branch with 0 peers: sealing would pause — aborting"
  kill "$PA" "$PB" "$PW" "$PWB" 2>/dev/null; echo "=== rig end $(date -u +%FT%TZ)"; exit 4
fi

frozen=0
FB_H=0
SPLIT_H=$(height 9981)
MARGIN_DEPTH=$((B_MARGIN + 5))
stall_h=-1; stall_t=0
i=0
while [ "$i" -lt 7200 ]; do
  sleep 20; i=$((i+20))
  hA=$(height 9981); hB=$(height 9983)
  read -r wA _ < <(workof "$ROOT/A"); read -r wB _ < <(workof "$ROOT/B")
  echo "t=${i}s A=#$hA work=$wA | B=#$hB work=$wB | depth=$((hA-hB)) frozen=$frozen"
  # A stall guard: a difficulty that is too hard for the CPU miner looks exactly
  # like a healthy node (idle peer, no errors) and burns 40 minutes. Say it.
  if [ "$hA" = "$stall_h" ]; then stall_t=$((stall_t+20)); else stall_h=$hA; stall_t=0; fi
  if [ "$stall_t" -ge 300 ]; then
    echo "--- A STALLED at #$hA for ${stall_t}s (CPU miner cannot find a block at this difficulty) — aborting"
    kill -CONT "$PB" "$PWB" 2>/dev/null
    kill -9 "$PA" "$PB" "$PW" "$PWB" 2>/dev/null
    echo "=== rig end $(date -u +%FT%TZ)"; exit 5
  fi
  if [ "$frozen" = "0" ]; then
    # B must accrue real work BEFORE it is frozen, otherwise we would compare a
    # 2-block branch against 130 blocks and correctly lose. Two admissible
    # freezes: (a) B already has $B_BLOCKS blocks since the split, or (b) B is
    # already heavier while still 40+ blocks behind.
    mined=$((hB - SPLIT_H))
    if [ "$mined" -ge "$B_BLOCKS" ]; then
      echo "   B mined $mined blocks since the split at #$hB — freezing B, A pulls ahead"
      FB_H=$hB
      kill -STOP "$PB" "$PWB" 2>/dev/null; frozen=1; sleep 2
    elif [ "$mined" -ge 20 ] && [ "$wB" -gt "$wA" ] && [ "$hB" -le $((hA - B_MARGIN)) ]; then
      echo "   B is heavier ($wB > $wA) at depth $((hA-hB)) — freezing B at #$hB"
      FB_H=$hB
      kill -STOP "$PB" "$PWB" 2>/dev/null; frozen=1; sleep 2
    elif false; then
      kill -STOP "$PB" "$PWB" 2>/dev/null; frozen=1; sleep 2
    fi
  fi
  if [ "$frozen" = "1" ] && [ "$hA" -ge "$A_TARGET" ] && [ $((hA - FB_H)) -ge "$MARGIN_DEPTH" ]; then break; fi
done
hA=$(height 9981); hB=${FB_H:-0}
read -r wA _ < <(workof "$ROOT/A"); read -r wB _ < <(workof "$ROOT/B")
echo "--- before connection: A=#$hA work=$wA  B=#$hB (frozen height) work=$wB  depth=$((hA-hB))"
if [ "$hB" -gt 0 ] && [ "$hB" -le $((hA - B_MARGIN)) ] && [ "$wB" -gt "$wA" ]; then
  echo "--- CONSTRUCTION OK: B is shorter by $((hA-hB)) and heavier by $((wB-wA))"
else
  echo "--- CONSTRUCTION FAILED: B must be >=$B_MARGIN shorter AND heavier"
  kill -CONT "$PB" "$PWB" 2>/dev/null; kill "$PA" "$PB" "$PW" "$PWB" 2>/dev/null
  echo "=== rig end $(date -u +%FT%TZ)"; exit 3
fi

echo "--- phase 3: connect A and B (restart both with each other as reserved node)"
kill -CONT "$PB" "$PWB" 2>/dev/null
kill -9 "$PB" "$PWB" 2>/dev/null
kill "$PA" "$PW" 2>/dev/null; sleep 10
# both peer ids are fixed by the key files, so read them from the logs
PEERA=$(grep -ho "Local node identity is: 12D3[A-Za-z0-9]*" "$ROOT/A.log" | tail -1 | awk '{print $NF}')
PEERB=$(grep -ho "Local node identity is: 12D3[A-Za-z0-9]*" "$ROOT/B.log" | tail -1 | awk '{print $NF}')
# Row 228, fifth run: the fourth run's accept verdict was overwritten 29 s later
# because B kept MINING after the restart (03:18:04 "ahead at #271"), which
# cleared `accepted_fork` and closed the gate before the fork request could be
# answered. Freezing B with SIGSTOP is not the fix (a stopped node cannot serve
# the branch either) — start B in GPN_STANDBY, which keeps it a full node that
# SYNCs and SERVES but neither authors nor mines (`service::standby_requested`).
# So B holds ONE fixed heavier branch at ONE fixed depth for the whole window.
# `gpn_node::block_serving=debug` + `sc_network_sync=debug` on A make both halves
# of the question visible: whether the request is emitted at all, and what the
# gate did with it.
PA=$(node "$ROOT/A" 30381 9981 "$ROOT/keyA" "$A_CPUS" 0 "/ip4/127.0.0.1/tcp/30383/p2p/$PEERB" "$CPU_ONLY" \
      "info,gpn_node::block_serving=debug,sc_network_sync=debug,fork_choice=debug,runtime::gpu-pow=debug")
PB=$(node "$ROOT/B" 30383 9983 "$ROOT/keyB" "$B_CPUS" 1 "/ip4/127.0.0.1/tcp/30381/p2p/$PEERA")
wait_jid 9981 180 >/dev/null; wait_jid 9983 180 >/dev/null

# Five-minute observation window (the fourth run held 120 s and the sync tick
# had not fired yet). Poll every 15 s and print the lines that answer the
# question as they appear, so the log is evidence even if the run is cut short.
echo "--- phase 3 window: 300 s, B in standby (serves, does not mine)"
seen=0
for t in 15 30 45 60 90 120 150 180 210 240 270 300; do
  sleep 15
  hA3=$(height 9981); hB3=$(height 9983)
  read -r wA3 _ < <(workof "$ROOT/A"); read -r wB3 _ < <(workof "$ROOT/B")
  echo "t=${t}s A=#$hA3 work=$wA3 | B=#$hB3 work=$wB3 | depth=$((hA3-hB3))"
  n=$(grep -cE "fork-choice: work|block-serving: (asking|not asking)|Downloading (fork|requested fork)|Explicit sync request|Refusing to sync known" "$ROOT/A.log" 2>/dev/null)
  if [ "$n" -gt "$seen" ]; then
    grep -hE "fork-choice: work|block-serving: (asking|not asking)|Downloading (fork|requested fork)|Explicit sync request|Refusing to sync known" "$ROOT/A.log" | tail -$((n-seen)) | sed 's/^/   | /'
    seen=$n
  fi
done

echo "--- evidence from A's own log"
grep -hE "fork-choice|block-serving|Imported #|chain-state: peer|Downloading fork|Explicit sync request|Refusing to sync known" "$ROOT/A.log" | tail -60
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
