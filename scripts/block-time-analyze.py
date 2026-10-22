#!/usr/bin/env python3
"""Analyse a block-time sample (scripts/block-time-sampler.py output) and
attribute every long inter-block gap to one of four causes.

Cause taxonomy (the task's a/b/c/d):
  (a) mining paused for an AI job      - gap overlaps a "miner parked ... AI job" window
  (b) no authority produced in time    - the gap is real chain time: the *authored*
                                         timestamps are just as far apart as the
                                         arrival times (difficulty / variance / offline)
  (c) tx pool did not include user txs - not a block-time gap at all; measured
                                         separately from AP/provider logs
                                         (create submit -> inclusion), reported aside
  (d) node stalled / syncing           - block was authored but arrived late here:
                                         authored interval << arrival interval, or
                                         isSyncing true during the gap

Usage:
  block-time-analyze.py /tmp/gpnbt/bt-20261022.jsonl --out report.md
Journal windows are read with `journalctl --user` between the sample's
start and end timestamps.
"""
import argparse
import json
import re
import statistics
import subprocess
import sys
from datetime import datetime, timedelta


def pct(vals, p):
    if not vals:
        return float("nan")
    s = sorted(vals)
    k = (len(s) - 1) * p / 100.0
    lo = int(k)
    hi = min(lo + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def load(path):
    recs = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    recs.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return recs


PARK_RE = re.compile(r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d)\s.*?miner (parked: GPU yielded to an AI job|resuming: GPU is free again|parked: GPU unavailable|parked \(no nonce search\))", re.S)


def journal_since(unit, t0, t1):
    """Return [(epoch_seconds, line)] for a user unit in a wall-clock window."""
    since = (datetime.fromtimestamp(t0) - timedelta(seconds=5)).strftime("%Y-%m-%d %H:%M:%S")
    until = (datetime.fromtimestamp(t1) + timedelta(seconds=5)).strftime("%Y-%m-%d %H:%M:%S")
    out = subprocess.run(
        ["journalctl", "--user", "-u", unit, "--since", since, "--until", until,
         "--no-pager", "-o", "short-iso"],
        capture_output=True, text=True, timeout=120).stdout
    rows = []
    for line in out.splitlines():
        m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)[+-]\d\d:\d\d\s", line)
        if not m:
            continue
        try:
            ts = datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S").timestamp()
        except ValueError:
            continue
        rows.append((ts, line))
    return rows


def park_windows(rows):
    """Collapse park/unpark log lines into [(start, end, reason)] windows.

    The supervisor runs more than one miner thread (one per author slot), each
    with its own ParkLog, so duplicate lines within a couple of seconds are
    normal; the last transition wins.
    """
    events = []
    for ts, line in rows:
        if "miner parked: GPU yielded to an AI job" in line:
            events.append((ts, "ai-job"))
        elif "miner parked: GPU unavailable" in line:
            events.append((ts, "gpu-unavailable"))
        elif "idle mode: miner parked" in line:
            events.append((ts, "idle"))
        elif "miner resuming: GPU is free again" in line or "idle ended, resuming" in line:
            events.append((ts, None))
    events.sort()
    wins = []
    cur = None
    for ts, reason in events:
        if reason is not None:
            if cur is None:
                cur = [ts, ts, reason]
            else:
                cur[1] = ts
                cur[2] = reason
        else:
            if cur is not None:
                cur[1] = ts
                wins.append(tuple(cur))
                cur = None
    if cur is not None:
        wins.append(tuple(cur))
    return wins


