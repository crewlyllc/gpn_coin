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

# ---- effective interval (the stall catcher) ---------------------------------
# Day 1 on 2026-11-25 printed "0.08 s" for 83 blocks in 0.29 h — that is
# 83/1044, i.e. blocks per second, printed as seconds per block. A stall would
# make the real figure LARGE, so the inverted number could never catch one.
eq(round(sr.effective_interval_secs(364404, 1795654020.0, 364487, 1795654020.0 + 1044), 2),
   12.58, "83 blocks in 1044 s is 12.58 s/block, not 0.08")
eq(sr.effective_interval_secs(None, None, 364487, 1795654020.0), None,
   "no previous record is None, not a fabricated number")
eq(sr.effective_interval_secs(364487, 1795654020.0, 364487, 1795654020.0 + 600), None,
   "no forward progress is None (a stall is reported by peers/imports, not 0 s)")
eq(sr.effective_interval_secs(364487, 1795654020.0, 364500, 1795654020.0 + 30), None,
   "a window under min_dt is not measured")
eq(sr.effective_interval_secs(100, 1000.0, 200, 7000.0), 60.0,
   "a 300 s block gap reads as a drift-scale number, not 0.0167")

# a day-2 section must not eat the day-1 section
day2 = "### Day 2 — 2026-11-26 10:00 UTC\n\n- height: **300**"
doc3 = sr.splice_day(doc2, header, day2)
eq(doc3.count("### Day 1"), 1, "adding day 2 keeps day 1")
eq(doc3.count("### Day 2"), 1, "day 2 is written once")
eq("- height: **200**" in doc3, True, "day 1's last reading survives day 2")
eq(doc3.index("### Day 1") < doc3.index("### Day 2"), True, "days stay in order")
eq("protocol text" in doc3, True, "header survives across days")

doc4 = sr.splice_day(doc3, header, "### Day 2 — 2026-11-26 22:00 UTC\n\n- height: **400**")
eq(doc4.count("### Day 2"), 1, "day 2 appears once after a rewrite")
eq("- height: **400**" in doc4, True, "the rewrite is what day 2 shows")
eq("- height: **300**" in doc4, False, "day 2's superseded reading is gone")
eq(doc4.count("### Day 1"), 1, "day 1 is untouched by a day-2 rewrite")
eq("- height: **200**" in doc4, True, "day 1 content still intact")

# three days, out of order on input, land 1-2-3
doc5 = sr.splice_day(doc4, header, "### Day 3 — 2026-11-27 07:00 UTC\n\n- height: **500**")
eq([doc5.index(f"### Day {n}") for n in (1, 2, 3)] ==
   sorted(doc5.index(f"### Day {n}") for n in (1, 2, 3)), True, "three days in ascending order")
eq(doc5.count("### Day"), 3, "exactly three day sections")

# ---- anything that is not a day is not the last day's tail -------------------
# Measured 2026-11-26: a 13:19Z same-day refresh deleted two hand-written `### Note`
# sections because the splitter only knew `### Day ` as a boundary. A refresh must
# replace the day's numbers and touch nothing else.
with_note = sr.splice_day(doc5, header, "### Day 3 — 2026-11-27 08:00 UTC\n\n- height: **501**")
with_note += "\n### Note (2026-11-27) — a hand-written note\n\nkeep me\n"
rewritten = sr.splice_day(with_note, header, "### Day 3 — 2026-11-27 09:00 UTC\n\n- height: **502**")
eq("keep me" in rewritten, True, "a note under a day survives that day's rewrite")
eq(rewritten.count("### Note"), 1, "the note is not duplicated")
eq("- height: **502**" in rewritten, True, "the day is still refreshed")
eq(rewritten.index("### Day 3") < rewritten.index("### Note"), True, "notes stay below the days")

