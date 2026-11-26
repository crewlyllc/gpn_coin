#!/usr/bin/env python3
"""Record one day of soak numbers for row 273 (docs/STABILITY-GATE.md).

Writes/replaces today's section in `docs/soak-<start-date>.md` with numbers a
stranger could re-run:

  * height, and the *effective* block interval since the previous record
    (height delta / elapsed time) -- this is the only number that catches a stall
    nobody was watching
  * authored-timestamp p50/p95 over the last --window blocks (cadence detail)
  * peers / isSyncing now, peers seen across the last 24 h of the journal
  * restart count and start times for gpn-fleet.service in the last 24 h
  * update outcomes for the day -- the update unit's own journal (the only honest
    source), with `last-exit.json` reported separately and labelled as a *node*
    exit report, because tests and manual runs write there too (row 281)
  * jobs attempted/succeeded from ~/.gpn/assignments.json + AP :8092/jobs
  * every `Reason:` / ban line the journal emitted in the window
  * box 1 (the second soak machine) via scripts/box1.sh, recorded as
    "unreachable (<exit code>)" rather than silently omitted

Idempotent per date: re-running replaces that date's section, keeping the
latest full-window numbers. State for the effective-interval calc lives in
~/.gpn/soak-state.json.

Usage: scripts/soak-record.py [--days 7] [--window 200] [--root <repo>]
Exit codes: 0 recorded, 2 nothing to record (node unreachable).
"""
import argparse
import glob
import json
import math
import os
import re
import shutil
import socket
import statistics
import struct
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timedelta, timezone

RPC = "http://127.0.0.1:9944"
STATE = os.path.expanduser("~/.gpn/soak-state.json")
TELEMETRY_DIR = os.path.expanduser("~/.gpn/telemetry")
TREND_DAYS = 7
TREND_THRESHOLD = 0.25  # a metric moving more than this week over week is drift
M64 = 0xFFFFFFFFFFFFFFFF


# ---------- substrate helpers (same xxh64 as gpn-state-query.py) ----------
def _xxh64(data, seed=0):
    P1 = 0x9E3779B185EBCA87; P2 = 0xC2B2AE3D27D4EB4F; P3 = 0x165667B19E3779F9
    P4 = 0x85EBCA77C2B2AE63; P5 = 0x27D4EB2F165667C5; M = M64

    def rotl(x, r):
        return ((x << r) | (x >> (64 - r))) & M
    n = len(data); i = 0
    if n >= 32:
        v1 = (seed + P1 + P2) & M; v2 = (seed + P2) & M; v3 = seed & M; v4 = (seed - P1) & M
        while i + 32 <= n:
            v1 = (rotl((v1 + int.from_bytes(data[i:i + 8], 'little') * P2) & M, 31) * P1) & M
            v2 = (rotl((v2 + int.from_bytes(data[i + 8:i + 16], 'little') * P2) & M, 31) * P1) & M
            v3 = (rotl((v3 + int.from_bytes(data[i + 16:i + 24], 'little') * P2) & M, 31) * P1) & M
            v4 = (rotl((v4 + int.from_bytes(data[i + 24:i + 32], 'little') * P2) & M, 31) * P1) & M
            i += 32
        h = (rotl(v1, 1) + rotl(v2, 7) + rotl(v3, 12) + rotl(v4, 18)) & M
        for v in (v1, v2, v3, v4):
            v = (rotl((v * P2) & M, 31) * P1) & M
            h = ((h ^ v) * P1 + P4) & M
    else:
        h = (seed + P5) & M
    h = (h + n) & M
    while i + 8 <= n:
        h = (rotl(h ^ ((rotl((int.from_bytes(data[i:i + 8], 'little') * P2) & M, 31) * P1) & M), 27) * P1 + P4) & M
        i += 8
    if i + 4 <= n:
        h = (rotl(h ^ ((int.from_bytes(data[i:i + 4], 'little') * P1) & M), 23) * P2 + P3) & M
        i += 4
    while i < n:
        h = (rotl(h ^ (data[i] * P5 & M), 11) * P1) & M
        i += 1
    h ^= h >> 33; h = (h * P2) & M
    h ^= h >> 29; h = (h * P3) & M
    h ^= h >> 32
    return h


def twox128(d):
    return struct.pack('<Q', _xxh64(d, 0)) + struct.pack('<Q', _xxh64(d, 1))


TS_KEY = (twox128(b"Timestamp") + twox128(b"Now")).hex()


def q(method, params=None, timeout=10):
    req = urllib.request.Request(
        RPC, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                              "params": params or []}).encode(),
        headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout)).get("result")


def rpc_with_retry(method, params=None, tries=3):
    last = None
    for _ in range(tries):
        try:
            return q(method, params)
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(2)
    raise last


def ts_from_storage_hex(r):
    """Decode a `state_getStorage` value for `Timestamp::Now`: SCALE u64 ms, LE.

    A big-endian decode here reads 1795649856381 as 9.0e18, i.e. a timestamp in the
    year 2256 — that mistake shipped in the first version of this file and is what
    this function exists to keep testable.
    """
    if not r:
        return None
    raw = bytes.fromhex(r[2:] if r.startswith("0x") else r)
    return int.from_bytes(raw[:8], "little")


def ts_at(block_hash):
    return ts_from_storage_hex(q("state_getStorage", [TS_KEY, block_hash]))


def _day_number(section):
    m = re.search(r"### Day (\d+)", section)
    return int(m.group(1)) if m else None


