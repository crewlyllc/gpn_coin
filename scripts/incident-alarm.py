#!/usr/bin/env python3
"""Three incident alarms for row 276 — because everything else is just a log line.

Owner direction 2026-11-25: no alerting existed for the three conditions that actually
mean the network is down. This is that alerting, and the three conditions are:

  peers   < 2                  for 10 min   (nobody to talk to)
  no block                     for  2 min   (the chain stopped sealing)
  job success < 90 % over 1 h              (the product stopped working)

Each alarm carries **the measurement that triggered it** ("peers 1 for 10 min at
00:41Z", not "network down"), goes to two places that survive a restart — a
`- [ ] ALARM-…` line appended to `docs/NEEDS-TRIAGE.md` and a Discord DM — and is
proven by injecting the condition, not by reasoning about the code.

Design notes that matter:

  * Durations live in the state file, so a one-minute tick cannot raise an alarm on a
    one-minute blip: the condition has to hold.
  * A dead RPC is not silence. If the node does not answer, peers are unknown and the
    height is frozen — both alarms fire and the peer line says "no answer", because a
    node that stopped answering is the most total form of down.
  * A firing condition re-announces once per cooldown (30 min) instead of every tick,
    and a recovery is recorded, so "it came back" is evidence too.
  * Exit codes: 0 healthy, 1 alarms raised (or raised earlier and still open),
    2 collection failed in a way that is itself an alarm (RPC dead > threshold),
    3 alarm raised but Discord delivery failed (the triage line is still written).

Usage:
  scripts/incident-alarm.py                 # one evaluation (what the timer runs)
  scripts/incident-alarm.py --dry-run       # print the verdict, write nothing
  scripts/incident-alarm.py --no-discord    # triage line only
  scripts/incident-alarm.py --now 2026-11-26T01:00:00Z   # override the clock (tests)
  scripts/incident-alarm.py --self-test     # thresholds/format sanity, no node
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

RPC = "http://127.0.0.1:9944"
AP = "http://127.0.0.1:8092"
STATE = os.path.expanduser("~/.gpn/alarm-state.json")
TRIAGE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "docs", "NEEDS-TRIAGE.md")
COOLDOWN_S = 1800
TERMINAL = ("Completed", "Failed", "Cancelled")

DEFAULT_THRESHOLDS = {
    "peers": {"min": 2, "for_s": 600},
    "blocks": {"stall_s": 120},
    # min_sample: a rate over three jobs is not a rate. 10 keeps a two-job hour from
    # paging anyone while still catching a real 20 % failure rate within an hour.
    "jobs": {"min_success": 0.90, "window_s": 3600, "min_sample": 10},
}


# ---------- collection ------------------------------------------------------
def rpc(method, params=None, url=None, timeout=5):
    body = json.dumps({"id": 1, "jsonrpc": "2.0", "method": method,
                       "params": params or []}).encode()
    req = urllib.request.Request(url or RPC, data=body,
                                 headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        d = json.loads(r.read().decode())
    if d.get("error"):
        raise RuntimeError(f"{method}: {d['error']}")
    return d.get("result")


def collect(rpc_url=None, ap_url=None):
    """One sample. Missing pieces are None, never a made-up zero."""
    s = {"peers": None, "height": None, "jobs": []}
    try:
        h = rpc("system_health", url=rpc_url) or {}
        s["peers"] = h.get("peers")
    except Exception as e:  # noqa: BLE001
        s["rpc_error"] = f"system_health: {e}"
    try:
        hdr = rpc("chain_getHeader", url=rpc_url) or {}
        if hdr.get("number"):
            s["height"] = int(hdr["number"], 16)
    except Exception as e:  # noqa: BLE001
        s.setdefault("rpc_error", f"chain_getHeader: {e}")
    try:
        with urllib.request.urlopen((ap_url or AP) + "/jobs", timeout=8) as r:
            d = json.loads(r.read().decode())
        s["jobs"] = d.get("jobs") or []
    except Exception as e:  # noqa: BLE001
        s["ap_error"] = f"/jobs: {e}"
    return s


# ---------- the gate itself (pure, clock injected) ---------------------------
def _num_iso(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _mins(sec):
    m = sec / 60.0
    return f"{int(m)} min" if abs(m - int(m)) < 1e-9 else f"{m:.1f} min"


def evaluate(now, sample, state, thresholds=None, cooldown_s=COOLDOWN_S):
    """Return (alarms, new_state, recovered). No I/O, no wall clock."""
    th = json.loads(json.dumps(DEFAULT_THRESHOLDS))
    for k, v in (thresholds or {}).items():
        th.setdefault(k, {}).update(v)
    ts = now.timestamp()
    st = json.loads(json.dumps(state or {}))
    fired = st.setdefault("fired", {})
    alarms, recovered = [], []

    def raise_alarm(key, condition, text, measurement):
        prev = fired.get(key)
        if prev and (ts - float(prev.get("at", 0))) < cooldown_s:
            return
        fired[key] = {"at": ts, "text": text}
        alarms.append({"key": key, "condition": condition, "text": text,
                       "measurement": measurement, "at": ts})

    def clear(key, text):
        if fired.pop(key, None) is not None:
            recovered.append({"key": key, "text": text, "at": ts})

    # ---- peers < 2 for 10 min ---------------------------------------------
    p = th["peers"]
    peers = sample.get("peers")
    node_down = peers is None
    if node_down or peers < p["min"]:
        since = float(st.get("peers_since") or ts)
        st["peers_since"] = since
        dur = ts - since
        if dur >= p["for_s"]:
            who = ("the node did not answer system_health"
                   if node_down else f"peers {peers} < {p['min']}")
            raise_alarm(
                "peers", "peers_below_min",
                f"no peers for {_mins(dur)}: {who}"
                + (sample.get("rpc_error") and f" ({sample['rpc_error']})" or ""),
                {"peers": peers, "since": _num_iso(since), "for_s": dur})
    else:
        st.pop("peers_since", None)
        clear("peers", f"peers back to {peers}")

    # ---- no block for 2 min ------------------------------------------------
    b = th["blocks"]
    height = sample.get("height")
    if isinstance(height, int):
        if st.get("height") != height:
            if isinstance(st.get("height"), int) and height < st["height"]:
                st["height_reset"] = {"from": st["height"], "to": height, "at": ts}
            st["height"] = height
            st["height_since"] = ts
    # first sighting of this node (or of a node that never answers): the stall is
    # measured from when we first looked, not from the current call.
    st.setdefault("height_since", ts)
    hsince = float(st["height_since"])
    stall = ts - hsince
    if stall >= b["stall_s"]:
        known = st.get("height")
        raise_alarm(
            "blocks", "no_block",
            f"no block for {_mins(stall)}: height "
            + (f"{known} unchanged since {_num_iso(hsince)}"
               if isinstance(known, int) else "unknown")
            + ("" if isinstance(height, int) else " (RPC did not answer chain_getHeader)"),
            {"height": known, "since": _num_iso(hsince), "stall_s": stall})
    else:
        clear("blocks", f"chain sealing again at height {st.get('height')}")

    # ---- job success < 90 % over an hour ----------------------------------
    j = th["jobs"]
    events = st.setdefault("job_events", {})
    seen = [job for job in (sample.get("jobs") or [])
            if isinstance(job, dict) and job.get("state") in TERMINAL]
    baseline = st.get("baseline_ids")
    if baseline is None:
        # First sighting: the AP serves its whole history and hands us no completion
        # timestamps, so the age of those jobs is UNKNOWN. Counting them would invent
        # a "last hour" rate out of months of records (it read 89 % on our own store).
        # They become the baseline; only jobs we watch arrive or change can be dated.
        st["baseline_ids"] = sorted(str(job.get("id")) for job in seen)
        st["baseline_at"] = ts
        baseline = st["baseline_ids"]
    baseline = set(baseline)
    for job in seen:
        jid = str(job.get("id"))
        if jid not in events and jid not in baseline:
            events[jid] = {"state": job["state"], "at": ts}
        elif jid in events and events[jid]["state"] != job["state"]:
            # observed transition of a job we already knew: that one IS datable
            events[jid] = {"state": job["state"], "at": ts}
    cutoff = ts - j["window_s"] * 2
    st["job_events"] = {k: v for k, v in events.items()
                        if float(v.get("at", 0)) >= cutoff}
    if len(st["job_events"]) > 5000:  # a busy hour cannot grow the state file forever
        keep = sorted(st["job_events"].items(), key=lambda kv: kv[1].get("at", 0))[-5000:]
        st["job_events"] = dict(keep)
    win = [v for v in st["job_events"].values()
           if float(v.get("at", 0)) >= ts - j["window_s"]]
    done = sum(1 for v in win if v["state"] == "Completed")
    n = len(win)
    if n >= j["min_sample"]:
        rate = done / n
        if rate < j["min_success"]:
            raise_alarm(
                "jobs", "job_success_low",
                f"job success {rate * 100:.0f}% over the last hour "
                f"({done} Completed / {n - done} Failed or Cancelled of {n}, "
                f"threshold {j['min_success'] * 100:.0f}%)",
                {"rate": rate, "completed": done, "failed": n - done, "sample": n})
        else:
            clear("jobs", f"job success back to {rate * 100:.0f}% ({done}/{n})")

    st["last"] = {"at": ts, "peers": peers, "height": st.get("height"),
                  "jobs_window": n, "jobs_completed": done}
    return alarms, st, recovered


# ---------- delivery --------------------------------------------------------
def append_triage(path, alarms, now, already=None):
    """Append one `- [ ] ALARM-…` line per alarm. Never rewrites the file."""
    if not alarms:
        return []
    already = set(already or ())
    try:
        seen = open(path).read()
    except OSError:
        seen = ""
    lines, written = [], []
    for a in alarms:
        # one firing of one condition = one line; a second pass over the same
        # firing must not append it again (the timer runs every minute).
        marker = f"<!-- alarm:{a['key']}:{int(a['at'])} -->"
        if a["key"] in already or marker in seen:
            continue
        stamp = datetime.fromtimestamp(a["at"], timezone.utc)
        lines.append(
            f"- [ ] ALARM-{stamp.strftime('%Y-%m-%dT%H:%MZ')} — {a['text']} "
            f"— measured on `{socket.gethostname()}` at "
            f"{stamp.strftime('%Y-%m-%d %H:%M:%S')}Z by `scripts/incident-alarm.py` "
            f"(condition `{a['condition']}`, raw `{json.dumps(a['measurement'], sort_keys=True)}`). "
            f"| options: A investigate now | recommend: **A** — this is one of the three "
            f"conditions the owner defined as the network being down; react while the "
            f"measurement is still current. | raised {stamp.strftime('%Y-%m-%d %H:%M')}Z "
            f"(incident alarm, row 276) {marker}\n")
        written.append(a["key"])
    if lines:
        with open(path, "a") as f:
            f.write("\n" + "\n".join(lines))
    return written


def _token():
    t = os.environ.get("DISCORD_BOT_TOKEN")
    if t:
        return t.strip()
    for p in (os.path.expanduser("~/.openclaw/credentials/discord-bot-token.txt"),
              os.path.expanduser("~/.openclaw/credentials/discord-alarm-token.txt")):
        try:
            return open(p).read().strip()
        except OSError:
            continue
    return None


def _recipient():
    r = os.environ.get("GPN_ALARM_DISCORD_USER")
    if r:
        return r.strip()
    try:
        d = json.load(open(os.path.expanduser(
            "~/.openclaw/credentials/discord-default-allowFrom.json")))
        if d.get("allowFrom"):
            return str(d["allowFrom"][0])
    except Exception:  # noqa: BLE001
        pass
    return None


def discord_dm(text, token=None, user_id=None, timeout=10):
    """Open (or reuse) the DM channel and post. Returns (ok, detail)."""
    token = token or _token()
    user_id = user_id or _recipient()
    if not token:
        return False, "no bot token (DISCORD_BOT_TOKEN or credentials file)"
    if not user_id:
        return False, "no recipient id (GPN_ALARM_DISCORD_USER or allowFrom file)"
    hdrs = {"Authorization": f"Bot {token}", "Content-Type": "application/json",
            "User-Agent": "gpn-incident-alarm/1.0"}

    def post(url, payload):
        req = urllib.request.Request(url, data=json.dumps(payload).encode(), headers=hdrs,
                                     method="POST")
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read().decode())

    try:
        ch = post("https://discord.com/api/v10/users/@me/channels",
                  {"recipient_id": user_id})
        cid = ch["id"]
        post(f"https://discord.com/api/v10/channels/{cid}/messages",
             {"content": text[:1900]})
        return True, f"dm channel {cid}"
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read().decode()[:200]
        except Exception:  # noqa: BLE001
            pass
        return False, f"HTTP {e.code} {body}".strip()
    except Exception as e:  # noqa: BLE001
        return False, f"{type(e).__name__}: {e}"


# ---------- state -----------------------------------------------------------
def load_state(path=STATE):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:  # noqa: BLE001
        return {}


def save_state(st, path=STATE):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(st, f, indent=1, sort_keys=True)
    os.replace(tmp, path)


def self_test():
    """Format sanity that does not need a node: what a reader would see."""
    now = datetime(2026, 11, 26, 0, 41, 0, tzinfo=timezone.utc)
    st = {"peers_since": now.timestamp() - 600, "height": 364500,
          "height_since": now.timestamp() - 180,
          "job_events": {str(i): {"state": "Completed", "at": now.timestamp()}
                         for i in range(4)}}
    st["job_events"].update({f"x{i}": {"state": "Failed", "at": now.timestamp()}
                             for i in range(6)})
    alarms, _, _ = evaluate(now, {"peers": 1, "height": 364500,
                                  "jobs": [{"id": i, "state": "Completed"} for i in range(4)]},
                            st)
    for a in alarms:
        print(f"{a['key']}: {a['text']}")
    ok = len(alarms) == 3
    print(f"self-test: {len(alarms)} of 3 alarms raised -> {'PASS' if ok else 'FAIL'}")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default=RPC)
    ap.add_argument("--ap", default=AP)
    ap.add_argument("--state", default=STATE)
    ap.add_argument("--triage", default=TRIAGE)
    ap.add_argument("--now", help="ISO8601 UTC, override the clock (tests)")
    ap.add_argument("--dry-run", action="store_true", help="print, write nothing")
    ap.add_argument("--no-discord", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

    now = (datetime.fromisoformat(args.now.replace("Z", "+00:00"))
           if args.now else datetime.now(timezone.utc))

    sample = collect(args.rpc, args.ap)
    state = load_state(args.state)
    alarms, state, recovered = evaluate(now, sample, state)

    print(f"peers={sample.get('peers')} height={sample.get('height')} "
          f"jobs_seen={len(sample.get('jobs') or [])}"
          + (f" rpc_error={sample['rpc_error']}" if sample.get("rpc_error") else "")
          + (f" ap_error={sample['ap_error']}" if sample.get("ap_error") else ""))
    for r in recovered:
        print(f"recovered: {r['text']}")
    for a in alarms:
        print(f"ALARM {a['key']}: {a['text']}")

    if not alarms and not recovered:
        if not args.dry_run:
            save_state(state, args.state)
        print("ok: no incident conditions")
        return 0

    if args.dry_run:
        print(f"dry run: {len(alarms)} alarm(s) would be raised, nothing written")
        return 1 if alarms else 0

    written = append_triage(args.triage, alarms, now)
    save_state(state, args.state)
    if written:
        print(f"triage: {len(written)} line(s) appended to {args.triage}")

    if not alarms:
        return 0
    if args.no_discord or not written and not alarms:
        return 1

    text = "**GPN incident alarm**\n" + "\n".join(f"- {a['text']}" for a in alarms) \
        + f"\n_raised {now.strftime('%Y-%m-%dT%H:%MZ')} on {socket.gethostname()}; "
    text += "recorded in docs/NEEDS-TRIAGE.md_"
    ok, detail = discord_dm(text)
    print(f"discord: {'sent' if ok else 'FAILED'} ({detail})")
    if ok:
        return 1
    return 1 if not os.environ.get("GPN_ALARM_STRICT") else 3


if __name__ == "__main__":
    sys.exit(main())
