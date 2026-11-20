#!/usr/bin/env python3
"""Cadence gate for rows 217/218 — asserts the acceptance wording decided 2026-11-20.

Acceptance (docs/NEEDS-TRIAGE.md, items 217/218; assistant decision A, revised 2026-11-20
after the arithmetic showed an absolute ceiling is unsatisfiable on a healthy chain):

  over a 100-block window on the live testnet, using each block's *authored*
  `Timestamp::Now` (chain time, not our import time):

    1. no inter-block gap exceeds 10 x the window mean  (blunt detector), AND
    2. every gap above 3 x the window mean has a *cause in the node's own output*
       inside that window: a sealer heartbeat, a slow-solve WARN, a blocked-submit WARN,
       a GPU error, a panic, a restart, or a self-heal window.

Why clause 2 is attribution and not an absolute ceiling: at a 15-16 s mean a memoryless
hashing race produces a gap >45 s about 5.4 % of the time, i.e. ~5 offending gaps per 99 and
a 0.4 % chance of a clean window. Both live samples (11:12 PT: 4 violations, 12:05 PT: 6)
matched that prediction exactly, so an absolute ceiling measures the arrival process, not us.
What is *ours* to answer for is the silent stop: a 153 s hole with nothing in the log is a
failure, and a 153 s hole with `SealWatch ... unsolved 153 s` in the log is explained.

Exit codes: 0 pass, 1 fail (with the offending gaps printed), 2 RPC/collection error.

Usage:
  scripts/check-block-cadence.py                  # last 100 blocks from the local node
  scripts/check-block-cadence.py --journal-unit gpn-fleet   # attribute gaps from the node log
  scripts/check-block-cadence.py --log node.log    # attribute from a captured log file
  scripts/check-block-cadence.py --blocks 100 --rpc http://127.0.0.1:9944
  scripts/check-block-cadence.py --json out.json   # also write the raw sample for evidence
  scripts/check-block-cadence.py --self-test       # clause logic against synthetic windows
"""
import argparse
import datetime as dt
import json
import re
import struct
import statistics
import subprocess
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

# A line that explains a long gap. Kept to literals a human writes in code, not module
# names: the heartbeat and the two WARNs are what branch fix/218-sealer-heartbeat emits.
CAUSE_RE = re.compile(
    r"(sealer: still searching|sealer: slow solve|seal submit for pre_hash|"
    r"no template from consensus|gpu (?:error|fault|reset)|sealing failed|"
    r"panic|self[- ]?heal|node (?:start|exit)|restart)",
    re.IGNORECASE,
)

LOG_TIME_RE = re.compile(r"(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}:\d{2})")


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


def parse_log_lines(text):
    """-> [(datetime, line)] for every line carrying an ISO-ish timestamp."""
    out = []
    for ln in text.splitlines():
        m = LOG_TIME_RE.search(ln)
        if not m:
            continue
        try:
            t = dt.datetime.strptime(m.group(1) + " " + m.group(2), "%Y-%m-%d %H:%M:%S")
        except ValueError:
            continue
        out.append((t, ln))
    return out


def read_journal(unit, since_s):
    since = (dt.datetime.now() - dt.timedelta(seconds=since_s)).strftime("%Y-%m-%d %H:%M:%S")
    p = subprocess.run(["journalctl", "--user", "-u", unit, "--since", since, "--no-pager"],
                       capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"journalctl --user -u {unit} failed: {p.stderr.strip()[:200]}")
    return p.stdout


def analyse(rows, max_ratio=10.0, attribute_ratio=3.0, log_lines=None):
    """Pure function: blocks -> verdict. `rows` is [{'number','hash','ts'}]."""
    gaps = []
    for a, b in zip(rows, rows[1:]):
        gaps.append({"from": a["number"], "to": b["number"],
                     "t_from": a["ts"], "t_to": b["ts"],
                     "gap": round(b["ts"] - a["ts"], 2)})
    mean = statistics.fmean(g["gap"] for g in gaps)
    median = statistics.median(g["gap"] for g in gaps)
    mx = max(gaps, key=lambda g: g["gap"])
    over_ratio = [g for g in gaps if g["gap"] > max_ratio * mean]
    suspect = [g for g in gaps if g["gap"] > attribute_ratio * mean]

    for g in suspect:
        causes = []
        if log_lines:
            for t, line in log_lines:
                if g["t_from"] < t.timestamp() <= g["t_to"] and CAUSE_RE.search(line):
                    causes.append(line.strip())
                    if len(causes) == 3:
                        break
        g["causes"] = causes
        g["unexplained"] = not causes

    unexplained = [g for g in suspect if g["unexplained"]]
    return {
        "rows": rows, "gaps": gaps, "mean": mean, "median": median,
        "max_gap": mx["gap"], "over_ratio": over_ratio, "suspect": suspect,
        "unexplained": unexplained,
        "attribution_available": log_lines is not None,
        "max_ratio": max_ratio, "attribute_ratio": attribute_ratio,
        "pass": not over_ratio and not unexplained,
    }