# a section starts at any level-2/3 heading, so an operator note or a verdict
# section written under the days is its own block — not the tail of the last day
_SECTION_RE = re.compile(r"(?m)^(#{2,3}) ")


def _split_blocks(text):
    """Split into (head, [block, ...]) where every block keeps its heading line.

    Splitting on `### Day ` alone made any note appended under a day part of that
    day's section, so the next same-day refresh deleted it (measured 2026-11-26:
    a 13:19Z rewrite ate two hand-written notes about the exit log and the
    shutdown path). A block boundary is a heading, not a day.
    """
    parts = _SECTION_RE.split(text)
    head = parts[0]
    blocks = [parts[i] + " " + parts[i + 1] for i in range(1, len(parts) - 1, 2)]
    return head, blocks


def splice_day(existing_text, header, section):
    """Keep the header, every day already recorded and any other section; replace only this day's.

    Idempotent for today (the recorder runs several times a day: timer + every loop
    pass) and *additive* across days. The first version kept only the header, so the
    day-2 write silently deleted day 1 — a seven-day soak could never hold seven
    days, and the rule that a missed day restarts the soak made that fatal.
    """
    head = ""
    sections = []
    if existing_text and _SECTION_RE.search(existing_text):
        head, sections = _split_blocks(existing_text)
        head = head.rstrip("\n")
    elif existing_text:
        head = existing_text.rstrip("\n")
    if not head.strip():
        head = header.rstrip("\n")

    new = section.strip("\n")
    n_new = _day_number(new)
    # Three buckets, because only the days are renumbered: anything written above the
    # first day (the baseline) stays on top, the days stay in ascending order, and
    # anything written below them (operator notes, the day-7 verdict) stays below.
    pre, days, post = [], [], []
    seen_day, replaced = False, False
    for sec in sections:
        sec = sec.strip("\n")
        n = _day_number(sec)
        if n is None:
            (post if seen_day else pre).append(sec)
            continue
        seen_day = True
        if n_new is not None and n == n_new:
            days.append((n, new))
            replaced = True
        else:
            days.append((n, sec))
    if not replaced:
        if n_new is None:
            post.append(new)
        else:
            days.append((n_new, new))
            replaced = True
    days.sort(key=lambda t: t[0])
    kept = pre + [d[1] for d in days] + post
    return head + "\n\n" + "\n\n".join(kept) + "\n"


def authored_intervals(window):
    """Intervals (s) between authored timestamps of the last `window` blocks."""
    best = int(rpc_with_retry("chain_getHeader")["number"], 16)
    start = max(1, best - window)
    stamps = []
    hashes = {}
    for n in range(start, best + 1):
        h = q("chain_getBlockHash", [n])
        if not h:
            continue
        hashes[n] = h
    for n, h in hashes.items():
        t = ts_at(h)
        if t:
            stamps.append(t)
    stamps.sort()
    return best, [(b - a) / 1000.0 for a, b in zip(stamps, stamps[1:])]


def journal(since, grep=None, unit="gpn-fleet.service", fmt="cat"):
    cmd = ["journalctl", "--user", "-u", unit, "--since", since, "-o", fmt, "--no-pager"]
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=60).stdout
    except Exception:  # noqa: BLE001
        return ""
    if grep:
        return "\n".join(l for l in out.splitlines() if re.search(grep, l))
    return out


def systemd_props(unit="gpn-fleet.service"):
    try:
        raw = subprocess.run(["systemctl", "--user", "show", unit,
                              "-p", "NRestarts", "-p", "ActiveEnterTimestamp",
                              "-p", "SubState", "-p", "ExecMainStartTimestamp"],
                             capture_output=True, text=True, timeout=30).stdout
    except Exception:  # noqa: BLE001
        return {}
    d = {}
    for line in raw.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            d[k] = v
    return d


def import_gaps(since):
    """(imported blocks, longest gap between imports in seconds, when) from the
    journal over `since`. Full-window coverage, independent of any sampling."""
    out = journal(since, grep=r"Imported #", fmt="short-iso")
    stamps = []
    for line in out.splitlines():
        m = re.match(r"^([0-9-]+)[T ]([0-9:]{8})", line)
        if not m:
            continue
        try:
            stamps.append(datetime.strptime(f"{m.group(1)} {m.group(2)}",
                                            "%Y-%m-%d %H:%M:%S").timestamp())
        except ValueError:
            continue
    stamps.sort()
    if len(stamps) < 2:
        return len(stamps), None, None
    gaps = [(b - a, b) for a, b in zip(stamps, stamps[1:])]
    worst, when = max(gaps)
    return len(stamps), worst, datetime.fromtimestamp(when, timezone.utc).strftime("%H:%M UTC")


GATE6_PATTERNS = {
    "unfunded": r"unfunded",
    "Incompatible pruning modes": r"Incompatible pruning modes",
    "update declined": r"update declined",
}


def gate6_counts(since):
    """STABILITY-GATE test 6: no silent failures. Counts the three named strings."""
    out = {}
    for label, pat in GATE6_PATTERNS.items():
        out[label] = len([l for l in journal(since, grep=pat).splitlines() if l.strip()])
    return out


def jobs_numbers():
    """attempted/succeeded from the AP job API and the local assignment store."""
    attempted = succeeded = failed = None
    try:
        req = urllib.request.Request("http://127.0.0.1:8092/jobs")
        data = json.load(urllib.request.urlopen(req, timeout=15))
        rows = data if isinstance(data, list) else data.get("jobs", [])
        attempted = len(rows)
        succeeded = sum(1 for r in rows if str(r.get("state", "")).lower() in
                        ("completed", "complete", "settled", "succeeded", "done"))
        failed = sum(1 for r in rows if str(r.get("state", "")).lower() in
                     ("failed", "expired", "rejected"))
    except Exception:  # noqa: BLE001
        pass
    return attempted, succeeded, failed


