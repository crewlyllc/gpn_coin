#!/usr/bin/env python3
"""Cadence gate for rows 217/218 — asserts the acceptance wording decided 2026-11-20.

Acceptance (docs/NEEDS-TRIAGE.md, item 217, assistant decision A'):
  over a 100-block window on the live testnet, using each block's *authored*
  `Timestamp::Now` (chain time, not our import time):
    1. no inter-block gap exceeds 10 x the window mean, AND
    2. no inter-block gap exceeds 45 s at all.

Both clauses must hold; a silent multi-minute hashing stop (row 218) fails (1) even when
the window mean is inflated by the stop itself, and fails (2) unconditionally.

Exit codes: 0 pass, 1 fail (with the offending gaps printed), 2 RPC/collection error.

Usage:
  scripts/check-block-cadence.py                  # last 100 blocks from the local node
  scripts/check-block-cadence.py --blocks 100 --rpc http://127.0.0.1:9944
  scripts/check-block-cadence.py --json out.json   # also write the raw sample for evidence
"""
import argparse
import json
import struct
import statistics
import sys
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
    b = n
    i = 0
    while i + 8 <= n:
        k = int.from_bytes(data[i:i + 8], 'little')
        k = (k * P2) & M64
        k = rotl(k, 31)
        k = (k * P1) & M64
        b ^= k
        b = ((rotl(b, 27) * P1) + P4) & M64
        i += 8
    while i < n:
        b ^= (data[i] * P5) & M64
        b = (rotl(b, 11) * P1) & M64
        i += 1
    b ^= b >> 33
    b = (b * P2) & M64
    b ^= b >> 29
    b = (b * P3) & M64
    b ^= b >> 32
    return b


def twox128(d):
    return struct.pack('<Q', _xxh64(d, 0)) + struct.pack('<Q', _xxh64(d, 1))


TS_KEY = "0x" + twox128(b"Timestamp").hex() + twox128(b"Now").hex()

RPC = "http://127.0.0.1:9944"


def rpc(method, params=None, timeout=10):
    req = urllib.request.Request(
        RPC,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                         "params": params or []}).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.load(urllib.request.urlopen(req, timeout=timeout)).get("result")


def block_timestamp_ms(block_hash):
    """pallet_timestamp::Now as set by the block itself: the first extrinsic is
    `timestamp.set(now)` (pallet 40, call 5, era/immortal header, then a compact u64).
    Reading it from the block body is version-independent; `state_getStorage` on
    TS_KEY returned None for every block on spec 52, so it is no longer relied on."""
    b = rpc("chain_getBlock", [block_hash])
    exs = (b or {}).get("block", {}).get("extrinsics", [])
    if not exs:
        return None
    ex = bytes.fromhex(exs[0][2:]) if isinstance(exs[0], str) else bytes(exs[0])
    i = 4  # pallet index, call index, era/immortal + sign bit, then compact
    if len(ex) <= i:
        return None
    b0 = ex[i]
    mode = b0 & 3
    if mode == 0:
        return b0 >> 2
    if mode == 1:
        if len(ex) < i + 2:
            return None
        return int.from_bytes(ex[i:i + 2], 'little') >> 2
    if mode == 2:
        if len(ex) < i + 4:
            return None
        return int.from_bytes(ex[i:i + 4], 'little') >> 2
    n = (b0 >> 2) + 4
    if len(ex) < i + 1 + n:
        return None
    return int.from_bytes(ex[i + 1:i + 1 + n], 'little')


def main():
    global RPC
    ap = argparse.ArgumentParser()
    ap.add_argument("--blocks", type=int, default=100, help="window size (default 100)")
    ap.add_argument("--rpc", default=RPC)
    ap.add_argument("--max-gap", type=float, default=45.0, help="absolute gap ceiling (s)")
    ap.add_argument("--max-ratio", type=float, default=10.0, help="gap / window-mean ceiling")
    ap.add_argument("--json", default=None, help="write the raw sample here")
    args = ap.parse_args()
    RPC = args.rpc

    try:
        head = rpc("chain_getHeader")
        head_num = int(head["number"], 16)
        first = max(1, head_num - args.blocks + 1)
        rows = []
        for num in range(first, head_num + 1):
            bh = rpc("chain_getBlockHash", [num])
            if not bh:
                continue
            ms = block_timestamp_ms(bh)
            if ms is None:
                continue
            rows.append({"number": num, "hash": bh, "ts": ms / 1000.0})
    except Exception as e:
        print(f"ERROR: could not collect blocks from {RPC}: {e}", file=sys.stderr)
        return 2

    if len(rows) < 10:
        print(f"ERROR: only {len(rows)} blocks collected, need at least 10", file=sys.stderr)
        return 2

    gaps = []
    for a, b in zip(rows, rows[1:]):
        gaps.append({"from": a["number"], "to": b["number"],
                     "gap": round(b["ts"] - a["ts"], 2)})

    mean = statistics.fmean(g["gap"] for g in gaps)
    mx = max(gaps, key=lambda g: g["gap"])
    over_abs = [g for g in gaps if g["gap"] > args.max_gap]
    over_ratio = [g for g in gaps if g["gap"] > args.max_ratio * mean]
    median = statistics.median(g["gap"] for g in gaps)

    print(f"window          : blocks {rows[0]['number']}..{rows[-1]['number']} "
          f"({len(rows)} blocks, {len(gaps)} gaps)")
    print(f"authored span   : {rows[-1]['ts'] - rows[0]['ts']:.1f} s "
          f"({(rows[-1]['ts'] - rows[0]['ts']) / 3600:.2f} h)")
    print(f"mean gap        : {mean:.2f} s   median {median:.2f} s")
    print(f"max gap         : {mx['gap']:.2f} s at #{mx['from']} -> #{mx['to']}")
    print(f"clause 1 (<= {args.max_ratio:g}x mean = {args.max_ratio * mean:.1f} s): "
          f"{len(over_ratio)} violation(s)")
    for g in over_ratio:
        print(f"   FAIL #{g['from']} -> #{g['to']}  {g['gap']:.2f} s "
              f"({g['gap'] / mean:.2f}x mean)")
    print(f"clause 2 (<= {args.max_gap:g} s absolute): {len(over_abs)} violation(s)")
    for g in over_abs:
        print(f"   FAIL #{g['from']} -> #{g['to']}  {g['gap']:.2f} s")

    if args.json:
        with open(args.json, "w") as f:
            json.dump({"rpc": RPC, "blocks": rows, "gaps": gaps,
                       "mean": mean, "max_gap": mx["gap"]}, f, indent=1)
        print(f"sample written  : {args.json}")

    if over_ratio or over_abs:
        print("\nRESULT: FAIL — cadence acceptance (217 restated, 2026-11-20) is not met")
        return 1
    print("\nRESULT: PASS — cadence acceptance (217 restated, 2026-11-20) is met")
    return 0


if __name__ == "__main__":
    sys.exit(main())
