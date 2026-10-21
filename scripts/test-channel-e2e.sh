#!/usr/bin/env bash
# scripts/test-channel-e2e.sh — prove the off-chain escrow (payment channel)
# job path end-to-end on a LIVE dev node, and prove it issues **zero per-job
# on-chain transactions**.
#
# What it does:
#   1. snapshots `ComputeMarketplace::NextJobId`;
#   2. starts a provider (channel endpoint = heartbeat port + 1) and a heartbeat
#      on free ports, plus a second access point using its own signer;
#   3. POSTs N AI jobs to the access point (channel mode);
#   4. asserts `NextJobId` did NOT move (no per-job create/claim/complete),
#      prints the per-job wall time, the off-chain channel state, and settles
#      cooperatively on chain at the end.
#
# Requirements: a running node (ENDPOINT, default ws://127.0.0.1:9944), ollama,
# and a funded requestor signer (AP_SIGNER, default //Charlie on a dev chain).
#
# Env:
#   ENDPOINT        ws://127.0.0.1:9944
#   AP_SIGNER       //Charlie
#   PROVIDER_SIGNER hex seed or //Alice — must be a REGISTERED provider
#   PROVIDER_ADDR   ss58 of that provider (required if PROVIDER_SIGNER has no
#                   heartbeat); the script reads it from the heartbeat otherwise
#   JOBS            number of jobs (default 3)
#   CHANNEL_DEPOSIT base units locked in the channel (default 100 GPN)
#   KEEP            1 = leave the processes running for inspection
#
# Exit: 0 = all jobs served off-chain AND NextJobId unchanged.
set -euo pipefail