UPDATE_UNIT = "gpn-update.service"
EXIT_LOG = os.path.expanduser("~/.gpn/last-exit.json")


SYSTEMD_NOISE = re.compile(
    r"^(Starting |Finished |.*Consumed .*CPU time|.*: (Succeeded|Deactivated|Main process exited)|"
    r"-- )")


def update_journal_lines(unit=UPDATE_UNIT, hours=24):
    """The update unit's own output in the window, or None when there is no journal.

    This is the only source that answers "did the update work?": each fire prints either
    `up to date` or `confirmed <version> is running` / a refusal. `last-exit.json` cannot
    answer it -- it records node *start failures*, and any caller may write there (row 281:
    a test wrote 32 fake `corrupt-key` records and two soak days quoted them as the update
    outcome).
    """
    try:
        out = subprocess.run(
            ["journalctl", "--user", "-u", unit, "--since", f"{hours} hours ago",
             "--no-pager", "-o", "cat"],
            capture_output=True, text=True, timeout=20)
    except Exception:  # noqa: BLE001
        return None
    if out.returncode != 0:
        return None
    lines = [l.strip() for l in out.stdout.splitlines() if l.strip()]
    return lines or None


def summarise_update_lines(lines):
    """(fires, the updater's own lines) from a unit journal extract.

    systemd's `Starting`/`Finished` lines say a fire happened, not what it decided; the
    updater's own lines (`confirmed 0.3.80 is running`, `up to date`, `holding`, `error:`)
    are the outcome. Both are kept, separately, so "fired 24 times" cannot be mistaken for
    "did anything".
    """
    if not lines:
        return 0, []
    fires = sum(1 for l in lines if l.startswith("Starting "))
    own = [l for l in lines if not SYSTEMD_NOISE.match(l)]
    return fires, own


def restart_audit(lines):
    """(terminations, planned, unexplained) from a journal extract, in order.

    A *termination* is the node's own `Essential task `<name>` failed. Shutting down service.`
    line. It is counted PLANNED when systemd's `Stopping gpn-fleet.service` line appears within
    two lines either side of it: systemd stops the unit and the node's txpool background task
    then ends with `revalidation_queue::revalidate_view: ... error=Closed(..)`.

    Measured 2026-11-26 over 3 days: 13 terminations, 12 carrying the stop line *before* the
    node's line. The 13th (the 0.3.80 updater apply, 2026-11-25 21:05:49) carries it *after*:
    `gpn_update` signals the node's process directly, so the node's own line is written first and
    systemd records the stop reason a line later in the unit's journal (read back with
    `journalctl --user -u gpn-fleet.service -o cat` ⇒ termination at index i, `Stopping
    gpn-fleet.service ...` at i+1). A backward-only window scored a planned restart as
    "unexplained", and an unexplained count above 0 is what blocks the stability gate — a
    classifier that cannot see a stop it caused itself makes day 7 unreachable. `Stopping` is
    emitted only for an explicit stop request, never for a crash (a dead main process logs
    `Main process exited` / `Scheduled restart job`), so the forward half cannot mask a real death.

    Without this split the day line could only say "starts/stops happened", which reads as
    restarts by choice and hides a node that died on its own. The unexplained count is what the
    stability gate must read, so it is counted, never assumed away.
    """
    terminations = planned = 0
    for i, line in enumerate(lines):
        if "Essential task `" in line and "failed" in line:
            terminations += 1
            window = lines[max(0, i - 2):i + 3]
            if any("Stopping gpn-fleet.service" in w for w in window):
                planned += 1
    return terminations, planned, terminations - planned


def newest_exit_record(path=None):
    """The newest record in the JSONL exit log, as (age_hours, record), or None."""
    path = path or EXIT_LOG
    if not os.path.exists(path):
        return None
    age_h = (time.time() - os.stat(path).st_mtime) / 3600.0
    best = None
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(rec, dict):
                    continue
                key = rec.get("ts_unix") or 0
                if best is None or key >= best[0]:
                    best = (key, rec)
    except Exception:  # noqa: BLE001
        return (age_h, None)
    return (age_h, best[1] if best else None)


def update_outcome():
    """Day's update outcome, then any node exit report -- labelled, never conflated."""
    bits = []
    lines = update_journal_lines()
    if lines:
        fires, own = summarise_update_lines(lines)
        if own:
            bits.append(f"`{UPDATE_UNIT}` (24 h, {fires} fire(s)): " + " | ".join(own[-3:])[:300])
        else:
            bits.append(f"`{UPDATE_UNIT}` (24 h, {fires} fire(s)): no version change "
                        f"(every fire ended 'up to date')")
    log = os.path.expanduser("~/.gpn/update/update.log")
    if os.path.exists(log):
        try:
            age_h = (time.time() - os.stat(log).st_mtime) / 3600.0
            if age_h <= 24:
                tail = subprocess.run(["tail", "-3", log], capture_output=True,
                                      text=True, timeout=15).stdout.strip().replace("\n", " | ")
                if tail:
                    bits.append(f"`update.log` ({age_h:.1f}h old): {tail[:300]}")
        except Exception as e:  # noqa: BLE001
            bits.append(f"update.log: unreadable ({e})")
    newest = newest_exit_record()
    if newest:
        age_h, rec = newest
        if age_h <= 24:
            if rec:
                detail = str(rec.get("detail") or "")[:160]
                bits.append(f"node exit report (NOT an update result) "
                            f"`last-exit.json` ({age_h:.1f}h old, {rec.get('ts','?')}, "
                            f"kind={rec.get('kind','?')}): {detail}")
            else:
                bits.append(f"`last-exit.json` ({age_h:.1f}h old): no parseable record")
    return bits or ["no update activity in the last 24 h"]


