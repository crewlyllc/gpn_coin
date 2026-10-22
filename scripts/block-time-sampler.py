#!/usr/bin/env python3
"""Sample GPN block production at ~1 s for a window, and log the side signals
needed to attribute long inter-block gaps to a cause.

Records, per 1 s tick:
  * best block number (chain_getHeader) -> arrival time of every new block
  * the block's *authored* timestamp (pallet_timestamp::Now at that block hash)
    so "node was slow to import" can be told apart from "nobody solved it"

And every 5/10/30 s:
  * nvidia-smi GPU util + VRAM
  * author_pendingExtrinsics (tx pool depth)
  * system_health (peers, isSyncing)

Output: JSONL, one record per event. Analysis is a separate pass
(scripts/block-time-analyze.py) which also pulls the journal windows.

Usage: block-time-sampler.py --minutes 45 --out /tmp/bt.jsonl [--rpc http://127.0.0.1:9944]
"""
import argparse
import json
import struct
import subprocess
import time
import urllib.request

M64 = 0xFFFFFFFFFFFFFFFF


def _xxh64(data, seed=0):
    P1 = 0x9E3779B185EBCA87
    P2 = 0xC2B2AE3D27D4EB4F
    P3 = 0x165667B19E3779F9
    P4 = 0x85EBCA77C2B2AE63
    P5 = 0x27D4EB2F165667C5

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


TS_KEY = "0x" + twox128(b"Timestamp").hex() + twox128(b"Now").hex()
DIFF_KEY = "0x" + (twox128(b"GPUPoW") + twox128(b"Difficulty")).hex()

RPC = "http://127.0.0.1:9944"


def rpc(method, params=None, timeout=6):
    req = urllib.request.Request(
        RPC,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                         "params": params or []}).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.load(urllib.request.urlopen(req, timeout=timeout)).get("result")


def gpu():
    try:
        out = subprocess.run(
            ["nvidia-smi", "--query-gpu=utilization.gpu,memory.used,memory.total",
             "--format=csv,noheader,nounits"],
            capture_output=True, text=True, timeout=5).stdout.strip().splitlines()[0]
        u, used, total = [int(x) for x in out.split(",")]
        return {"util": u, "vram_used_mb": used, "vram_total_mb": total}
    except Exception as e:
        return {"error": str(e)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--minutes", type=float, default=45)
    ap.add_argument("--out", default="/tmp/bt-sample.jsonl")
    ap.add_argument("--rpc", default="http://127.0.0.1:9944")
    args = ap.parse_args()
    global RPC
    RPC = args.rpc

    f = open(args.out, "w", buffering=1)
    end = time.time() + args.minutes * 60

    def emit(rec):
        f.write(json.dumps(rec) + "\n")

    emit({"kind": "start", "t": time.time(), "minutes": args.minutes,
          "rpc": RPC, "monotonic": time.monotonic()})

    last_num = None
    next_gpu = next_pool = next_health = 0.0
    next_diff = 0.0
    errors = 0

    while time.time() < end:
        now = time.time()
        try:
            h = rpc("chain_getHeader")
            num = int(h["number"], 16)
            if num != last_num:
                # The block's OWN timestamp = pallet_timestamp::Now in the state
                # AT that block, so we need the block hash (state_getStorage with
                # block 0x<hash>) and must read at the block itself, never at its
                # parent (that would be the previous block's timestamp).
                ts = None
                bh = None
                try:
                    bh = rpc("chain_getBlockHash", [num])
                    v = rpc("state_getStorage", [TS_KEY, bh])
                    if v:
                        ts = int.from_bytes(bytes.fromhex(v[2:]), 'little')
                except Exception:
                    pass
                emit({"kind": "block", "t": now, "number": num,
                      "hash": bh, "parent": h["parentHash"],
                      "authored_ts_ms": ts})
                last_num = num
        except Exception as e:
            errors += 1
            emit({"kind": "error", "t": now, "err": str(e)[:200]})

        if now >= next_gpu:
            emit({"kind": "gpu", "t": now, **gpu()})
            next_gpu = now + 5
        if now >= next_pool:
            try:
                pend = rpc("author_pendingExtrinsics") or []
                emit({"kind": "pool", "t": now, "pending": len(pend)})
            except Exception as e:
                emit({"kind": "pool", "t": now, "err": str(e)[:120]})
            next_pool = now + 10
        if now >= next_health:
            try:
                sh = rpc("system_health")
                peers = rpc("system_peers") or []
                emit({"kind": "health", "t": now,
                      "peers": sh.get("peers"), "is_syncing": sh.get("isSyncing"),
                      "authority_peers": sum(1 for p in peers
                                             if "AUTHORITY" in str(p.get("roles")))})
            except Exception as e:
                emit({"kind": "health", "t": now, "err": str(e)[:120]})
            next_health = now + 30
        if now >= next_diff:
            try:
                dv = rpc("state_getStorage", [DIFF_KEY])
                if dv and dv != "0x":
                    v = int.from_bytes(bytes.fromhex(dv[2:]), 'little')
                    emit({"kind": "difficulty", "t": now, "target": v,
                          "log2": round(__import__("math").log2(v), 3)})
            except Exception:
                pass
            next_diff = now + 60

        time.sleep(max(0.0, 1.0 - (time.time() - now)))

    emit({"kind": "end", "t": time.time(), "rpc_errors": errors})
    f.close()


if __name__ == "__main__":
    main()
