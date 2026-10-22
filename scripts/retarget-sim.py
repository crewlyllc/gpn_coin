#!/usr/bin/env python3
"""Before/after simulation of the GPN PoW difficulty retarget.

WHAT IS WRONG TODAY (measured, change-log #52):
    new/stored = 2x/(1+x),   x = window_mean_interval / 15 s,  window = 10, no clamp.
That law is NOT symmetric under a hashrate error of the same magnitude in each
direction:
    x = 2.0 -> 1.333   (a 2x-too-slow window relaxes the target by only +33 %)
    x = 0.5 -> 0.667   (a 2x-too-fast window HARDENS it by -33 %)
and, worse, it is unbounded below: x -> 0 gives r -> 0, so a burst of fast
solves can collapse the target in one window (observed 85.51B -> 42.27B, -50.6 %
on a 4.9 s window mean). Recovery is capped at r < 2 per window and only runs
once per window, so the chain ratchets tight and then crawls: 86-96 s block
tails behind a 15 s target. Because a `create` tx can only ride the NEXT block,
that tail is directly the AI request latency.

THE CANDIDATES SIMULATED HERE
  current    r = 2x/(1+x),                       window 10, no clamp
  clamp-x    r = clamp(x, lo, hi),               symmetric in log space (hi = 1/lo)
  clamp-r    r = clamp(2x/(1+x), 1-p, 1+p),      symmetric in linear space
  sqrt-damp  r = sqrt(x)  (geometric-mean controller: halves the correction)
plus a window sweep (5 / 10 / 20 / 45) for the winner.

METHOD (no chain writes, nothing deployed)
 1. Harvest N blocks: hash -> pallet_timestamp::Now, GPUPoW::Difficulty.
 2. interval_i = authored ts spacing; target_i = difficulty in force.
 3. interval_i ~= K * noise_i / target_i, so the *implied hashrate*
    h_i = target_i / interval_i is a controller-free draw whose scatter is the
    memoryless-PoW solve noise. Replay {h_i} (normalised) through each candidate
    controller. Paired: every candidate sees the same draws.
 4. Adversarial synthetic patterns exercise what 2 500 real blocks may not
    contain: burst->slow oscillation, a lone 5x sealer pause, and a genuine 2x
    hashrate step (which must still be *tracked*, not merely damped).

Usage:
  scripts/retarget-sim.py --blocks 2500 --out <report.md>
"""
import argparse
import json
import math
import os
import random
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
RPC = os.environ.get("GPN_RPC", "http://127.0.0.1:9944")


def rpc(m, p=None, timeout=10):
    req = urllib.request.Request(
        RPC, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m,
                              "params": p or []}).encode(),
        headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout)).get("result")


def harvest(blocks, cache):
    if cache and os.path.exists(cache):
        try:
            d = json.load(open(cache))
            if len(d) >= blocks:
                return d[-blocks:]
        except (json.JSONDecodeError, OSError):
            pass
    head = int(rpc("chain_getHeader")["number"], 16)
    out = []
    n = head
    stop = max(1, head - blocks)
    while n > stop:
        bh = rpc("chain_getBlockHash", [n])
        if not bh:
            break
        ts = rpc("state_getStorage", [TS, bh])
        df = rpc("state_getStorage", [DF, bh])
        if ts in (None, "0x") or df in (None, "0x"):
            break
        out.append({"number": n,
                    "ts_ms": int.from_bytes(bytes.fromhex(ts[2:]), 'little'),
                    "target": int.from_bytes(bytes.fromhex(df[2:]), 'little')})
        n -= 1
    out.reverse()
    if cache:
        os.makedirs(os.path.dirname(cache), exist_ok=True)
        json.dump(out, open(cache, "w"))
    return out