def box1_snapshot(root):
    """Second soak machine. A failure is recorded, never omitted."""
    candidates = [os.path.join(root, "scripts", "box1.sh"),
                  os.path.expanduser("~/.openclaw/workspace/scripts/box1.sh")]
    script = next((c for c in candidates if os.path.exists(c)), None)
    if not script:
        return "unreachable (no box1.sh at " + " or ".join(candidates) + ")"
    ps = ("$h=(Invoke-RestMethod -Uri http://127.0.0.1:9944 -Method Post "
          "-ContentType 'application/json' "
          "-Body '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"system_health\",\"params\":[]}').result;"
          "$b=(Invoke-RestMethod -Uri http://127.0.0.1:9944 -Method Post "
          "-ContentType 'application/json' "
          "-Body '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"chain_getHeader\",\"params\":[]}').result.number;"
          "\"peers=$($h.peers) syncing=$($h.isSyncing) height=$([Convert]::ToInt64($b,16))\"")
    try:
        r = subprocess.run([script, ps], capture_output=True, text=True, timeout=180)
    except subprocess.TimeoutExpired:
        return "unreachable (timeout 180s)"
    out = (r.stdout or "").strip().splitlines()
    out = [l for l in out if l.strip()]
    if r.returncode != 0 and not out:
        return f"unreachable (box1.sh exit {r.returncode})"
    return out[-1] if out else f"no output (exit {r.returncode})"


def load_state():
    try:
        with open(STATE) as f:
            return json.load(f)
    except Exception:  # noqa: BLE001
        return {}


def save_state(st):
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(st, f, indent=1)
    os.replace(tmp, STATE)


def day_index(start_date, today):
    return (today - start_date).days + 1


def _entry_day(entry):
    """The UTC date a state entry was recorded on, or None."""
    ts = entry.get("ts") if isinstance(entry, dict) else None
    if not ts:
        return None
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%d")


def select_prev(st, today_str):
    """The record the day's effective interval is measured FROM.

    Priority: the LAST reading of a previous day — that is the day-over-day drift the
    gate reads — then, on the soak's first day or with a legacy state file that has no
    per-day history, this day's FIRST reading.

    Why the day's own first reading must not win: it is written by whichever pass runs
    first that day. On 2026-11-26 a loop pass at 13:19Z created day 2's first reading and
    the timer's 13:20Z record measured from it, so the day's headline number became
    "2 blocks in 0.02 h = 32.53 s/block, slower than the 13.4 s baseline — drift": a
    65-second window presented as the day's cadence, twice the truth (the real
    day-over-day figure was 3,138 blocks in 13.38 h = 15.35 s/block).
    """
    earlier = {d: e for d, e in (st.get("day_last") or {}).items()
               if isinstance(e, dict) and d < today_str}
    legacy = st.get("last") or {}
    ld = _entry_day(legacy)
    if ld and ld < today_str and ld not in earlier:
        earlier[ld] = legacy
    if earlier:
        return earlier[max(earlier)]
    first = (st.get("day_first") or {}).get(today_str)
    if first:
        return first
    return legacy


# The drift number is a day-scale measure: a window shorter than this cannot say anything
# about cadence, and using one turns 2 blocks into a headline figure (see select_prev).
# The prose in main() carries the same two numbers — edit them together.
MIN_WINDOW_SECS = 900
BASELINE_INTERVAL_SECS = 13.4
DRIFT_LIMIT_SECS = 15.0


def effective_interval_secs(prev_height, prev_ts, best, now_ts, min_dt=MIN_WINDOW_SECS):
    """Seconds per block between two records — the number that catches a stall.

    Height delta over elapsed wall time, expressed as s/block (NOT blocks/s):
    the day-1 record printed 0.08 "s" for 83 blocks in 0.29 h, which is the
    reciprocal (83/1044) with the wrong unit. A stall makes this number big,
    so the drift test has to compare it against the cadence baseline (>15 s).
    Returns None when there is no usable previous record (first record of the
    soak, no forward progress, or a window shorter than `min_dt`).
    """
    if not prev_height or not prev_ts or best is None or best <= prev_height:
        return None
    dt = now_ts - prev_ts
    if dt <= min_dt:
        return None
    return dt / (best - prev_height)


# ---------- per-node telemetry, row 274 --------------------------------------
# One small JSON per node per day, so the audit can trend a number instead of
# reading prose. Every node writes its own ~/.gpn/telemetry/<date>.json; the
# node name inside the file is what makes a copied/fleet-gathered file honest.
def telemetry_payload(height, peers, intervals, restarts_24h, jobs, update,
                      disk_free_gb, node=None, now=None):
    now = now or datetime.now(timezone.utc)
    attempted, succeeded, failed = (jobs or (None, None, None))[:3]
    iv = [x for x in (intervals or []) if isinstance(x, (int, float)) and not math.isnan(x)]
    p50 = statistics.median(iv) if iv else None
    p95 = (statistics.quantiles(iv, n=20)[18] if len(iv) >= 20
           else (max(iv) if iv else None))
    return {
        "node": node or socket.gethostname(),
        "date": now.strftime("%Y-%m-%d"),
        "recorded_at": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "height": height,
        "peers": peers,
        "block_interval_p50": p50,
        "block_interval_p95": p95,
        "restarts_24h": restarts_24h,
        "jobs": {"attempted": attempted, "succeeded": succeeded, "failed": failed},
        "update": update if isinstance(update, (list, dict)) else [update] if update else [],
        "disk_free_gb": disk_free_gb,
    }


