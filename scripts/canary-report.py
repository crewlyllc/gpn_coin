#!/usr/bin/env python3
"""canary-report.py — write the canary proof for the version this node is RUNNING,
backed by two pieces of evidence that cannot be faked from a config file (row 275).

Evidence it collects:

  1. `system_version` over the node's own RPC — the version the *running process*
     reports. Not a file on disk, not the manifest, not a tag.
  2. A job in state `Completed` at the access point's `/jobs`, served by the
     provider account given on the command line. "It started" is not proof; work
     done on the new version is.

If either piece is missing the script writes nothing and exits non-zero. The
proof it writes is what `scripts/canary-publish.sh` publishes, and what every
other node in the fleet waits for before it will install the same release.

Usage:
  scripts/canary-report.py --provider 0x664c… --job-id 6342
  scripts/canary-report.py --provider … --job-id … --out /tmp/x.json --rpc-port 9944
Exit: 0 written, 2 evidence missing/unusable, 3 could not write.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
import urllib.request

DEFAULT_OUT = os.path.expanduser("~/.gpn/update/canary.json")


def die(code: int, msg: str) -> "NoReturn":  # type: ignore[name-defined]
    print(msg, file=sys.stderr)
    raise SystemExit(code)


def rpc(port: int, method: str) -> str:
    body = json.dumps({"id": 1, "jsonrpc": "2.0", "method": method, "params": []}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        doc = json.load(r)
    if "result" not in doc:
        raise RuntimeError(f"{method} answered {doc}")
    return str(doc["result"])


def jobs(ap_url: str) -> list:
    with urllib.request.urlopen(f"{ap_url.rstrip('/')}/jobs", timeout=10) as r:
        doc = json.load(r)
    got = doc.get("jobs")
    if not isinstance(got, list):
        raise RuntimeError("/jobs has no jobs list")
    return got


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc-port", type=int, default=9944)
    ap.add_argument("--ap-url", default="http://127.0.0.1:8092")
    ap.add_argument("--provider", required=True, help="the provider account the job must belong to")
    ap.add_argument("--job-id", type=int, required=True, help="a job COMPLETED on this version")
    ap.add_argument("--out", default=DEFAULT_OUT)
    args = ap.parse_args()

    try:
        version = rpc(args.rpc_port, "system_version").strip().strip('"')
    except Exception as e:  # noqa: BLE001 - any failure here is "no evidence"
        die(2, f"no canary report: the node did not report a version ({e})")
    if not version:
        die(2, "no canary report: system_version was empty")

    try:
        rows = jobs(args.ap_url)
    except Exception as e:  # noqa: BLE001
        die(2, f"no canary report: could not read jobs at {args.ap_url} ({e})")

    job = next((j for j in rows if str(j.get("id")) == str(args.job_id)), None)
    if job is None:
        die(2, f"no canary report: job {args.job_id} is not in {args.ap_url}/jobs")
    state = str(job.get("state") or "")
    if state.lower() != "completed":
        die(2, f"no canary report: job {args.job_id} is {state!r}, not completed")
    prov = str(job.get("provider") or job.get("lease_provider") or "")
    if prov and prov.lower() != args.provider.lower():
        die(
            2,
            f"no canary report: job {args.job_id} was served by {prov}, not {args.provider}",
        )

    report = {
        "version": version,
        "ok": True,
        "job_served": True,
        "job_id": args.job_id,
        "confirmed_at": int(time.time()),
        "failed_at": 0,
        "reason": "",
    }
    out = os.path.expanduser(args.out)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    try:
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(out), prefix=".canary-")
        with os.fdopen(fd, "w") as f:
            json.dump(report, f, indent=2)
            f.write("\n")
        os.replace(tmp, out)
    except OSError as e:
        die(3, f"could not write {out}: {e}")

    print(f"canary proof: {version} confirmed by the running node (system_version)")
    print(f"canary proof: job #{args.job_id} is Completed for {args.provider}")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
