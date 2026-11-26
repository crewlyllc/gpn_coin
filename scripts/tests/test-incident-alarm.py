#!/usr/bin/env python3
"""Tests for scripts/incident-alarm.py (row 276).

Three conditions, each with a duration, so the state machine — not the threshold —
is what these tests pin:

  * peers   < 2     for 10 min
  * no block        for 2 min
  * job success < 90 % over 1 h

Every case is a clock and a sample; no node, no AP, no network. Run:
  python3 scripts/tests/test-incident-alarm.py
"""
import importlib.util
import json
import os
import sys
import tempfile
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(os.path.dirname(HERE), "incident-alarm.py")

spec = importlib.util.spec_from_file_location("incident_alarm", SRC)
ia = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ia)

fails = []
checks = 0


def eq(got, want, label):
    global checks
    checks += 1
    if got != want:
        fails.append(f"FAIL {label}: got {got!r}, want {want!r}")


def ok(cond, label):
    eq(bool(cond), True, label)


T0 = datetime(2026, 11, 26, 0, 0, 0, tzinfo=timezone.utc)


def sample(peers=3, height=364500, jobs=None, mins=0):
    """A sample at minute `mins`; the height advances unless a test freezes it."""
    return {"peers": peers, "height": None if height is None else height + mins,
            "jobs": jobs or []}


def run(samples):
    """Feed (offset_minutes, sample) pairs through the gate, returning all alarms."""
    state, out = {}, []
    for mins, s in samples:
        alarms, state, recovered = ia.evaluate(T0 + timedelta(minutes=mins), s, state)
        out.append((alarms, recovered, state))
    return out


# ---- 1. a healthy node raises nothing ---------------------------------------
res = run([(0, sample()), (1, sample(mins=1)), (2, sample(mins=2))])
eq([a for alarms, _, _ in res for a in alarms], [], "healthy node: no alarm")
eq([r for _, rec, _ in res for r in rec], [], "healthy node: nothing to recover")

# ---- 2. peers < 2 must hold for 10 MINUTES, not 10 seconds ------------------
res = run([(0, sample(peers=1)), (9, sample(peers=1, mins=9))])
eq([a for alarms, _, _ in res for a in alarms], [],
   "9 min at 1 peer: no alarm yet (the duration is the threshold)")
alarms = run([(0, sample(peers=1)), (10, sample(peers=1, mins=10))])[-1][0]
eq([a["key"] for a in alarms], ["peers"],
   "10 min at 1 peer: the peer alarm and nothing else")
eq(alarms[0]["key"], "peers", "peer alarm names its condition")
ok("1" in alarms[0]["text"] and "10 min" in alarms[0]["text"],
   f"peer alarm quotes the measurement it triggered on: {alarms[0]['text']!r}")

# ---- 3. recovery is recorded, and the timer restarts ------------------------
res = run([(0, sample(peers=1)), (10, sample(peers=1, mins=10)), (11, sample(peers=3, mins=11)),
           (12, sample(peers=1, mins=12)), (15, sample(peers=1, mins=15))])
eq([r["key"] for r in res[2][1]], ["peers"], "peers back: a recovery line is emitted")
eq(res[4][0], [], "peer timer restarted after recovery: 3 min at 1 peer is not yet 10")

# ---- 4. no block for 2 min --------------------------------------------------
res = run([(0, sample()), (2, sample())])
eq([a["key"] for a in res[1][0]], ["blocks"], "height frozen 2 min: the block alarm")
eq(res[1][0][0]["key"], "blocks", "block alarm names its condition")
ok("364500" in res[1][0][0]["text"], f"block alarm quotes the frozen height: {res[1][0][0]['text']!r}")
res = run([(0, sample()), (1, sample())])
eq(res[1][0], [], "height frozen 1 min: below threshold, no alarm")
res = run([(0, sample()), (5, sample(mins=5))])
eq(res[1][0], [], "height advancing: no alarm even after 5 min")

# ---- 5. a dead RPC is a stall, not silence ----------------------------------
st = {}
alarms, st, _ = ia.evaluate(T0, {"peers": None, "height": None, "jobs": [],
                                 "rpc_error": "system_health: refused"}, st)
eq(alarms, [], "RPC dead at T0: nothing is old enough yet, and nothing is invented")
alarms, st, _ = ia.evaluate(T0 + timedelta(minutes=2), {"peers": None, "height": None,
                                                        "jobs": [], "rpc_error": "refused"}, st)
eq([a["key"] for a in alarms], ["blocks"],
   "RPC dead 2 min: the block alarm fires (2 min is its threshold)")
ok("did not answer" in alarms[0]["text"],
   f"dead-RPC block alarm says the RPC did not answer: {alarms[0]['text']!r}")
alarms, st, _ = ia.evaluate(T0 + timedelta(minutes=10), {"peers": None, "height": None,
                                                         "jobs": []}, st)
