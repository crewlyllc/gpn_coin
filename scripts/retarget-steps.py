#!/usr/bin/env python3
"""Empirical retarget steps actually applied by the live chain.

Reads the harvested (block, timestamp, difficulty-target) series and reports the
distribution of `new/stored` at every retarget event. This is the controller's
*output* — no simulation, no model — so it is the strongest single piece of
evidence about whether the loop is damped.

Usage: retarget-steps.py [--cache /tmp/gpnbt/harvest.json] [--blocks 2500]
"""
import argparse
import json
import os
import statistics
import struct
import urllib.request

M64 = 0xFFFFFFFFFFFFFFFF


def _xxh64(data, seed=0):
    P1, P2, P3, P4, P5 = (0x9E3779B185EBCA87, 0xC2B2AE3D27D4EB4F,
                          0x165667B19E3779F9, 0x85EBCA77C2B2AE63,
                          0x27D4EB2F165667C5)

    def rotl(x, r):
        return ((x << r) | (x >> (64 - r))) & M64

    n = len(data)
    i = 0
    if n >= 32:
        v1 = (seed + P1 + P2) & M64
        v2 = (seed + P2) & M64
        v3 = seed & M64
        v4 = (seed - P1) & M64
        while i + 32 <= n:
            v1 = (rotl((v1 + int.from_bytes(data[i:i + 8], 'little') * P2) & M64, 31) * P1) & M64
            v2 = (rotl((v2 + int.from_bytes(data[i + 8:i + 16], 'little') * P2) & M64, 31) * P1) & M64
            v3 = (rotl((v3 + int.from_bytes(data[i + 16:i + 24], 'little') * P2) & M64, 31) * P1) & M64
            v4 = (rotl((v4 + int.from_bytes(data[i + 24:i + 32], 'little') * P2) & M64, 31) * P1) & M64
            i += 32
        h = (rotl(v1, 1) + rotl(v2, 7) + rotl(v3, 12) + rotl(v4, 18)) & M64
        for v in (v1, v2, v3, v4):
            v = (rotl((v * P2) & M64, 31) * P1) & M64
            h = ((h ^ v) * P1 + P4) & M64
    else:
        h = (seed + P5) & M64
    h = (h + n) & M64
    while i + 8 <= n:
        h = (rotl(h ^ ((rotl((int.from_bytes(data[i:i + 8], 'little') * P2) & M64, 31) * P1) & M64), 27) * P1 + P4) & M64
        i += 8
    if i + 4 <= n:
        h = (rotl(h ^ ((int.from_bytes(data[i:i + 4], 'little') * P1) & M64), 23) * P2 + P3) & M64
        i += 4
    while i < n:
        h = (rotl(h ^ (data[i] * P5 & M64), 11) * P1) & M64
        i += 1
    h ^= h >> 33
    h = (h * P2) & M64
    h ^= h >> 29
    h = (h * P3) & M64
    h ^= h >> 32
    return h


def twox128(d):
    return struct.pack('<Q', _xxh64(d, 0)) + struct.pack('<Q', _xxh64(d, 1))


TS = "0x" + twox128(b"Timestamp").hex() + twox128(b"Now").hex()
DF = "0x" + (twox128(b"GPUPoW") + twox128(b"Difficulty")).hex()
RPC = "http://127.0.0.1:9944"


def rpc(m, p=None, timeout=10):
    req = urllib.request.Request(
        RPC, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m,
                              "params": p or []}).encode(),
        headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout)).get("result")


def harvest(blocks, cache):
    if cache and os.path.exists(cache):
        d = json.load(open(cache))
        if len(d) >= blocks:
            return d[-blocks:]
    head = int(rpc("chain_getHeader")["number"], 16)
    out = []
    n = head
    stop = max(1, head - blocks)
    while n > stop:
        bh = rpc("chain_getBlockHash", [n])
        ts = rpc("state_getStorage", [TS, bh]) if bh else None
        df = rpc("state_getStorage", [DF, bh]) if bh else None
        if not bh or ts in (None, "0x") or df in (None, "0x"):
            break
        out.append({"number": n,
                    "ts_ms": int.from_bytes(bytes.fromhex(ts[2:]), 'little'),
                    "target": int.from_bytes(bytes.fromhex(df[2:]), 'little')})
        n -= 1
    out.reverse()
    if cache:
        json.dump(out, open(cache, "w"))
    return out


def pct(v, p):
    s = sorted(v)
    k = (len(s) - 1) * p / 100.0
    lo, hi = int(k), min(int(k) + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", default="/tmp/gpnbt/harvest.json")
    ap.add_argument("--blocks", type=int, default=2500)
    a = ap.parse_args()

    d = harvest(a.blocks, a.cache)
    steps = []
    for x, y in zip(d, d[1:]):
        if y["number"] != x["number"] + 1:
            continue
        if y["target"] != x["target"]:
            steps.append(y["target"] / x["target"])
    if not steps:
        raise SystemExit("no retarget events in the harvest")

    n = len(steps)
    print("## Retarget steps actually applied (measured, not modelled)\n")
    print("%d retarget events over %d blocks (one per ~%.0f blocks).\n"
          % (n, len(d), len(d) / n))
    print("| metric | value |")
    print("|---|---|")
    print("| smallest step | %.3f (−%.0f %%) |" % (min(steps), 100 * (1 - min(steps))))
    print("| p5 | %.3f |" % pct(steps, 5))
    print("| median | %.3f |" % statistics.median(steps))
    print("| p95 | %.3f |" % pct(steps, 95))
    print("| largest step | %.3f (+%.0f %%) |" % (max(steps), 100 * (max(steps) - 1)))
    print("| mean abs step | **%.1f %%** |"
          % (100 * statistics.mean([abs(s - 1) for s in steps])))
    print("| steps beyond ±12 %% | **%d of %d (%.0f %%)** |"
          % (sum(1 for s in steps if abs(s - 1) > 0.12), n,
             100.0 * sum(1 for s in steps if abs(s - 1) > 0.12) / n))
    print("| steps beyond ±25 %% | %d of %d (%.0f %%) |"
          % (sum(1 for s in steps if abs(s - 1) > 0.25), n,
             100.0 * sum(1 for s in steps if abs(s - 1) > 0.25) / n))
    print()
    print("A ±12 %% clamp would have acted on **%d of %d** windows. The loop is "
          "not gently steering the cadence: it is setting a new difficulty by a "
          "median-absolute %.0f %% — and up to %.0f %% — every ten blocks, which "
          "at a 15 s target is every ~2.5 minutes."
          % (sum(1 for s in steps if abs(s - 1) > 0.12), n,
             100 * statistics.mean([abs(s - 1) for s in steps]),
             100 * max(abs(s - 1) for s in steps)))


if __name__ == "__main__":
    main()