# a level-2 section written above the first day (the baseline) stays above them
baseline = "## Written baseline, day 1\n\n- p50 13.4 s\n"
layered = sr.splice_day(baseline + "\n" + doc5, header, "### Day 3 — 2026-11-27 10:00 UTC\n\n- height: **503**")
eq(layered.index("## Written baseline") < layered.index("### Day 1"), True,
   "a `##` section above the days is not sorted below them")
eq("p50 13.4 s" in layered, True, "the baseline body survives")
eq(layered.count("### Day"), 3, "still three days")

# ---- restart audit: planned stops vs a node that died on its own -------------
# Real shapes, taken from the journal on 2026-11-26 (3 days, 13 events, 12 preceded by
# systemd's Stopping line). A restart column that cannot tell these apart reads every
# stop as a choice and hides a crash.
planned = [
    "2026-11-25 02:57:19 \U0001f4a4 Idle (3 peers), best: #361471",
    "Stopping gpn-fleet.service - GPN fleet supervisor (node on the directive's target network)...",
    "2026-11-25 02:57:19 Essential task `txpool-background` failed. Shutting down service.",
]
eq(sr.restart_audit(planned), (1, 1, 0), "a systemd stop is planned, not a crash")

eq(sr.restart_audit(["2026-11-25 03:18:48 peer 12D3KooWP1 best=#361540 vs ours #361540",
                     "2026-11-25 03:18:48 Essential task `txpool-background` failed. Shutting down service."]),
   (1, 0, 1), "a termination with no Stopping line is unexplained — counted, not assumed away")

eq(sr.restart_audit([]), (0, 0, 0), "no journal is zero of everything, not a false clean")
eq(sr.restart_audit(["Stopping gpn-fleet.service ..."]), (0, 0, 0),
   "a stop with no termination line is not a termination")
# the window is measured on both sides: 12 of 13 planned events put the stop line 1-2 lines
# BEFORE the node's line, the 13th (an updater apply) put it 1 line AFTER
TERM = "Essential task `txpool-background` failed. Shutting down service."
STOP = "Stopping gpn-fleet.service - GPN fleet supervisor (node on the directive's target network)..."
eq(sr.restart_audit([TERM, STOP]), (1, 1, 0),
   "a stop line AFTER the termination is planned too — the updater signals the node "
   "directly, so the node's line is written before systemd records the stop "
   "(measured 2026-11-25 21:05:49)")
eq(sr.restart_audit([STOP, "a", TERM]), (1, 1, 0),
   "a stop 2 lines before is planned (the common suite-stop shape)")
eq(sr.restart_audit(["Stopping gpn-fleet.service ...", "a", "b",
                     "Essential task `txpool-background` failed. Shutting down service."]),
   (1, 0, 1), "a stop 4 lines earlier is outside the measured window")
eq(sr.restart_audit([TERM, "a", "b", "c", STOP]), (1, 0, 1),
   "a stop 4 lines later is outside the window too — the window must not grow into "
   "'any stop in the day means the day was planned'")

# ---- select_prev: a same-day re-run must not destroy the day's effective interval ----
_prev_state = {"last": {"height": 367023, "ts": 1795689648.0},
               "day_first": {"2026-11-26": {"height": 366977, "ts": 1795688520.0}}}
eq(sr.select_prev(_prev_state, "2026-11-26"), {"height": 366977, "ts": 1795688520.0},
   "a re-run on the same day measures from the day's FIRST reading, not the run 2 min ago")
eq(sr.select_prev({"last": {"height": 364401, "ts": 1.0}}, "2026-11-27"),
   {"height": 364401, "ts": 1.0},
   "the first run of a new day measures from the previous record (yesterday's last)")
eq(sr.select_prev({}, "2026-11-25"), {}, "no state at all is an empty prev, not a crash")
eq(sr.effective_interval_secs(366977, 1795688520.0, 367023, 1795689900.0), 1380 / 46,
   "46 blocks in 23 min is 30 s/block — the stall number, not its reciprocal")

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