eq([a["key"] for a in alarms], ["peers"], "RPC dead 10 min: the peer alarm joins in")
ok("did not answer system_health" in alarms[0]["text"],
   "dead-RPC peer alarm says the node did not answer, not that it had 0 peers")

# ---- 6. job success rate over an hour --------------------------------------
def jobs(n_done, n_bad, start_id=7000):
    out = [{"id": start_id + i, "state": "Completed"} for i in range(n_done)]
    out += [{"id": start_id + n_done + i, "state": "Failed"} for i in range(n_bad)]
    return out


def job_case(n_done, n_bad, start=7000):
    """Baseline first (the AP's history has no timestamps), then a fresh batch."""
    _, st, _ = ia.evaluate(T0, sample(jobs=[{"id": 1, "state": "Completed",
                                             "old": True}]), {})
    return ia.evaluate(T0 + timedelta(minutes=1), sample(mins=1, jobs=jobs(n_done, n_bad, start)),
                       st)[0]


eq(job_case(9, 1), [], "90 % exactly: not below 90 %, no alarm")
a = job_case(8, 2)
eq(len(a), 1, "80 % success: one alarm")
eq(a[0]["key"], "jobs", "job alarm names its condition")
ok("80%" in a[0]["text"].replace(" ", ""), f"job alarm quotes the rate: {a[0]['text']!r}")
eq(job_case(1, 1), [], "2 jobs is too small a sample to call a rate")
eq(job_case(5, 4), [], "9 jobs is still below the 10-job sample floor — no rate is called")

# the AP serves its whole history with no completion timestamps; those jobs are
# undateable, so the first sighting must not be read as "the last hour"
res = run([(0, sample(jobs=jobs(3, 7)))])
eq(res[0][0], [], "first sighting of the job store: no rate, its ages are unknown")
ok(res[0][2].get("baseline_ids") is not None, "the first sighting becomes the baseline")

# ---- 7. jobs leave the hourly window ---------------------------------------
_, st, _ = ia.evaluate(T0, sample(jobs=[{"id": 1, "state": "Completed"}]), {})
alarms, st, _ = ia.evaluate(T0 + timedelta(minutes=1),
                            sample(mins=1, jobs=jobs(5, 5, 8000)), st)
eq(len([x for x in alarms if x["key"] == "jobs"]), 1, "5 of 10 failing: the job alarm fires")
alarms, st, _ = ia.evaluate(T0 + timedelta(minutes=123), sample(mins=123, jobs=[]), st)
eq(alarms, [], "jobs older than the 1 h window stop counting (no stale rate)")
eq(st["job_events"], {}, "expired job events are pruned from state, not accumulated forever")

# ---- 8. one alarm per cooldown, not one per minute --------------------------
res = run([(0, sample(peers=1)), (10, sample(peers=1, mins=10)),
           (11, sample(peers=1, mins=11)), (12, sample(peers=1, mins=12))])
eq(len([a for alarms, _, _ in res for a in alarms if a["key"] == "peers"]), 1,
   "a condition that persists fires once per cooldown, not every tick")
res = run([(0, sample(peers=1)), (10, sample(peers=1, mins=10)),
           (41, sample(peers=1, mins=41))])
eq(len([a for alarms, _, _ in res for a in alarms if a["key"] == "peers"]), 2,
   "still down after the cooldown: it says so again instead of going quiet")

# ---- 9. the triage file gets one line per alarm, with the measurement ------
tmp = tempfile.mkdtemp()
triage = os.path.join(tmp, "NEEDS-TRIAGE.md")
open(triage, "w").write("# NEEDS-TRIAGE\n\n")
alarms, st, _ = ia.evaluate(T0 + timedelta(minutes=10), sample(peers=1, mins=10),
                            ia.evaluate(T0, sample(peers=1), {})[1])
wrote = ia.append_triage(triage, alarms, T0 + timedelta(minutes=10))
body = open(triage).read()
eq(len(wrote), 1, "append_triage reports what it wrote")
ok("- [ ] ALARM-" in body, "triage line carries the ALARM marker")
ok("1" in body and "peers" in body, "triage line carries the measurement, not a bare 'down'")
ok(body.startswith("# NEEDS-TRIAGE"), "existing triage content is preserved, not overwritten")
eq(ia.append_triage(triage, alarms, T0 + timedelta(minutes=10)), [],
   "re-appending an already-recorded alarm key is a no-op")
eq(open(triage).read().count("ALARM-"), 1, "no duplicate alarm line in the triage file")

# ---- 10. thresholds are data, not scattered constants ----------------------
_, st, _ = ia.evaluate(T0, sample(peers=1), {})
alarms, _, _ = ia.evaluate(T0 + timedelta(minutes=3), sample(peers=1, mins=3), st,
                           thresholds={"peers": {"min": 2, "for_s": 120}})
eq([a["key"] for a in alarms], ["peers"],
   "a tightened peer threshold is honoured (testability of the real gate)")

print(f"{checks} checks run, {len(fails)} failed")
for f in fails:
    print(f)
sys.exit(1 if fails else 0)