def write_telemetry(payload, directory=None):
    d = directory or TELEMETRY_DIR
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, f"{payload['date']}.json")
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(payload, f, indent=1, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)
    return path


_FLAT_METRICS = ("height", "peers", "block_interval_p50", "block_interval_p95",
                 "restarts_24h", "disk_free_gb", "jobs.attempted", "jobs.succeeded",
                 "jobs.failed")


def _flat(rec, key):
    cur = rec
    for part in key.split("."):
        if not isinstance(cur, dict) or part not in cur:
            return None
        cur = cur[part]
    return cur


def trend_report(directory=None, days=TREND_DAYS, threshold=TREND_THRESHOLD):
    """Week-over-week trend from the telemetry files.

    Returns (text, flags). Only a run of `days` CONSECUTIVE dates counts: a day
    missed means the week is not a week, and no flag is derived from it.
    """
    d = directory or TELEMETRY_DIR
    recs = {}
    for p in sorted(glob.glob(os.path.join(d, "*.json"))):
        try:
            with open(p) as f:
                r = json.load(f)
        except Exception:  # noqa: BLE001
            continue
        if r.get("date"):
            recs[r["date"]] = r
    if not recs:
        return (f"insufficient: no telemetry files under {d}", [])
    latest = max(datetime.strptime(k, "%Y-%m-%d") for k in recs)
    run = []
    for i in range(days):
        day = (latest - timedelta(days=i)).strftime("%Y-%m-%d")
        if day not in recs:
            break
        run.append(day)
    if len(run) < days:
        return (f"insufficient: {len(run)} consecutive day(s) of {days} needed for a "
                f"week-over-week trend (latest {latest.strftime('%Y-%m-%d')})", [])
    run.sort()
    first, last = recs[run[0]], recs[run[-1]]
    lines = [f"trend {run[0]} -> {run[-1]} ({days} consecutive days, node "
             f"{last.get('node', '?')}):"]
    flags = []
    for key in _FLAT_METRICS:
        a, b = _flat(first, key), _flat(last, key)
        if not isinstance(a, (int, float)) or not isinstance(b, (int, float)) or a == 0:
            continue
        pct = (b - a) / abs(a)
        lines.append(f"  {key}: {a:g} -> {b:g} ({pct * 100:+.1f}%)")
        if abs(pct) > threshold:
            flags.append(f"{key} moved {pct * 100:+.1f}% week over week "
                         f"({a:g} -> {b:g}) on {last.get('node', '?')}")
    if flags:
        lines.append("DRIFT:")
        lines.extend(f"  - {f}" for f in flags)
    else:
        lines.append(f"no metric moved more than {threshold * 100:.0f}% week over week")
    return ("\n".join(lines), flags)


# ---------- day-7 verdict (row 273: one verdict per STABILITY-GATE test) ----------
# The recorder writes numbers; the gate asks six questions. Until the verdict was generated
# from those numbers, day 7 would have been hand-written by reading seven sections — the exact
# place where a stretch becomes a claim. Every verdict below quotes the day it came from, and
# a test the soak does not measure says so instead of quietly counting as a pass.
GATE_TESTS = [
    (1, "Clean install, no prompt"),
    (2, "Two unattended updates"),
    (3, "Seven days peered"),
    (4, "A job served by a remote node"),
    (5, "Self-heal"),
    (6, "No silent failures"),
]

_DAY_FIELDS = {
    "height": r"^- height: \*\*(\d+)\*\*",
    "eff": r"^- effective block interval[^:]*: \*\*([\d.]+) s\*\*",
    "p50": r"p50 \*\*([\d.]+) s\*\*",
    "p95": r"p95 \*\*([\d.]+) s\*\*",
    "peers": r"^- peers now: \*\*(\d+)\*\*",
    "peer_min": r"idle lines, min \*\*(\d+)\*\*",
    "zero_peers": r"lines at 0 peers: \*\*(\d+)\*\*",
    "imports": r"^- imports in 24 h: \*\*(\d+)\*\* blocks",
    "gap": r"longest gap between imports \*\*(\d+) s\*\*",
    "terminations": r"^- node terminations in 24 h: \*\*(\d+)\*\*",
    "unexplained": r"\*\*(\d+) unexplained\*\*",
    "starts": r"\*\*(\d+) systemd starts / (\d+) stops",
    "jobs_att": r"^- jobs: attempted=(\d+)",
    "jobs_ok": r"^- jobs: attempted=\d+, succeeded=(\d+)",
    "jobs_fail": r"^- jobs: attempted=\d+, succeeded=\d+, failed=(\d+)",
    "box1": r"^- box 1: (.+)$",
    "update": r"^- update: (.+)$",
    "gate6": r"^- gate 6 \(silent failures\) in 24 h: (.+)$",
    "bans": r"^- `Reason:`/ban lines in 24 h: \*\*(\d+)\*\*",
    "stamp": r"^### Day \d+ — (\d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC)",
}