# ---- update line: the journal is the update outcome, last-exit.json is not (row 281) ----
# Two soak days quoted a fake `corrupt-key` record (written by a TEST) as "the update
# outcome". The recorder must say what the update unit did, and label any exit report
# as a node exit report so the two can never be read as one thing again.
tmpdir = tempfile.mkdtemp(prefix="soak-update-")
try:
    exit_log = os.path.join(tmpdir, "last-exit.json")
    with open(exit_log, "w", encoding="utf-8") as f:
        f.write('{"ts":"2026-11-25T15:33:35Z","ts_unix":1795630415,"code":3,'
                '"kind":"corrupt-key","detail":"older fake record"}\n')
        f.write('{"ts":"2026-11-26T08:03:49Z","ts_unix":1795689829,"code":3,'
                '"kind":"corrupt-key","detail":"keystore /tmp/tmp.pSZHHdyHwe/data/keystore.json"}\n')
    real_exit, real_journal = sr.EXIT_LOG, sr.update_journal_lines
    try:
        sr.EXIT_LOG = exit_log
        sr.update_journal_lines = lambda *a, **k: [
            "Starting gpn-update.service - GPN signed-release updater (one check + apply)...",
            "  confirm: process=up rpc=yes 24s version=0.3.80",
            "  confirmed 0.3.80 is running",
            "Finished gpn-update.service - GPN signed-release updater (one check + apply)."]
        got = sr.update_outcome()
        joined = "; ".join(got)
        eq("gpn-update.service" in joined, True, "the update outcome quotes the update unit")
        eq("confirmed 0.3.80 is running" in joined, True, "the update outcome carries the real result line")
        eq("1 fire(s)" in joined, True, "the outcome counts the fires, so 'fired' != 'did something'")
        eq("Starting gpn-update.service" in joined, False,
           "systemd's Starting line is not quoted as the outcome")
        eq("NOT an update result" in joined, True,
           "the node exit report is labelled as one, not as the update outcome")
        eq(joined.index("gpn-update.service") < joined.index("corrupt-key"), True,
           "the update unit is stated before any node exit report")
        eq("older fake record" in joined, False, "only the newest exit record is quoted")

        # a day of fires that changed nothing must not read as an outcome it did not have
        sr.update_journal_lines = lambda *a, **k: [
            "Starting gpn-update.service - GPN signed-release updater (one check + apply)...",
            "Finished gpn-update.service - GPN signed-release updater (one check + apply).",
            "gpn-update.service: Consumed 1.2s CPU time.",
            "Starting gpn-update.service - GPN signed-release updater (one check + apply)...",
            "Finished gpn-update.service - GPN signed-release updater (one check + apply)."]
        quiet = "; ".join(sr.update_outcome())
        eq("2 fire(s)" in quiet, True, "quiet fires are counted")
        eq("up to date" in quiet, True, "quiet fires are described as up to date, not as a change")
    finally:
        sr.EXIT_LOG, sr.update_journal_lines = real_exit, real_journal

    # no journal available (not a systemd host) => say nothing rather than invent
    try:
        sr.EXIT_LOG = os.path.join(tmpdir, "absent.json")
        sr.update_journal_lines = lambda *a, **k: None
        eq(sr.update_outcome(), ["no update activity in the last 24 h"],
           "no journal and no log is 'no activity', not a claim")
    finally:
        sr.EXIT_LOG, sr.update_journal_lines = real_exit, real_journal

    # newest record wins even when the file is out of order
    got = sr.newest_exit_record(exit_log)
    eq(got[1]["detail"].startswith("keystore /tmp/tmp.pSZHHdyHwe"), True,
       "newest_exit_record picks the newest ts_unix, not the last line")
    eq(sr.newest_exit_record(os.path.join(tmpdir, "nope.json")), None,
       "a missing exit log is None, not an exception")
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