cd "$(dirname "$0")/.."
BIN=./gpn-agent/target/debug
ENDPOINT=${ENDPOINT:-ws://127.0.0.1:9944}
AP_SIGNER=${AP_SIGNER:-//Charlie}
PROVIDER_SIGNER=${PROVIDER_SIGNER:?set PROVIDER_SIGNER (registered provider seed/URI)}
JOBS=${JOBS:-3}
DEPOSIT=${CHANNEL_DEPOSIT:-100000000000000}
STATUS_PORT=${STATUS_PORT:-8095}
AP_PORT=${AP_PORT:-8097}
WORK=${WORK:-$(mktemp -d /tmp/gpn-channel-e2e.XXXXXX)}

for b in gpn_llm_provider gpn_provider_status gpn_scheduler gpn_channel; do
  [ -x "$BIN/$b" ] || { echo "missing $BIN/$b — run: (cd gpn-agent && cargo build --bins)"; exit 2; }
done
curl -sf -m 3 http://127.0.0.1:11434/api/tags >/dev/null || { echo "ollama not reachable on :11434"; exit 2; }

next_job_id() {
  /usr/bin/python3 - "$ENDPOINT" <<'PY'
import hashlib, json, struct, sys, urllib.request
def xxh64(data, seed=0):
    P1=0x9E3779B185EBCA87;P2=0xC2B2AE3D27D4EB4F;P3=0x165667B19E3779F9;P4=0x85EBCA77C2B2AE63;P5=0x27D4EB2F165667C5
    M=0xFFFFFFFFFFFFFFFF
    def rotl(x,r): return ((x<<r)|(x>>(64-r)))&M
    n=len(data); i=0
    if n>=32:
        v1=(seed+P1+P2)&M;v2=(seed+P2)&M;v3=seed&M;v4=(seed-P1)&M
        while i+32<=n:
            v1=(rotl((v1+int.from_bytes(data[i:i+8],'little')*P2)&M,31)*P1)&M
            v2=(rotl((v2+int.from_bytes(data[i+8:i+16],'little')*P2)&M,31)*P1)&M
            v3=(rotl((v3+int.from_bytes(data[i+16:i+24],'little')*P2)&M,31)*P1)&M
            v4=(rotl((v4+int.from_bytes(data[i+24:i+32],'little')*P2)&M,31)*P1)&M
            i+=32
        h=(rotl(v1,1)+rotl(v2,7)+rotl(v3,12)+rotl(v4,18))&M
        for v in (v1,v2,v3,v4):
            v=(rotl((v*P2)&M,31)*P1)&M; h=((h^v)*P1+P4)&M
    else: h=(seed+P5)&M
    h=(h+n)&M
    while i+8<=n:
        h=(rotl(h^((rotl((int.from_bytes(data[i:i+8],'little')*P2)&M,31)*P1)&M),27)*P1+P4)&M;i+=8
    if i+4<=n:
        h=(rotl(h^((int.from_bytes(data[i:i+4],'little')*P1)&M),23)*P2+P3)&M;i+=4
    while i<n:
        h=(rotl(h^(data[i]*P5&M),11)*P1)&M;i+=1
    h^=h>>33;h=(h*P2)&M;h^=h>>29;h=(h*P3)&M;h^=h>>32
    return h
def twox128(d): return struct.pack('<Q',xxh64(d,0))+struct.pack('<Q',xxh64(d,1))
url=sys.argv[1].replace("ws://","http://")
key="0x"+(twox128(b"ComputeMarketplace")+twox128(b"NextJobId")).hex()
req=urllib.request.Request(url,data=json.dumps({"jsonrpc":"2.0","id":1,"method":"state_getStorage","params":[key]}).encode(),headers={"Content-Type":"application/json"})
v=json.load(urllib.request.urlopen(req,timeout=8))["result"]
print(int.from_bytes(bytes.fromhex(v[2:]),'little'))
PY
}

cleanup() {
  [ "${KEEP:-0}" = "1" ] && { echo "KEEP=1 — pids: $(cat "$WORK"/*.pid 2>/dev/null | tr '\n' ' ')"; return; }
  for f in "$WORK"/*.pid; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null || true; done
}
trap cleanup EXIT

before=$(next_job_id)
echo "NextJobId before: $before"
echo "work dir: $WORK"

# 1) heartbeat (the AP derives the channel endpoint as port+1)
GPN_PROVIDER_ADDR=${PROVIDER_ADDR:-} setsid nohup "$BIN/gpn_provider_status" \
  --listen "127.0.0.1:$STATUS_PORT" ${PROVIDER_ADDR:+--address "$PROVIDER_ADDR"} \
  --gflops 30000 --capacity 1 --tok-per-s 60 --owner-cost-usd-hr 0.03 \
  > "$WORK/status.log" 2>&1 < /dev/null & echo $! > "$WORK/status.pid"

# 2) provider — channel endpoint on STATUS_PORT+1
GPN_INFER_LISTEN="127.0.0.1:$((STATUS_PORT + 1))" \
GPN_PROVIDER_CHANNEL_STATE="$WORK/provider_channels.json" \
GPN_BASELINE_MODELS= \
setsid nohup "$BIN/gpn_llm_provider" "$ENDPOINT" "$PROVIDER_SIGNER" 60000 \
  > "$WORK/provider.log" 2>&1 < /dev/null & echo $! > "$WORK/provider.pid"

sleep 3
# 3) access point in channel mode
GPN_ESCROW_MODE=channel GPN_HUB_STATE="$WORK/hub.json" \
GPN_HUB_CHANNEL_DEPOSIT="$DEPOSIT" GPN_HUB_JOB_DEADLINE_MS=180000 \
setsid nohup "$BIN/gpn_scheduler" --listen "127.0.0.1:$AP_PORT" \
  --signer-uri "$AP_SIGNER" --allow-hot-wallet \
  --providers "http://127.0.0.1:$STATUS_PORT/status" \
  --assign-store "$WORK/assignments.json" > "$WORK/ap.log" 2>&1 < /dev/null & echo $! > "$WORK/ap.pid"

# The AP does chain discovery at boot (~5-15 s on a busy node); wait for it
# instead of racing it with the first job.
for _ in $(seq 1 40); do
  curl -sf -m 2 "http://127.0.0.1:$AP_PORT/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -sf -m 2 "http://127.0.0.1:$AP_PORT/health" >/dev/null || {
  echo "AP did not become healthy:"; cat "$WORK/ap.log"; exit 1; }

# `/health` is up before the first provider refresh completes; wait until the
# registry actually knows a model (otherwise the AP rejects the job as
# "unknown model ... available: []").
for _ in $(seq 1 30); do
  n=$(curl -sf -m 2 "http://127.0.0.1:$AP_PORT/providers" 2>/dev/null \
    | /usr/bin/python3 -c 'import json,sys; print(sum(len(p.get("models") or []) for p in json.load(sys.stdin).get("providers",[])))' 2>/dev/null || echo 0)
  [ "${n:-0}" -gt 0 ] && break
  sleep 1
done
[ "${n:-0}" -gt 0 ] || { echo "AP registry never learned a model"; cat "$WORK/ap.log"; exit 1; }

fail=0
for i in $(seq 1 "$JOBS"); do
  t0=$(date +%s%N)
  out=$(curl -s -m 200 -X POST "http://127.0.0.1:$AP_PORT/jobs" -H 'Content-Type: application/json' \
    -d '{"model":"llama3.2:1b","prompt":"say hi in three words","max_tokens":24,"wait":true,"wait_secs":180}')
  t1=$(date +%s%N)
  ms=$(( (t1 - t0) / 1000000 ))
  printf '%s' "$out" > "$WORK/job$i.json"
  /usr/bin/python3 - "$WORK/job$i.json" "$i" "$ms" <<'PY' || fail=1
import json,sys
d=json.load(open(sys.argv[1]))
ok=bool(d.get("ok")) and d.get("escrow")=="channel"
ans=(d.get("answer") or {}).get("response","")
u=(d.get("usage") or {}).get("total_tokens")
print("  job %s: escrow=%s channel=%s %sms usage=%s -> %.50r" % (
    sys.argv[2], d.get("escrow"), d.get("channel_id"), sys.argv[3], u, ans))
if not ok: print("    payload:", json.dumps(d)[:400])
sys.exit(0 if ok else 1)
PY
done

after=$(next_job_id)
echo "NextJobId after:  $after"
echo "--- AP ---";     grep -E "\[hub\]" "$WORK/ap.log" | tail -5
echo "--- provider ---"; grep -E "\[channel\]" "$WORK/provider.log" | tail -5
echo "--- off-chain channel state ---"
/usr/bin/python3 - "$WORK/hub.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
for k,v in d.get("channels",{}).items():
    s=v["state"]
    print(f"  {k}: channel {v['channel_id']} nonce {s['nonce']} req {s['balance_requestor']} prov {s['balance_provider']}")
PY

if [ "$before" != "$after" ]; then
  echo "FAIL: NextJobId moved $before -> $after — a per-job on-chain tx happened"
  fail=1
fi
[ "$fail" = 1 ] && { echo "FAIL"; exit 1; }

# 4) cooperative settle: restart the AP with a 1-unit close threshold and wait
for f in "$WORK/ap.pid"; do kill "$(cat "$f")" 2>/dev/null || true; done
sleep 1
GPN_ESCROW_MODE=channel GPN_HUB_STATE="$WORK/hub.json" GPN_HUB_CLOSE_AT=1 GPN_HUB_CLOSE_NONCE=1 \
setsid nohup "$BIN/gpn_scheduler" --listen "127.0.0.1:$AP_PORT" --signer-uri "$AP_SIGNER" \
  --allow-hot-wallet --providers "http://127.0.0.1:$STATUS_PORT/status" \
  --assign-store "$WORK/assignments.json" > "$WORK/ap-settle.log" 2>&1 < /dev/null & echo $! > "$WORK/ap.pid"
echo "waiting for the settler (up to 75 s)…"
for _ in $(seq 1 25); do
  grep -q "settled on chain" "$WORK/ap-settle.log" && break
  sleep 3
done
grep -E "settling|settled" "$WORK/ap-settle.log" || { echo "FAIL: no on-chain settlement"; exit 1; }

echo "PASS: $JOBS jobs served off-chain, NextJobId unchanged ($before), channel settled on chain."