def parse_day(section):
    """Read one recorded day section into numbers. Absent field => None, never 0."""
    day = {"day": _day_number(section)}
    for key, pat in _DAY_FIELDS.items():
        m = re.search(pat, section, re.M)
        if not m:
            day[key] = None
        elif key in ("starts", "gate6", "box1", "update", "stamp"):
            day[key] = m.groups() if len(m.groups()) > 1 else m.group(1)
        elif key in ("eff", "p50", "p95"):
            day[key] = float(m.group(1))
        else:
            day[key] = int(m.group(1))
    if day.get("gate6"):
        day["gate6"] = {k: int(v) for k, v in re.findall(r"`([^`]+)` \*\*(\d+)\*\*", day["gate6"])}
    return day


def parse_days(text):
    """Every `### Day n` section, ascending."""
    _, blocks = _split_blocks(text or "")
    days = [parse_day(b) for b in blocks if _day_number(b) is not None]
    return sorted(days, key=lambda d: d["day"])


def _distinct_versions(days):
    """Versions a day claims the updater confirmed running, in order first seen."""
    seen = []
    for d in days:
        for v in re.findall(r"confirmed (\S+) is running", d.get("update") or ""):
            if v not in seen:
                seen.append(v)
    return seen


def soak_verdict(days, required=7):
    """One verdict per STABILITY-GATE test: (num, name, verdict, evidence).

    verdict is PASS, FAIL, INCOMPLETE (the soak cannot answer yet) or NOT-COVERED (the
    soak records nothing that bears on it — named with where the evidence does live).
    """
    days = [d for d in days if d.get("day")]
    got = len(days)
    nums = [d["day"] for d in days]
    consecutive = nums == list(range(1, got + 1))
    unexplained = sum(d.get("unexplained") or 0 for d in days)
    zero_peers = sum(d.get("zero_peers") or 0 for d in days)
    starved_days = [d["day"] for d in days if d.get("peer_min") == 0 or d.get("peers") == 0]
    versions = _distinct_versions(days)
    rollbacks = [d["day"] for d in days if "rolled back to" in (d.get("update") or "")]
    g6_bad = []
    for d in days:
        for k, v in (d.get("gate6") or {}).items():
            if v:
                g6_bad.append(f"day {d['day']}: `{k}` {v}")
    bans = sum(d.get("bans") or 0 for d in days)

    out = []

    # 3 — the soak's own question, so it is answered first and answers plainly.
    if got < required:
        v3 = ("INCOMPLETE", f"day {got} of {required} recorded"
              + ("" if consecutive else f" (day numbers {nums} are not consecutive)"))
    elif not consecutive:
        v3 = ("FAIL", f"day numbers {nums} are not seven consecutive days")
    elif unexplained:
        v3 = ("FAIL", f"{unexplained} unexplained termination(s) across days "
                       f"{[d['day'] for d in days]}")
    elif zero_peers or starved_days:
        v3 = ("FAIL", f"{zero_peers} journal line(s) at 0 peers; days with a 0-peer reading: "
                       f"{starved_days or 'none'}")
    else:
        v3 = ("PASS", f"{got} consecutive days, {unexplained} unexplained terminations, "
                       f"0 lines at 0 peers, peer min per day "
                       f"{[d.get('peer_min') for d in days]}")

    if len(versions) >= 2:
        v2 = ("PASS", f"updater confirmed {len(versions)} distinct versions running: "
                       f"{', '.join(versions)}"
                       + (f"; rollback exercised on day(s) {rollbacks}" if rollbacks else
                          "; rollback of a bad build not exercised in this soak"))
    else:
        v2 = ("INCOMPLETE", f"{len(versions)} distinct version(s) confirmed running "
                            f"({', '.join(versions) or 'none recorded'}) — the gate asks for two")

    if g6_bad:
        v6 = ("FAIL", "; ".join(g6_bad) + (f"; {bans} `Reason:`/ban line(s)" if bans else ""))
    elif got < required:
        v6 = ("INCOMPLETE", f"clean so far ({got} of {required} days, 0 silent-failure strings)")
    else:
        v6 = ("PASS", f"{got} days: unfunded 0, incompatible pruning 0, update declined 0, "
                       f"`Reason:`/ban lines 0")

    out.append((3, "Seven days peered", v3[0], v3[1]))
    out.append((2, "Two unattended updates", v2[0], v2[1]))
    out.append((6, "No silent failures", v6[0], v6[1]))
    out.append((1, "Clean install, no prompt", "NOT-COVERED",
                "a soak measures a node already installed; the install half lives in "
                "checklist §0e (`scripts/stranger-lifecycle.ps1`) and rows 104/106/208"))
    out.append((4, "A job served by a remote node", "NOT-COVERED",
                "the soak records job counts, not attribution; needs a provider id that is not "
                "the AP's host (gate 4's own evidence, rows 275/276)"))
    out.append((5, "Self-heal", "NOT-COVERED",
                "a fork must be manufactured to test this; proven for the depth guard in row 194 "
                "and the pause-clearing residual in row 262"))
    return sorted(out, key=lambda t: t[0])