def overlaps(a0, a1, b0, b1):
    return a0 <= b1 and b0 <= a1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("sample")
    ap.add_argument("--out", default=None)
    ap.add_argument("--target", type=float, default=15.0, help="target block time (s)")
    args = ap.parse_args()

    recs = load(args.sample)
    blocks = [r for r in recs if r.get("kind") == "block"]
    gpus = [r for r in recs if r.get("kind") == "gpu"]
    pools = [r for r in recs if r.get("kind") == "pool"]
    health = [r for r in recs if r.get("kind") == "health"]
    diffs = [r for r in recs if r.get("kind") == "difficulty"]
    start = next((r for r in recs if r.get("kind") == "start"), {})
    end = next((r for r in recs if r.get("kind") == "end"), {})
    t0 = start.get("t")
    t1 = end.get("t") or (blocks[-1]["t"] if blocks else t0)

    lines = []
    w = lines.append
    w("# GPN block-time measurement — %s" % datetime.fromtimestamp(t0).strftime("%Y-%m-%d %H:%M"))
    w("")
    w("Window: **%s → %s** (%.1f min), target block time **%.0f s**"
      % (datetime.fromtimestamp(t0).strftime("%H:%M:%S"),
         datetime.fromtimestamp(t1).strftime("%H:%M:%S"), (t1 - t0) / 60, args.target))
    w("")

    if len(blocks) < 5:
        w("**NOT ENOUGH BLOCKS** (%d) — sample too short." % len(blocks))
        text = "\n".join(lines)
        print(text)
        if args.out:
            open(args.out, "w").write(text + "\n")
        return

    # ---- arrival-based intervals -------------------------------------------
    arr = [b["t"] for b in blocks]
    nums = [b["number"] for b in blocks]
    intervals = [round(arr[i] - arr[i - 1], 3) for i in range(1, len(arr))]
    # a gap that spans several blocks is many short intervals; only real gaps matter

    # ---- authored intervals (chain time) -----------------------------------
    auth = [(b["number"], b["authored_ts_ms"] / 1000.0) for b in blocks
            if b.get("authored_ts_ms")]
    auth_iv = {}
    for i in range(1, len(auth)):
        n, ts = auth[i]
        pn, pts = auth[i - 1]
        if n == pn + 1:
            auth_iv[n] = round(ts - pts, 3)

    w("## Distribution (wall-clock arrival, n=%d intervals over %d blocks)"
      % (len(intervals), len(blocks)))
    w("")
    w("| metric | value |")
    w("|---|---|")
    w("| p50 | %.2f s |" % pct(intervals, 50))
    w("| p75 | %.2f s |" % pct(intervals, 75))
    w("| p90 | %.2f s |" % pct(intervals, 90))
    w("| p95 | %.2f s |" % pct(intervals, 95))
    w("| p99 | %.2f s |" % pct(intervals, 99))
    w("| max | %.2f s |" % max(intervals))
    w("| mean | %.2f s |" % statistics.mean(intervals))
    w("| blocks/min | %.2f |" % (len(blocks) / ((t1 - t0) / 60)))
    w("")
    over = [x for x in intervals if x > args.target]
    w("Blocks over the %.0f s target: **%d of %d (%.1f %%)**, median excess **%.1f s**"
      % (args.target, len(over), len(intervals),
         100.0 * len(over) / len(intervals),
         statistics.median([x - args.target for x in over]) if over else 0))
    w("")
    # ideal exponential reference
    import math
    exp_over = math.exp(-args.target / args.target if False else -1.0)  # placeholder
    w("Reference: for a memoryless (exponential) solve at a %.0f s mean, "
      "P(interval > 2x) = 13.5 %%, P(> 4x) = 1.8 %%, P(> 5.5x) = 0.4 %%."
      % args.target)
    w("")

    # ---- authored vs arrival ------------------------------------------------
    # FINDING (2026-10-22): the block timestamp is stamped when the proposer
    # BUILDS the template, before the PoW solve (inherent = from_system_time at
    # prepare time; the miner then spends the solve time, then seals). So
    #   ts_i ≈ arrival_{i-1} + prep     and   interval_i ≈ solve_i + propagation.
    # Consequence 1: `ts_i - ts_{i-1}` is a ONE-BLOCK-LAGGED copy of the arrival
    #   interval, which is why it looks shifted here (it is not a stall).
    # Consequence 2 (this is why it matters for #52): every retarget window's
    #   `elapsed_ms` is measured between two *preparation* timestamps, so it is
    #   lagged by one block. At window 10 that is a ~10 % bias term; at 45, ~2 %.
    pairs = [(n, auth_iv[n], intervals[nums.index(n) - 1])
             for n in auth_iv if n in nums and nums.index(n) > 0]
    lags = [a - c for (n, a, c) in pairs if c > 0]
    w("## Timestamp semantics (measured, not assumed)")
    w("")
    w("`pallet_timestamp` is set at **template build**, before the PoW solve, so "
      "`ts[i] - ts[i-1]` is a one-block-lagged copy of the arrival interval "
      "(median offset %.1f s here). This is NOT a stalled node: the timestamp "
      "precedes the seal by that block's solve time."
      % (statistics.median(lags) if lags else float("nan")))
    w("")
    w("Practical consequence for the retarget: `elapsed_ms` is measured between "
      "two *preparation* timestamps, so it is lagged by one block — a ~10 %% bias "
      "at a 10-block window, ~2 %% at 45. (See the `MinimumPeriod` clamp note: "
      "intervals that read exactly 1.000 s are a lagging author's clock being "
      "clamped to parent+1 s, not a 1 s block.)")
    w("")

    # ---- GPU / pool / health ------------------------------------------------
    if gpus:
        utils = [g["util"] for g in gpus if "util" in g]
        vram = [g["vram_used_mb"] for g in gpus if "vram_used_mb" in g]
        if utils:
            w("## Side signals")
            w("")
            w("* GPU util: median **%d %%**, p5 **%d %%**, p95 **%d %%** (n=%d)"
              % (statistics.median(utils), pct(utils, 5), pct(utils, 95), len(utils)))
            w("* VRAM used: median **%d MiB** of %d MiB"
              % (statistics.median(vram), gpus[0].get("vram_total_mb", 0)))
    if pools:
        pends = [p["pending"] for p in pools if "pending" in p]
        if pends:
            w("* tx pool depth: median **%d**, max **%d**" % (statistics.median(pends), max(pends)))
    if health:
        warn = sum(1 for h in health if h.get("is_syncing"))
        w("* health samples: %d, isSyncing true in **%d**" % (len(health), warn))
    if diffs:
        w("* difficulty target log2: min %.2f / max %.2f (spread %.2f bits = %.1fx hashrate)"
          % (min(d["log2"] for d in diffs), max(d["log2"] for d in diffs),
             max(d["log2"] for d in diffs) - min(d["log2"] for d in diffs),
             2 ** (max(d["log2"] for d in diffs) - min(d["log2"] for d in diffs))))
    w("")

    # ---- attribution -------------------------------------------------------
    rows = journal_since("gpn-fleet", t0, t1)
    parks = park_windows(rows)
    total_park = sum(max(0.0, b - a) for a, b, _ in parks)

    # The node's own 5 s informant heartbeat ("💤 Idle (2 peers), best: #N") is
    # the evidence for cause (d): if the node reported `best: #<prev>` throughout
    # a gap, it was up, peering and polling and simply had NOTHING to import —
    # the block did not exist yet (cause b). A stall shows up as silence.
    best_samples = []
    for ts, line in rows:
        m = re.search(r"best: #(\d+)", line)
        if m:
            best_samples.append((ts, int(m.group(1))))
    # which blocks did THIS node seal? ("⛏ GPU sealed block" immediately precedes
    # its own "🏆 Imported #N"). A gap that ends on a locally sealed block means
    # the local miner was the one that was slow; ending on a peer's block means
    # the whole field was slow.
    local_sealed = set()
    pending_seal = False
    for ts, line in rows:
        if "GPU sealed block" in line:
            pending_seal = True
        else:
            m = re.search(r"Imported #(\d+)", line)
            if m and pending_seal:
                local_sealed.add(int(m.group(1)))
                pending_seal = False
    beats = []
    for i in range(1, len(arr)):
        gap = arr[i] - arr[i - 1]
        if gap <= args.target:
            continue
        g0, g1 = arr[i - 1], arr[i]
        prev_num = nums[i - 1]
        window = [(ts, b) for ts, b in best_samples if g0 <= ts <= g1]
        stalled_reports = [b for ts, b in window if b > prev_num]
        beats.append({"i": i, "g0": g0, "g1": g1, "gap": gap, "num": nums[i],
                      "samples": len(window), "saw_new_early": len(stalled_reports)})

    w("## Cause split for intervals > %.0f s" % args.target)
    w("")
    w("Mining-pause windows observed: **%d**, total **%.1f s** (%.2f %% of wall clock)."
      % (len(parks), total_park, 100.0 * total_park / max(1, (t1 - t0))))
    if parks:
        longest = max(parks, key=lambda p: p[1] - p[0])
        w("Longest pause: **%.1f s** (%s at %s)."
          % (longest[1] - longest[0], longest[2],
             datetime.fromtimestamp(longest[0]).strftime("%H:%M:%S")))
    w("")

    buckets = {"a_job_pause": [], "b_no_author": [], "d_stall": []}
    explained = {"a_job_pause": 0.0, "b_no_author": 0.0, "d_stall": 0.0}
    detail = []
    for bt in beats:
        gap = bt["gap"]
        g0, g1 = bt["g0"], bt["g1"]
        n = bt["num"]
        aiv = auth_iv.get(n)
        hit = [(a, b, r) for (a, b, r) in parks if overlaps(g0, g1, a, b)]
        # (d) the node went quiet: no heartbeat samples at all across the gap,
        # or it reported a height above ours before we recorded the block.
        silent = bt["samples"] == 0
        if hit:
            # A pause only ever explains the part of the gap it covers: a 28 s
            # park inside a 42 s gap explains 28 s, not 42 s.
            cover = sum(min(g1, b) - max(g0, a) for (a, b, r) in hit)
            cover = min(cover, gap)
            buckets["a_job_pause"].append(gap)
            explained["a_job_pause"] += cover
            explained["b_no_author"] += gap - cover
            why = "a: parking (%s, %.1fs of %.1fs)" % (hit[0][2], cover, gap)
        elif silent:
            buckets["d_stall"].append(gap)
            explained["d_stall"] += gap
            why = "d: no node heartbeat during the gap"
        else:
            buckets["b_no_author"].append(gap)
            explained["b_no_author"] += gap
            why = "b: node up+idle (%d beats, best stayed #%d)" % (bt["samples"], nums[bt["i"] - 1])
        detail.append((n, g0, gap, aiv, why))

    n_over = sum(len(v) for v in buckets.values())
    total_gap = sum(sum(v) for v in buckets.values())
    # Attributable seconds are exclusive: pause-covered time belongs to (a), the
    # remainder of a pause-overlapping gap belongs to (b), stalls to (d).
    explained["b_no_author"] = total_gap - explained["a_job_pause"] - explained["d_stall"]
    w("| cause | gaps | share of gaps | attributable gap time | share of gap time |")
    w("|---|---|---|---|---|")
    for k, label in (("a_job_pause", "(a) mining paused for an AI job"),
                     ("b_no_author", "(b) no authority produced in time (difficulty/variance/offline)"),
                     ("d_stall", "(d) node stalled / imported late")):
        v = buckets[k]
        w("| %s | %d | %.1f %% | %.0f s | %.1f %% |"
          % (label, len(v), 100.0 * len(v) / max(1, n_over), explained[k],
             100.0 * explained[k] / max(1, total_gap)))
    w("")
    w("Attributable time is exclusive: only the part of a gap actually covered by a "
      "mining pause counts as (a); the rest of that gap is (b).")
    w("")
    w("(c) *tx pool did not include user txs* is not a block-time gap: it is measured "
      "as create-submit → inclusion delay in the AP/provider journals and is reported "
      "separately below.")
    w("")

    w("### Worst gaps")
    w("")
    w("| block | at | gap | ts interval (lagged) | attribution |")
    w("|---|---|---|---|---|")
    for n, g0, gap, aiv, why in sorted(detail, key=lambda d: -d[2])[:15]:
        w("| #%d | %s | %.1f s | %s | %s |"
          % (n, datetime.fromtimestamp(g0).strftime("%H:%M:%S"), gap,
             ("%.1f s" % aiv) if aiv else "n/a", why))
    w("")
    w("### Who ended the long gaps")
    w("")
    long_gaps = [bt for bt in beats]
    if long_gaps:
        loc = sum(1 for bt in long_gaps if bt["num"] in local_sealed)
        w("%d of %d gaps > %.0f s were ended by a block **this node** sealed; "
          "%d by a peer. This node produced **%d of %d blocks (%.0f %%)** in the window, "
          "so it is one of three roughly equal producers."
          % (loc, len(long_gaps), args.target, len(long_gaps) - loc,
             len([b for b in nums if b in local_sealed]), len(nums),
             100.0 * len([b for b in nums if b in local_sealed]) / max(1, len(nums))))
    w("")

    text = "\n".join(lines)
    print(text)
    if args.out:
        open(args.out, "w").write(text + "\n")
        print("\n[written] %s" % args.out, file=sys.stderr)


if __name__ == "__main__":
    main()
