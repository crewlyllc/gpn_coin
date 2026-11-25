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