def verdict_section(days, required=7, now=None):
    """Markdown for the day's verdict block."""
    now = now or datetime.now(timezone.utc)
    rows = soak_verdict(days, required)
    verdicts = [r[2] for r in rows]
    failing = [r for r in rows if r[2] == "FAIL"]
    uncovered = [r for r in rows if r[2] == "NOT-COVERED"]
    pending = [r for r in rows if r[2] == "INCOMPLETE"]
    if failing:
        overall = "GATE RED — " + ", ".join(f"test {r[0]}" for r in failing)
    elif pending:
        overall = ("GATE NOT MET YET — still measuring: "
                   + ", ".join(f"test {r[0]}" for r in pending)
                   + ("; outside the soak: " + ", ".join(f"test {r[0]}" for r in uncovered)
                      if uncovered else ""))
    elif uncovered:
        # every soaked test passes, but the soak cannot answer the rest: say exactly that
        # rather than printing a green tick the numbers do not support.
        overall = ("SOAK GREEN, GATE STILL OPEN — tests "
                   + ", ".join(str(r[0]) for r in uncovered)
                   + " need their own evidence (install / remote attribution / self-heal)")
    else:
        overall = "GATE GREEN — all six tests pass"
    lines = [f"## Verdict — {now.strftime('%Y-%m-%d %H:%M UTC')} (day {len(days)} of {required})",
             "", f"**{overall}**", "",
             "| # | test | verdict | evidence |", "|---|---|---|---|"]
    for num, name, verdict, evidence in rows:
        lines.append(f"| {num} | {name} | **{verdict}** | {evidence} |")
    lines.append("")
    lines.append("Generated by `scripts/soak-record.py --verdict` from the day sections above; "
                 "a NOT-COVERED row is not a pass and is not counted as one.")
    return "\n".join(lines), overall


def replace_verdict(text, section):
    """Put one verdict block under the days, replacing any earlier one."""
    head, blocks = _split_blocks(text or "")
    kept = [b.strip("\n") for b in blocks
            if not re.match(r"^## Verdict", b.strip("\n"))]
    kept.append(section.strip("\n"))
    return head.rstrip("\n") + "\n\n" + "\n\n".join(kept) + "\n"


