#!/bin/bash
# Live proof of the automatic channel->on-chain fallback.
# Temporarily replaces the AP on :8092 with an instance whose hub is DISABLED,
# sends one `mode:"channel"` request, then restores the real service.
set -u
export HOME=/home/jeremyg
set -a; . /home/jeremyg/.gpn/accesspoint.env; set +a

cleanup() {
  [ -n "${AP_PID:-}" ] && kill "$AP_PID" 2>/dev/null
  sleep 1
  systemctl --user start gpn-accesspoint
  sleep 6
  echo "RESTORED: $(systemctl --user is-active gpn-accesspoint) $(curl -s -m 3 http://127.0.0.1:8092/health)"
}
trap cleanup EXIT

systemctl --user stop gpn-accesspoint
sleep 2

GPN_ESCROW_MODE=onchain nohup /home/jeremyg/.local/bin/gpn_scheduler \
  --listen 0.0.0.0:8092 --allow-hot-wallet --signer-uri "$AP_SEED" \
  --providers http://127.0.0.1:8093 > /tmp/ap-fallback-test.log 2>&1 &
AP_PID=$!
echo "test AP pid $AP_PID"

# The provider must poll THIS instance before it is routable.
for i in $(seq 1 40); do
  N=$(curl -s -m 2 http://127.0.0.1:8092/providers | grep -o '"healthy":true' | wc -l)
  [ "$N" -ge 1 ] && break
  sleep 1
done
echo "healthy providers seen: $N (waited ${i}s)"

echo "--- request (mode:channel, hub disabled) ---"
/usr/bin/python3 - <<'EOF'
import json, time, urllib.request, urllib.error
body = {"model":"llama3.2:1b","messages":[{"role":"user","content":"Reply with exactly: pong"}],"max_tokens":16,"mode":"channel"}
req = urllib.request.Request("http://127.0.0.1:8092/v1/chat/completions",
                            data=json.dumps(body).encode(),
                            headers={"Content-Type":"application/json"})
for attempt in range(1, 9):
    t0 = time.time()
    try:
        p = json.load(urllib.request.urlopen(req, timeout=300))
    except urllib.error.HTTPError as e:
        print(f"attempt {attempt}: HTTP {e.code} {e.read()[:200]}", flush=True)
        time.sleep(8)  # provider has not polled this instance yet
        continue
    print("elapsed_ms", round((time.time()-t0)*1000))
    print("payment", p.get("gpn"))
    print("text", p["choices"][0]["message"]["content"][:60])
    break
else:
    print("GAVE UP")
EOF
echo "--- fallback log ---"
grep -E "\[chat\] channel path" /tmp/ap-fallback-test.log | tail -3
