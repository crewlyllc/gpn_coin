#!/usr/bin/env python3
"""Tests for scripts/soak-record.py (row 273).

The recorder's two fallible pieces that are not "ask the node a question" are:
  * decoding `Timestamp::Now` — a big-endian decode silently yields a year-2256 value
  * splicing a day's section — run several times a day, it must replace, not stack

Both are exercised here with no node and no journal. Run:
  python3 scripts/tests/test-soak-record.py
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(os.path.dirname(HERE), "soak-record.py")

spec = importlib.util.spec_from_file_location("soak_record", SRC)
sr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sr)

fails = []
checks = 0


def eq(got, want, label):
    global checks
    checks += 1
    if got != want:
        fails.append(f"FAIL {label}: got {got!r}, want {want!r}")


# ---- Timestamp::Now decoding -------------------------------------------------
# real value read from the live node at block 364394 (2026-11-25T23:37:36.381Z)
eq(sr.ts_from_storage_hex("0x7d631215a2010000"), 1795649856381,
   "LE decode of a real Timestamp::Now value")
eq(sr.ts_from_storage_hex("7d631215a2010000"), 1795649856381,
   "decodes with or without the 0x prefix")
eq(sr.ts_from_storage_hex("0x0000000000000000"), 0, "zero decodes to zero")
eq(sr.ts_from_storage_hex(None), None, "absent storage key is None, not 0")
# the wrong (big-endian) reading of the same bytes, kept as the regression it is
eq(int("7d631215a2010000", 16) == 1795649856381, False,
   "big-endian would NOT have given the right answer")

# ---- storage key ------------------------------------------------------------
# twox128(x) = xxh64(x, 0) ++ xxh64(x, 1). Checked against published xxhash64 vectors
# so a broken hash would be caught before it silently reads the wrong storage item.
eq(len(sr.TS_KEY), 64, "storage key is 32 bytes of hex")
eq(sr._xxh64(b""), 0xEF46DB3751D8E999, "xxh64('') reference vector")
eq(sr._xxh64(b"a"), 0xD24EC4F1A98C6E5B, "xxh64('a') reference vector")
eq(sr._xxh64(b"abc"), 0x44BC2CF5AD770999, "xxh64('abc') reference vector")
eq(sr.TS_KEY, sr.twox128(b"Timestamp").hex() + sr.twox128(b"Now").hex(),
   "key is twox128('Timestamp') ++ twox128('Now')")

# ---- day splicing -----------------------------------------------------------
header = "# Soak 2026-11-25\n\nprotocol text\n"
day1 = "### Day 1 — 2026-11-25 10:00 UTC\n\n- height: **100**"
day1b = "### Day 1 — 2026-11-25 22:00 UTC\n\n- height: **200**"

doc = sr.splice_day("", header, day1)
eq(doc.count("### Day 1"), 1, "first write has exactly one day section")
eq("protocol text" in doc, True, "header is kept on first write")
eq("- height: **100**" in doc, True, "first reading is present")

doc2 = sr.splice_day(doc, header, day1b)
eq(doc2.count("### Day 1"), 1, "second write replaces the day, does not stack it")
eq("- height: **200**" in doc2, True, "second write wins")
eq("- height: **100**" in doc2, False, "the superseded reading is gone")
eq("protocol text" in doc2, True, "header survives a replacement")
eq(doc2.endswith("\n"), True, "file ends with a newline")

# a day-2 section must not eat the day-1 section
day2 = "### Day 2 — 2026-11-26 10:00 UTC\n\n- height: **300**"
doc3 = sr.splice_day(doc2 + "\n" + day2, header, "### Day 2 — 2026-11-26 22:00 UTC\n\n- height: **400**")
eq(doc3.count("### Day 2"), 1, "day 2 appears once after a rewrite")

# ---- per-node telemetry (row 274) -------------------------------------------
# The audit must be able to TREND, not just print prose: one small JSON per node
# per day, and a report that flags a metric moving >25% week over week.
import json
import shutil
import tempfile

required = ["node", "date", "recorded_at", "height", "peers", "block_interval_p50",
            "block_interval_p95", "restarts_24h", "jobs", "update", "disk_free_gb"]

tmpdir = tempfile.mkdtemp(prefix="soak-telemetry-")
try:
    payload = sr.telemetry_payload(
        height=364401, peers=3, intervals=[10.0, 11.0, 12.0], restarts_24h=4,
        jobs=(256, 229, 7), update="no update activity in the last 24 h",
        disk_free_gb=41.5, node="openclawassist",
        now=__import__("datetime").datetime(2026, 11, 25, 23, 39, tzinfo=__import__("datetime").timezone.utc))
    for k in required:
        eq(k in payload, True, f"telemetry payload carries `{k}`")
    eq(payload["date"], "2026-11-25", "payload date is the UTC date")
    eq(payload["height"], 364401, "payload height is the measured height")
    eq(payload["jobs"]["succeeded"], 229, "payload keeps job counts")
    eq(round(payload["block_interval_p50"], 2), 11.0, "payload keeps the interval p50")

    p1 = sr.write_telemetry(payload, tmpdir)
    eq(os.path.basename(p1), "2026-11-25.json", "telemetry file is named for the UTC date")
    eq(len(os.listdir(tmpdir)), 1, "one file per node per day")
    with open(p1) as f:
        eq(json.load(f)["height"], 364401, "file on disk parses back to the payload")
    payload2 = dict(payload, height=364500)
    sr.write_telemetry(payload2, tmpdir)
    eq(len(os.listdir(tmpdir)), 1, "a second reading the same day replaces, not stacks")
    with open(p1) as f:
        eq(json.load(f)["height"], 364500, "the later reading wins")

    # 7 consecutive days, one metric up 30% and one up 10%
    base = __import__("datetime").datetime(2026, 11, 25, tzinfo=__import__("datetime").timezone.utc)
    for i in range(7):
        d = (base + __import__("datetime").timedelta(days=i)).strftime("%Y-%m-%d")
        sr.write_telemetry(sr.telemetry_payload(
            height=364401 + i * 100, peers=3, intervals=[10.0, 12.0] if i < 6 else [11.0, 13.2],
            restarts_24h=4,
            jobs=(256, 229, 7), update="none", disk_free_gb=41.5 if i < 6 else 45.65,
            node="openclawassist", now=base + __import__("datetime").timedelta(days=i)), tmpdir)
        # p50 11.0 -> 12.1 (+10%); disk free 41.5 -> 45.65 (+10%)
    text, flags = sr.trend_report(tmpdir)
    eq(len(flags), 0, "a 10% move is not a drift flag")
    eq("trend" in text.lower(), True, "the report prints a trend line")
    eq("2026-11-25" in text and "2026-12-01" in text, True, "the trend names both ends of the week")

    # now make p50 30% worse week over week -> must flag
    shutil.rmtree(tmpdir)
    os.makedirs(tmpdir)
    for i in range(7):
        d = (base + __import__("datetime").timedelta(days=i))
        sr.write_telemetry(sr.telemetry_payload(
            height=364401, peers=3, intervals=[10.0, 10.0] if i == 0 else [13.0, 13.0],
            restarts_24h=4,
            jobs=(256, 229, 7), update="none", disk_free_gb=41.5, node="openclawassist", now=d), tmpdir)
    text, flags = sr.trend_report(tmpdir)
    eq(any("block_interval_p50" in f for f in flags), True,
       "a >25% week-over-week move is flagged, naming the metric")
    eq(any("30" in f or "30.0" in f for f in flags), True, "the flag states the size of the move")

    # fewer than 7 consecutive days -> not a trend yet, and no false flags
    shutil.rmtree(tmpdir)
    os.makedirs(tmpdir)
    for i in range(6):
        d = (base + __import__("datetime").timedelta(days=i))
        sr.write_telemetry(sr.telemetry_payload(
            height=364401, peers=3, intervals=[10.0, 12.0], restarts_24h=4, jobs=(256, 229, 7),
            update="none", disk_free_gb=41.5, node="openclawassist", now=d), tmpdir)
    text, flags = sr.trend_report(tmpdir)
    eq(flags, [], "six days produce no week-over-week flags")
    eq("insufficient" in text.lower() or "6" in text, True, "the report says it is short of a week")

    # a gap in the middle means they are not 7 consecutive days
    shutil.rmtree(tmpdir)
    os.makedirs(tmpdir)
    for i in [0, 1, 2, 4, 5, 6, 7]:
        d = (base + __import__("datetime").timedelta(days=i))
        sr.write_telemetry(sr.telemetry_payload(
            height=364401, peers=3, intervals=[10.0, 10.0] if i == 0 else [13.0, 13.0], restarts_24h=4,
            jobs=(256, 229, 7), update="none", disk_free_gb=41.5, node="openclawassist", now=d), tmpdir)
    text, flags = sr.trend_report(tmpdir)
    eq(flags, [], "a missing day breaks the week — no flags from non-consecutive files")
finally:
    shutil.rmtree(tmpdir, ignore_errors=True)

# ---- verdict wording --------------------------------------------------------
eq(sr.day_index(__import__("datetime").datetime(2026, 11, 25, tzinfo=__import__("datetime").timezone.utc),
                __import__("datetime").datetime(2026, 11, 25, 23, tzinfo=__import__("datetime").timezone.utc)),
   1, "the first day of the soak is day 1")
eq(sr.day_index(__import__("datetime").datetime(2026, 11, 25, tzinfo=__import__("datetime").timezone.utc),
                __import__("datetime").datetime(2026, 12, 1, tzinfo=__import__("datetime").timezone.utc)),
   7, "seven days after the start is day 7")

if fails:
    print("\n".join(fails))
    print(f"{checks - len(fails)} passed, {len(fails)} failed")
    sys.exit(1)
print(f"{checks} checks passed, 0 failed")