def main():
    global RPC
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=7)
    ap.add_argument("--window", type=int, default=200)
    ap.add_argument("--root", default=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap.add_argument("--rpc", default=RPC)
    ap.add_argument("--telemetry-dir", default=TELEMETRY_DIR)
    ap.add_argument("--trend", action="store_true",
                    help="print the week-over-week trend from the telemetry files and exit")
    ap.add_argument("--verdict", action="store_true",
                    help="print the per-gate verdict from the recorded days and exit")
    ap.add_argument("--write-verdict", action="store_true",
                    help="with --verdict: also record it in the soak doc as the `## Verdict` block")
    ap.add_argument("--doc", default=None, help="soak doc path (default: docs/soak-<start>.md)")
    args = ap.parse_args()
    RPC = args.rpc

    if args.verdict:
        st0 = load_state()
        doc0 = args.doc or os.path.join(args.root, "docs",
                                        f"soak-{st0.get('start_date', '')}.md")
        if not os.path.exists(doc0):
            print(f"no soak doc at {doc0}", file=sys.stderr)
            return 2
        text0 = open(doc0).read()
        days0 = parse_days(text0)
        section0, overall0 = verdict_section(days0, args.days)
        print(section0)
        if args.write_verdict:
            tmp0 = doc0 + ".tmp"
            with open(tmp0, "w") as f:
                f.write(replace_verdict(text0, section0))
            os.replace(tmp0, doc0)
            print(f"\nwrote verdict to {doc0}", file=sys.stderr)
        return 0 if overall0.startswith("GATE GREEN") else 1


    if args.trend:
        text, flags = trend_report(args.telemetry_dir)
        print(text)
        if flags:
            print(f"DRIFT: {len(flags)} metric(s) moved more than {TREND_THRESHOLD * 100:.0f}% "
                  f"week over week", file=sys.stderr)
        return 1 if flags else 0

    now = datetime.now(timezone.utc)
    st = load_state()
    if "start_date" not in st:
        st["start_date"] = now.strftime("%Y-%m-%d")
    start = datetime.strptime(st["start_date"], "%Y-%m-%d").replace(tzinfo=timezone.utc)
    doc = os.path.join(args.root, "docs", f"soak-{st['start_date']}.md")

    try:
        health = rpc_with_retry("system_health")
    except Exception as e:  # noqa: BLE001
        print(f"node unreachable: {e}", file=sys.stderr)
        return 2

    best, intervals = authored_intervals(args.window)
    p50 = statistics.median(intervals) if intervals else float("nan")
    p95 = (statistics.quantiles(intervals, n=20)[18] if len(intervals) >= 20
           else (max(intervals) if intervals else float("nan")))

    # effective interval since the previous record = the stall catcher
    today_str = now.strftime("%Y-%m-%d")
    prev = select_prev(st, today_str)
    eff = effective_interval_secs(prev.get("height"), prev.get("ts"),
                                  best, now.timestamp())

    since = (now - timedelta(hours=24)).strftime("%Y-%m-%d %H:%M:%S")
    bans = journal(since, grep=r"Reason:|banned|ban ")
    starts = journal(since, grep=r"^(Started|Stopped) ")
    n_start = sum(1 for l in starts.splitlines() if l.startswith("Started "))
    n_stop = sum(1 for l in starts.splitlines() if l.startswith("Stopped "))
    n_imports, worst_gap, gap_at = import_gaps(since)
    g6 = gate6_counts(since)
    props = systemd_props()
    att, succ, fail = jobs_numbers()
    box1 = box1_snapshot(args.root)
    day = day_index(start, now)

    peers_lines = journal(since, grep=r"peers")
    peer_nums = [int(m.group(1)) for m in
                 (re.search(r"\((\d+) peers\)", l) for l in peers_lines.splitlines()) if m]
    zero_peer = sum(1 for n in peer_nums if n == 0)

    lines = []
    lines.append(f"### Day {day} — {now.strftime('%Y-%m-%d %H:%M UTC')}")
    lines.append("")
    lines.append(f"- height: **{best}**")
    if eff is not None:
        lines.append(f"- effective block interval since previous record "
                     f"({best - prev['height']} blocks in {(now.timestamp()-prev['ts'])/3600:.2f} h): "
                     f"**{eff:.2f} s**"
                     + ("" if eff <= DRIFT_LIMIT_SECS else "  ⚠️ slower than the 13.4 s baseline — name this as drift"))
    else:
        lines.append("- effective block interval: n/a ("
                     + (f"no forward progress or a window shorter than "
                        f"{MIN_WINDOW_SECS} s"
                        if prev.get("height") else "first record of this soak") + ")")
    lines.append(f"- authored interval over the last {len(intervals)} blocks: "
                 f"p50 **{p50:.2f} s**, p95 **{p95:.2f} s**")
    lines.append(f"- peers now: **{health.get('peers')}**, isSyncing={health.get('isSyncing')}"
                 + (f"; journal 24 h: {len(peer_nums)} idle lines, min **{min(peer_nums)}** / "
                    f"max {max(peer_nums)}, lines at 0 peers: **{zero_peer}**"
                    if peer_nums else "; no peer log lines in 24 h"))
    if n_imports:
        lines.append(f"- imports in 24 h: **{n_imports}** blocks, longest gap between imports "
                     f"**{worst_gap:.0f} s** (ending {gap_at})")
    term = journal(since, grep=r"Stopping |Started |Essential task")
    n_term, n_planned, n_unexplained = restart_audit(term.splitlines())
    lines.append(f"- node terminations in 24 h: **{n_term}** "
                 f"({n_planned} adjacent to a systemd `Stopping`, "
                 f"**{n_unexplained} unexplained**) — an unexplained count above 0 is a node that "
                 f"died on its own and must be root-caused before the gate is called green")
    lines.append(f"- gpn-fleet.service: NRestarts={props.get('NRestarts','?')}, "
                 f"state={props.get('SubState','?')}, active since {props.get('ActiveEnterTimestamp','?')}, "
                 f"**{n_start} systemd starts / {n_stop} stops in 24 h** "
                 f"(manual or deploy restarts, not crashes — NRestarts counts only failed units)")
    lines.append(f"- jobs: attempted={att}, succeeded={succ}, failed={fail} (AP :8092/jobs)")
    lines.append("- update: " + "; ".join(update_outcome()))
    lines.append(f"- `Reason:`/ban lines in 24 h: **{len([l for l in bans.splitlines() if l.strip()])}**")
    lines.append("- gate 6 (silent failures) in 24 h: " + ", ".join(
        f"`{k}` **{v}**" for k, v in g6.items()))
    if bans.strip():
        for l in bans.strip().splitlines()[:5]:
            lines.append(f"  - `{l.strip()[:200]}`")
    lines.append(f"- box 1: {box1}")
    lines.append("")
    section = "\n".join(lines)

    header = (f"# Soak {st['start_date']} — {args.days} days, row 273\n\n"
              "Machine A: this host (`openclawassist`, node + AP + providers).\n"
              "Machine B: box 1 (Windows, reachable over SSH; recorded per day, never omitted).\n\n"
              "Protocol: `scripts/soak-record.py` is run at least once per day and appends that day's\n"
              "numbers below (idempotent per date — a second run replaces the day's section with the\n"
              "latest full-window numbers). p50/p95 are authored-timestamp intervals over the last 200\n"
              "blocks; the effective interval is height delta over elapsed wall time between records and\n"
              "is the only number that catches a stall nobody was watching. Verdicts per\n"
              "`docs/STABILITY-GATE.md` test are written on the final day.\n\n"
              "Baseline (day 1, this host): block interval p50 13.4 s is the reference cadence; a move\n"
              "to 18 s is drift and must be named as drift, not reported as healthy.\n\n")

    existing = ""
    if os.path.exists(doc):
        existing = open(doc).read()
    body = splice_day(existing, header, section)
    tmp = doc + ".tmp"
    with open(tmp, "w") as f:
        f.write(body)
    os.replace(tmp, doc)

    # row 274: the same numbers as one small JSON per node per day, so the audit
    # can trend them. Disk free is the machine's home filesystem — the one that
    # fills and takes the node down.
    try:
        disk_free_gb = round(shutil.disk_usage(os.path.expanduser("~")).free / 1e9, 1)
    except Exception:  # noqa: BLE001
        disk_free_gb = None
    try:
        tpath = write_telemetry(telemetry_payload(
            height=best, peers=health.get("peers"), intervals=intervals,
            restarts_24h=n_start, jobs=(att, succ, fail),
            update="; ".join(update_outcome()), disk_free_gb=disk_free_gb, now=now),
            args.telemetry_dir)
    except Exception as e:  # noqa: BLE001
        tpath = f"not written ({e})"
        print(f"telemetry not written: {e}", file=sys.stderr)

    trend_text, trend_flags = trend_report(args.telemetry_dir)

    st["last"] = {"height": best, "ts": now.timestamp(), "peers": health.get("peers")}
    st.setdefault("day_first", {}).setdefault(today_str, {"height": best, "ts": now.timestamp()})
    # Per-day LAST reading, so tomorrow's record measures from today rather than from
    # whichever pass happens to run first tomorrow (the 2026-11-26 defect, see select_prev).
    st.setdefault("day_last", {})[today_str] = {"height": best, "ts": now.timestamp()}
    st.setdefault("days", {})[today_str] = best
    save_state(st)

    print(f"recorded day {day} at height {best}: {doc}")
    print(f"telemetry: {tpath}")
    print(trend_text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