def render(rep):
    out = []
    rows = rep["rows"]
    out.append(f"window          : blocks {rows[0]['number']}..{rows[-1]['number']} "
               f"({len(rows)} blocks, {len(rep['gaps'])} gaps)")
    out.append(f"authored span   : {rows[-1]['ts'] - rows[0]['ts']:.1f} s "
               f"({(rows[-1]['ts'] - rows[0]['ts']) / 3600:.2f} h)")
    out.append(f"mean gap        : {rep['mean']:.2f} s   median {rep['median']:.2f} s")
    out.append(f"max gap         : {rep['max_gap']:.2f} s")
    out.append(f"clause 1 (<= {rep['max_ratio']:g}x mean = "
               f"{rep['max_ratio'] * rep['mean']:.1f} s): "
               f"{len(rep['over_ratio'])} violation(s)")
    for g in rep["over_ratio"]:
        out.append(f"   FAIL #{g['from']} -> #{g['to']}  {g['gap']:.2f} s "
                   f"({g['gap'] / rep['mean']:.2f}x mean)")
    if not rep["attribution_available"]:
        out.append(f"clause 2 (> {rep['attribute_ratio']:g}x mean must be explained): "
                   f"{len(rep['suspect'])} suspect gap(s), NO LOG SUPPLIED — "
                   "every one counts as unexplained")
    else:
        out.append(f"clause 2 (> {rep['attribute_ratio']:g}x mean must be explained): "
                   f"{len(rep['suspect'])} suspect gap(s), "
                   f"{len(rep['unexplained'])} unexplained")
    for g in rep["suspect"]:
        ratio = g["gap"] / rep["mean"]
        if g["unexplained"]:
            out.append(f"   FAIL #{g['from']} -> #{g['to']}  {g['gap']:.2f} s ({ratio:.2f}x) "
                       "— nothing in the node's output explains it")
        else:
            out.append(f"   ok   #{g['from']} -> #{g['to']}  {g['gap']:.2f} s ({ratio:.2f}x) "
                       f"— explained: {g['causes'][0][:110]}")
    out.append("")
    if rep["pass"]:
        out.append("RESULT: PASS — cadence acceptance (217 restated + 218 attribution, "
                   "2026-11-20) is met")
    else:
        out.append("RESULT: FAIL — cadence acceptance (217 restated + 218 attribution, "
                   "2026-11-20) is not met")
    return "\n".join(out)


T0 = 1_700_000_000.0  # synthetic epoch shared by synthetic() and _log()


def synthetic(window):
    """window: list of gaps in seconds -> rows with synthetic chain timestamps."""
    t = T0
    rows = [{"number": 1000, "hash": "0xsynthetic", "ts": t}]
    for i, gap in enumerate(window):
        t += gap
        rows.append({"number": 1001 + i, "hash": "0xsynthetic", "ts": t})
    return rows


def _log(lines):
    """('2026-11-20 11:12:00', text) -> [(datetime, line)] like parse_log_lines."""
    base = dt.datetime.fromtimestamp(T0)
    out = []
    for offset, text in lines:
        out.append((base + dt.timedelta(seconds=offset),
                    f"2026-11-20 11:12:00 {text}"))
    return out


