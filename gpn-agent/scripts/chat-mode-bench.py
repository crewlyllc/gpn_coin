#!/usr/bin/env python3
"""Latency comparison for /v1/chat/completions payment modes.

Runs N requests per mode against the live AP on 127.0.0.1:8092 and reports
p50/p95, plus the payment mode the AP itself reports for each response.
"""
import json, statistics, sys, time, urllib.request

URL = "http://127.0.0.1:8092/v1/chat/completions"
N = int(sys.argv[1]) if len(sys.argv) > 1 else 5
MODES = sys.argv[2].split(",") if len(sys.argv) > 2 else ["channel", "passthrough", "settle"]

def one(mode):
    body = {
        "model": "llama3.2:1b",
        "messages": [{"role": "user", "content": "Reply with exactly: pong"}],
        "max_tokens": 16,
        "mode": mode,
    }
    req = urllib.request.Request(
        URL, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"}
    )
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=180) as r:
        payload = json.load(r)
    dt = time.time() - t0
    payment = (payload.get("gpn") or {}).get("payment")
    return dt, payment, payload

out = {}
for mode in MODES:
    times, payments = [], []
    for i in range(N):
        try:
            dt, payment, payload = one(mode)
        except Exception as e:
            print(f"{mode}: request {i} FAILED: {e}", flush=True)
            continue
        times.append(dt)
        payments.append(payment)
        print(f"{mode} #{i+1}: {dt*1000:.0f} ms  payment={payment}", flush=True)
    if not times:
        continue
    times.sort()
    out[mode] = {
        "n": len(times),
        "p50_ms": round(statistics.median(times) * 1000, 1),
        "p95_ms": round(times[min(len(times) - 1, int(0.95 * len(times)))] * 1000, 1),
        "min_ms": round(times[0] * 1000, 1),
        "max_ms": round(times[-1] * 1000, 1),
        "payments": sorted(set(p for p in payments if p)),
    }
print(json.dumps(out, indent=2))