def pct(v, p):
    s = sorted(v)
    if not s:
        return float("nan")
    k = (len(s) - 1) * p / 100.0
    lo = int(k)
    hi = min(lo + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


# ---------------------------------------------------------------- laws


def law_current(x, window=10, clamp_x=None, clamp_r=None):
    r = 2.0 * x / (1.0 + x)
    if clamp_r is not None:
        lo, hi = 1.0 - clamp_r, 1.0 + clamp_r
        r = min(hi, max(lo, r))
    return r


def law_clamp_x(x, window=10, clamp_x=(0.5, 2.0), clamp_r=None):
    lo, hi = clamp_x
    return min(hi, max(lo, x))


def law_slew(x, window=10, clamp_x=(0.5, 2.0), clamp_r=None, pct=133.0):
    """The implemented law: clamp(actual/target, 100/p, p/100) with exact
    reciprocal bounds, so up-step x down-step == 1 and the loop cannot ratchet."""
    hi = pct / 100.0
    return min(hi, max(1.0 / hi, x))


def law_sqrt(x, window=10, clamp_x=(0.5, 2.0), clamp_r=None):
    return math.sqrt(x)


CANDIDATES = [
    # label,                         law,          window, clamp_x, clamp_r
    ("current: 2x/(1+x), w10, no clamp", law_current, 10, None, None),
    ("clamp-x [0.5, 2.0], w5", law_clamp_x, 5, (0.5, 2.0), None),
    ("clamp-x [0.5, 2.0], w10", law_clamp_x, 10, (0.5, 2.0), None),
    ("clamp-x [0.5, 2.0], w20", law_clamp_x, 20, (0.5, 2.0), None),
    ("clamp-x [0.5, 2.0], w30", law_clamp_x, 30, (0.5, 2.0), None),
    ("clamp-x [0.5, 2.0], w45", law_clamp_x, 45, (0.5, 2.0), None),
    ("slew [3/4, 4/3], w30", law_slew, 30, None, None),
    ("slew [3/4, 4/3], w45 (PROPOSED)", law_slew, 45, None, None),
    ("slew [3/4, 4/3], w60", law_slew, 60, None, None),
    ("clamp-x [0.667, 1.5], w10", law_clamp_x, 10, (2.0 / 3.0, 1.5), None),
    ("clamp-r +/-12 %, w10", law_current, 10, None, 0.12),
    ("clamp-r +/-12 %, w45", law_current, 45, None, 0.12),
    ("sqrt(x) damping, w10", law_sqrt, 10, None, None),
]


def simulate(noise, law, window, clamp_x, clamp_r, target0, block_count,
             drift=None):
    """Replay noise draws through one controller.

    `Difficulty` is a *target*: P(valid hash) = target/2^256, so the expected
    solve time is INVERSE in the target: interval = K * noise / target, with K
    pinned so interval == 15 s at target == target0 and noise == 1.

    `drift(i)` optionally returns a multiplier on solve time (hashrate drop),
    used by the hashrate-step scenario.
    """
    K = 15.0 * float(target0)
    target = float(target0)
    intervals, targets, steps = [], [], []
    blocked = 0
    i = 0
    since = 0
    acc = 0.0
    clamp_hits = 0
    for b in range(block_count):
        g = noise[i % len(noise)]
        if drift is not None:
            g *= drift(b)
        iv = K * g / target
        iv = max(0.3, iv)
        intervals.append(iv)
        targets.append(target)
        acc += iv
        since += 1
        if since >= window:
            x = (acc / since) / 15.0
            r = law(x, window, clamp_x, clamp_r)
            if clamp_x is not None and abs(r - x) > 1e-12:
                clamp_hits += 1
            if clamp_r is not None and abs(r - law_current(x)) > 1e-12:
                clamp_hits += 1
            steps.append(r)
            target *= r
            acc = 0.0
            since = 0
        i += 1
    return intervals, targets, steps, clamp_hits


def recovery_blocks(intervals, from_idx, thresh=18.0, hold=10):
    """Blocks after `from_idx` until the *window-free* 10-block median interval
    is back under `thresh` and stays there for `hold` blocks."""
    n = len(intervals)
    run = 0
    for b in range(from_idx, n):
        lo = max(0, b - 9)
        med = statistics.median(intervals[lo:b + 1])
        if med <= thresh:
            run += 1
            if run >= hold:
                return b - from_idx
        else:
            run = 0
    return None


def table_row(label, its, steps, clamp_hits, windows):
    return ("| %s | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f %% | %.1f %% | %.0f %% | %.0f %% | %.0f %% |"
            % (label, pct(its, 50), pct(its, 90), pct(its, 95), pct(its, 99),
               max(its), statistics.mean(its),
               100.0 * sum(1 for x in its if x > 15) / len(its),
               100.0 * sum(1 for x in its if x > 30) / len(its),
               100.0 * statistics.mean([abs(s - 1) for s in steps]),
               100.0 * max(abs(s - 1) for s in steps),
               100.0 * clamp_hits / max(1, windows)))


HEADER = ("| controller | p50 | p90 | p95 | p99 | max | mean | >15 s | >30 s | "
          "mean abs step | max step | windows clamped |")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--blocks", type=int, default=2500)
    ap.add_argument("--sim-blocks", type=int, default=20000)
    ap.add_argument("--out", default=None)
    ap.add_argument("--cache", default="/tmp/gpnbt/harvest.json")
    args = ap.parse_args()

    recs = harvest(args.blocks, args.cache)
    if len(recs) < 100:
        raise SystemExit("only %d blocks harvested" % len(recs))

    iv, tgt = [], []
    for a, b in zip(recs, recs[1:]):
        if b["number"] != a["number"] + 1:
            continue
        d = (b["ts_ms"] - a["ts_ms"]) / 1000.0
        if d <= 0 or d > 600:
            continue
        iv.append(d)
        tgt.append(b["target"])

    # Controller-free noise: interval_i ≈ target_i / hashrate_i, so
    # interval_i / target_i is a bootstrap sample of 1/hashrate * solve noise.
    # Use it directly (never its reciprocal: unbounded mean diverges).
    med_t = statistics.median(tgt)
    ratio = [d / t for t, d in zip(tgt, iv)]
    mean_ratio = statistics.mean(ratio)
    noise = [r / mean_ratio for r in ratio]

    L = []
    w = L.append
    w("# GPN retarget simulation — real observed intervals + adversarial patterns")
    w("")
    w("Harvested **%d consecutive blocks** (%s → %s), **%d intervals**."
      % (len(recs), recs[0]["number"], recs[-1]["number"], len(iv)))
    w("")
    w("## Observed (ground truth, controller in force: w10, no clamp)")
    w("")
    w("| metric | value |")
    w("|---|---|")
    for label, fn in (("p50", lambda: pct(iv, 50)), ("p90", lambda: pct(iv, 90)),
                      ("p95", lambda: pct(iv, 95)), ("p99", lambda: pct(iv, 99)),
                      ("max", lambda: max(iv)), ("mean", lambda: statistics.mean(iv))):
        w("| %s | %.1f s |" % (label, fn()))
    w("| share > 15 s | %.1f %% |" % (100.0 * sum(1 for x in iv if x > 15) / len(iv)))
    w("| share > 30 s | %.1f %% |" % (100.0 * sum(1 for x in iv if x > 30) / len(iv)))
    w("| share > 60 s | %.1f %% |" % (100.0 * sum(1 for x in iv if x > 60) / len(iv)))
    w("")
    w("Implied solve-noise scatter: **%.1f %%** (CV of interval/target; a single "
      "memoryless draw is 100 %%)." % (100.0 * statistics.pstdev(noise) / statistics.mean(noise)))
    w("")

    base = float(med_t)
    w("## A0. Floor: a PERFECT controller (target pinned at the design point)")
    w("")
    ideal = [15.0 * n for n in noise]
    w("| p50 | p90 | p95 | p99 | max | mean | >15 s | >30 s |")
    w("|---|---|---|---|---|---|---|---|")
    w("| %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f %% | %.1f %% |"
      % (pct(ideal, 50), pct(ideal, 90), pct(ideal, 95), pct(ideal, 99),
         max(ideal), statistics.mean(ideal),
         100.0 * sum(1 for x in ideal if x > 15) / len(ideal),
         100.0 * sum(1 for x in ideal if x > 30) / len(ideal)))
    w("")
    w("No retarget rule can beat this on a 15 s target: it is the observed "
      "solve-time scatter with the difficulty held perfect. **p95 ~48 s and a "
      "~200 s max are the chain's physics, not the controller's fault** — which "
      "is why the SLO has to be stated against this floor, not against 15 s.")
    w("")
    w("## A. Real noise, paired replay (%d blocks per candidate)" % args.sim_blocks)
    w("")
    w(HEADER)
    w("|---|---|---|---|---|---|---|---|---|---|---|---|")
    nw = args.sim_blocks
    for label, law, win, cx, cr in CANDIDATES:
        its, _, steps, hits = simulate(noise, law, win, cx, cr, base, nw)
        w(table_row(label, its, steps, hits, nw // win))
    w("")
    w("`windows clamped` = share of retarget events where the clamp changed the "
      "step. A high number on a clamp-x row means the clamp is doing the work; "
      "on the `current` row the law is unbounded and never clamped.")
    w("")

    # ---- B. burst -> slow oscillation (what #52 actually looked like)
    w("## B. Adversarial: burst → slow (20 blocks at 0.25x, 40 at 3x, repeat)")
    w("")
    burst_noise = [0.25 if (i % 60) < 20 else 3.0 for i in range(600)]
    w(HEADER)
    w("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for label, law, win, cx, cr in CANDIDATES:
        its, _, steps, hits = simulate(burst_noise, law, win, cx, cr, base, 6000)
        w(table_row(label, its, steps, hits, 6000 // win))
    w("")

    # ---- B2. burst period sweep: is the choice phase-locked to one pattern?
    w("## B2. Burst-period sweep (2/3 of blocks at 0.25x, 1/3 at 3x, period = P blocks)")
    w("")
    w("A single synthetic period can accidentally resonate with the window, so the "
      "choice is checked across periods. p95 block time / share of blocks > 30 s:")
    w("")
    w("| controller | P=30 | P=40 | P=60 | P=80 | P=120 |")
    w("|---|---|---|---|---|---|")
    for label, law, win, cx, cr in CANDIDATES:
        cells = []
        for period in (30, 40, 60, 80, 120):
            pat = [0.25 if (i % period) < period * 2 // 3 else 3.0 for i in range(600)]
            its, _, _, _ = simulate(pat, law, win, cx, cr, base, 6000)
            cells.append("%.0f s / %.0f %%" % (pct(its, 95),
                        100.0 * sum(1 for x in its if x > 30) / len(its)))
        w("| %s | %s |" % (label, " | ".join(cells)))
    w("")

    # ---- C. lone sealer pause: 1 in 30 blocks takes 5x
    w("## C. Adversarial: single-sealer pause (1 block in 30 takes 5x)")
    w("")
    pause_noise = [5.0 if i % 30 == 0 else 1.0 for i in range(600)]
    w(HEADER)
    w("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for label, law, win, cx, cr in CANDIDATES:
        its, _, steps, hits = simulate(pause_noise, law, win, cx, cr, base, 6000)
        w(table_row(label, its, steps, hits, 6000 // win))
    w("")

    # ---- D. genuine 2x hashrate drop: must be TRACKED, not damped away
    w("## D. Genuine change: hashrate halves at block 300 (solve time 2x)")
    w("")
    w("| controller | blocks to recover (10-block median ≤ 18 s) | mean interval 300–1000 |")
    w("|---|---|---|")
    rnd_noise = [math.exp(random.Random(7).gauss(0, 0.9)) for _ in range(1000)]
    for label, law, win, cx, cr in CANDIDATES:
        its, _, steps, hits = simulate(
            rnd_noise, law, win, cx, cr, base, 1000,
            drift=lambda b: 2.0 if b >= 300 else 1.0)
        rec = recovery_blocks(its, 300)
        tail = statistics.mean(its[300:1000])
        w("| %s | %s | %.1f s |" % (label, rec if rec is not None else "never", tail))
    w("")
    w("This is the counter-weight to A/B/C: a controller that is *too* damped "
      "scores beautifully on variance and then fails to track a real hashrate "
      "change, which is what the 3-authority fleet does whenever one sealer "
      "drops out. The winning configuration has to be good in both columns.")
    w("")

    text = "\n".join(L)
    print(text)
    if args.out:
        os.makedirs(os.path.dirname(args.out), exist_ok=True)
        open(args.out, "w").write(text + "\n")


if __name__ == "__main__":
    main()