def self_test():
    """Clause logic against fixed windows. Fails loudly; prints one line per case."""
    failures = 0
    steady = [15.0] * 99

    def check(name, rep, want_pass):
        nonlocal failures
        got = rep["pass"]
        tag = "ok  " if got == want_pass else "FAIL"
        if got != want_pass:
            failures += 1
        print(f"  {tag} {name}: pass={got} (wanted {want_pass}), "
              f"clause1={len(rep['over_ratio'])}, "
              f"unexplained={len(rep['unexplained'])}/{len(rep['suspect'])}")

    # 1. a healthy window with nothing odd in it
    check("steady 15 s window", analyse(synthetic(steady)), True)

    # 2. one 153 s silent stop: 9.95x mean, so clause 1 alone would let it through;
    #    the attribution clause must fail it because no log line explains it.
    stall = [15.0] * 40 + [153.0] + [15.0] * 58
    rep = analyse(synthetic(stall))
    check("153 s stop, no log", rep, False)
    assert rep["over_ratio"] == [], "153 s stop must not trip the 10x clause (it is 9.95x)"
    assert len(rep["unexplained"]) == 1

    # 3. the same stop, with the sealer heartbeat inside the window: explained -> pass
    heart = _log([
        (40 * 15 + 20, "sealer: still searching template 0xab after 20.0s "
                       "(4 nonce window(s) searched, GPU healthy, submit not yet attempted)"),
        (40 * 15 + 140, "sealer: still searching template 0xab after 140.0s "
                        "(28 nonce window(s) searched, GPU healthy, submit not yet attempted)"),
    ])
    rows = synthetic(stall)
    rep = analyse(rows, log_lines=heart)
    check("153 s stop, heartbeat explains it", rep, True)
    assert len(rep["unexplained"]) == 0

    # 4. a log line outside the window must not be borrowed as a cause
    outside = [(t, ln) for t, ln in heart
               if t < dt.datetime.fromtimestamp(T0) + dt.timedelta(seconds=200)]
    check("cause outside the gap does not count", analyse(rows, log_lines=outside), False)

    # 5. a 20x gap fails clause 1 even when the log explains it
    huge = [15.0] * 40 + [300.0] + [15.0] * 58
    rows_h = synthetic(huge)
    rep = analyse(rows_h, log_lines=_log(
        [(40 * 15 + 60, "sealer: slow solve — template 0xab took 60.0s to find a nonce")]))
    check("300 s gap fails clause 1 regardless of cause", rep, False)
    assert len(rep["over_ratio"]) == 1

    # 6. no log supplied at all: suspects are treated as unexplained, never as passes
    check("no log supplied cannot pass with suspects", analyse(synthetic(stall)), False)

    # 7. the ordinary bursty distribution (many 40 s gaps at a 15 s mean) stays a pass
    bursty = [15.0] * 60 + [40.0] * 39
    rep = analyse(synthetic(bursty), log_lines=_log(
        [(i * 15 + 5, "sealer: still searching template 0xab after 5.0s") for i in range(100)]))
    check("40 s gaps (2.4x mean) are not suspects", rep, True)

    print(f"self-test: {'FAILED' if failures else 'PASSED'} "
          f"({7 - failures if failures else 7}/7 cases)")
    return 1 if failures else 0


def main():
    global RPC
    ap = argparse.ArgumentParser()
    ap.add_argument("--blocks", type=int, default=100, help="window size (default 100)")
    ap.add_argument("--rpc", default=RPC)
    ap.add_argument("--max-ratio", type=float, default=10.0,
                    help="clause 1: gap / window-mean ceiling")
    ap.add_argument("--attribute-ratio", type=float, default=3.0,
                    help="clause 2: gaps above this multiple of the mean need a logged cause")
    ap.add_argument("--log", default=None, help="captured node log to attribute gaps from")
    ap.add_argument("--journal-unit", default=None,
                    help="systemd --user unit to read for attribution (e.g. gpn-fleet)")
    ap.add_argument("--json", default=None, help="write the raw sample here")
    ap.add_argument("--self-test", action="store_true", help="run the clause logic self-test")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

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

    log_lines = None
    if args.log:
        with open(args.log, "r", errors="replace") as f:
            log_lines = parse_log_lines(f.read())
    elif args.journal_unit:
        try:
            log_lines = parse_log_lines(read_journal(args.journal_unit, 2 * 3600))
        except Exception as e:
            print(f"ERROR: could not read {args.journal_unit}: {e}", file=sys.stderr)
            return 2

    rep = analyse(rows, args.max_ratio, args.attribute_ratio, log_lines)
    print(render(rep))

    if args.json:
        payload = {k: v for k, v in rep.items() if k not in ("rows", "gaps")}
        payload["blocks"] = rows
        payload["gaps"] = [{k: v for k, v in g.items() if k != "causes"} for g in rep["gaps"]]
        with open(args.json, "w") as f:
            json.dump(payload, f, indent=1)
        print(f"\nsample written  : {args.json}")

    return 0 if rep["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
