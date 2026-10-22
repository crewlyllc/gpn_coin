#!/usr/bin/env python3
"""Cause (c): did the chain produce blocks but not include our transaction?

Measures, per job, the wait between `create_job` being submitted and the job
becoming visible on chain (the #49 gate logs "not on chain yet … waiting for
inclusion before claiming" and then "claiming LLM job #N" once it lands).
That wait is what a chat request actually pays minus ~1 s of inference.

Also counts the client-visible failure modes in the same window:
  * `Priority is too low` (1014) — nonce/priority starvation, a pool problem
  * `timed out after`      — the accept_job -> inclusion 300 s timeout
  * `cancelled stale`      — the janitor cancelling Pending jobs
  * `dispatch reverted`    — in-block revert

Usage: inclusion-delay.py /tmp/gpnbt/provider.log [--out report.md]
"""
import argparse
import re
import statistics
import sys
from datetime import datetime

LINE = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)")
WAIT = re.compile(r"job #(\d+) is not on chain yet")
CLAIM = re.compile(r"claiming LLM job #(\d+)")


def pct(v, p):
    s = sorted(v)
    if not s:
        return float("nan")
    k = (len(s) - 1) * p / 100.0
    lo, hi = int(k), min(int(k) + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log")
    ap.add_argument("--out")
    a = ap.parse_args()

    waits = {}          # job -> first "not on chain yet" timestamp
    delays = []
    unresolved = 0
    first_ts = last_ts = None

    for line in open(a.log, errors="replace"):
        m = LINE.match(line)
        if m:
            try:
                ts = datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S").timestamp()
            except ValueError:
                ts = None
        else:
            ts = None
        if ts is not None:
            first_ts = first_ts or ts
            last_ts = ts
        if ts is None:
            continue
        w = WAIT.search(line)
        if w:
            waits.setdefault(w.group(1), ts)
            continue
        c = CLAIM.search(line)
        if c and c.group(1) in waits:
            delays.append((c.group(1), ts - waits.pop(c.group(1))))
    unresolved = len(waits)

    counts = {}
    for line in open(a.log, errors="replace"):
        for key, pat in (("1014", r"Priority is too low"), ("revert", r"dispatch reverted"),
                         ("timeout300", r"timed out after"),
                         ("janitor", r"cancelled stale"),
                         ("orphan", r"adopted untracked")):
            if re.search(pat, line):
                counts[key] = counts.get(key, 0) + 1

    d = [x for _, x in delays]
    L = []
    w = L.append
    w("# Cause (c): create-submit → inclusion wait")
    w("")
    if first_ts:
        w("Window: %s → %s (%.1f h), %d jobs with a measurable wait."
          % (datetime.fromtimestamp(first_ts).strftime("%m-%d %H:%M"),
             datetime.fromtimestamp(last_ts).strftime("%m-%d %H:%M"),
             (last_ts - first_ts) / 3600, len(d)))
    w("")
    if d:
        w("| metric | value |")
        w("|---|---|")
        for label, v in (("p50", pct(d, 50)), ("p75", pct(d, 75)), ("p90", pct(d, 90)),
                         ("p95", pct(d, 95)), ("p99", pct(d, 99)), ("max", max(d)),
                         ("mean", statistics.mean(d))):
            w("| %s | %.1f s |" % (label, v))
        w("| jobs waiting > 30 s | %d (%.0f %%) |"
          % (sum(1 for x in d if x > 30), 100.0 * sum(1 for x in d if x > 30) / len(d)))
        w("| jobs waiting > 60 s | %d (%.0f %%) |"
          % (sum(1 for x in d if x > 60), 100.0 * sum(1 for x in d if x > 60) / len(d)))
        w("| jobs waiting > 110 s (Worker proxy limit) | %d |"
          % sum(1 for x in d if x > 110))
        w("")
        w("Longest waits:")
        w("")
        for job, x in sorted(delays, key=lambda t: -t[1])[:8]:
            w("* job #%s — **%.0f s**" % (job, x))
    else:
        w("no measurable waits (every create was already on chain when claimed)")
    w("")
    w("## Pool / failure classes in the same window")
    w("")
    w("| class | count |")
    w("|---|---|")
    for k, label in (("1014", "1014 Priority is too low"),
                     ("revert", "dispatch reverted"),
                     ("timeout300", "accept_job → inclusion timed out"),
                     ("janitor", "janitor cancelled stale Pending"),
                     ("orphan", "adopted untracked")):
        w("| %s | %d |" % (label, counts.get(k, 0)))
    if unresolved:
        w("")
        w("%d create(s) were still waiting at the end of the window (in flight)." % unresolved)

    text = "\n".join(L)
    print(text)
    if a.out:
        open(a.out, "w").write(text + "\n")


if __name__ == "__main__":
    main()
