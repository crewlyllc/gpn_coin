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
  scripts/canary-report.py --auto --min-job-id 6600   # row 290: no human picks
  scripts/canary-report.py --provider … --job-id … --out /tmp/x.json --rpc-port 9944
Exit: 0 written, 2 evidence missing/unusable, 3 could not write.

`--auto` (row 290) finds the canary's own provider account from the access
point's `/providers` (the entry whose `status_url`/`channel_url` is loopback) and
takes the newest `Completed` job for it with id > `--min-job-id`. That is the
same evidence as the manual form — it only removes the human who used to read
it — so a release can be bridged without anyone remembering the step.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
import urllib.parse
import urllib.request

DEFAULT_OUT = os.path.expanduser("~/.gpn/update/canary.json")

# --- account forms -------------------------------------------------------
# The same account is published as `0x…` hex by the chain client and as ss58 by
# everything user-facing (row 285). A proof must compare the ACCOUNT, not the
# spelling: `--provider 0x664c…` against a `/jobs` row that says `5ENqSp…` is the
# same provider and used to be rejected as "another provider".

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def _b58decode(s: str) -> bytes:
    n = 0
    for ch in s:
        n = n * 58 + B58.index(ch)  # ValueError on a non-base58 char
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    pad = len(s) - len(s.lstrip("1"))
    return b"\x00" * pad + raw


def canon_account(account: str):
    """32-byte account id as hex, whichever form the caller used; None if unknown."""
    s = str(account or "").strip()
    if not s:
        return None
    if s[:2].lower() == "0x":
        try:
            return bytes.fromhex(s[2:]).hex() if len(s) == 66 else None
        except ValueError:
            return None
    try:
        raw = _b58decode(s)
    except ValueError:
        return None
    if len(raw) == 35:      # 1-byte prefix + 32 + 2-byte checksum
        return raw[1:33].hex()
    if len(raw) == 36:      # 2-byte prefix (e.g. 0x02 0x06)
        return raw[2:34].hex()
    return None


def same_account(a: str, b: str) -> bool:
    ca, cb = canon_account(a), canon_account(b)
    if ca and cb:
        return ca == cb
    return str(a or "").lower() == str(b or "").lower()


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
    with urllib.request.urlopen(f"{ap_url.rstrip('/')}/jobs?limit=1000", timeout=10) as r:
        doc = json.load(r)
    got = doc.get("jobs")
    if not isinstance(got, list):
        raise RuntimeError("/jobs has no jobs list")
    return got


def providers(ap_url: str) -> list:
    with urllib.request.urlopen(f"{ap_url.rstrip('/')}/providers", timeout=10) as r:
        doc = json.load(r)
    got = doc.get("providers")
    if not isinstance(got, list):
        raise RuntimeError("/providers has no providers list")
    return got


def _is_loopback(url: str) -> bool:
    try:
        host = urllib.parse.urlsplit(str(url or "")).hostname or ""
    except ValueError:
        return False
    return host in ("127.0.0.1", "localhost", "::1")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc-port", type=int, default=9944)
    ap.add_argument("--ap-url", default="http://127.0.0.1:8092")
    ap.add_argument("--provider", help="the provider account the job must belong to")
    ap.add_argument("--job-id", type=int, help="a job COMPLETED on this version")
    ap.add_argument("--auto", action="store_true",
                    help="find this host's provider and its newest Completed job (row 290)")
    ap.add_argument("--min-job-id", type=int, default=0,
                    help="with --auto: only a job with a LARGER id counts (one served after the last proof)")
    ap.add_argument("--out", default=DEFAULT_OUT)
    args = ap.parse_args()
    if not args.auto and (not args.provider or args.job_id is None):
        die(2, "give --provider and --job-id, or --auto")

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

    provider = args.provider
    job_id = args.job_id
    if args.auto:
        try:
            plist = providers(args.ap_url)
        except Exception as e:  # noqa: BLE001
            die(2, f"no canary report: could not read providers at {args.ap_url} ({e})")
        local = [p for p in plist
                 if _is_loopback(p.get("status_url")) or _is_loopback(p.get("channel_url"))]
        if not local:
            die(2, f"no canary report: {args.ap_url}/providers lists no loopback provider "
                   "(this host is not a provider to the access point it asked)")
        # Several provider processes may run on the canary host (LLM, images,
        # fine-tune). The proof needs work done on THIS machine, and the update
        # restarts all of them, so any qualifies: take the newest qualifying job
        # across the set rather than refusing as "ambiguous".
        provs = [str(p.get("address") or "") for p in local]
        provs = [p for p in provs if p]
        if not provs:
            die(2, "no canary report: the loopback provider rows have no addresses")
        done = []
        for j in rows:
            if str(j.get("state") or "").lower() != "completed":
                continue
            jprov = str(j.get("provider") or j.get("lease_provider") or "")
            match = next((p for p in provs if same_account(jprov, p)), None)
            if not match:
                continue
            try:
                jid = int(j.get("id"))
            except (TypeError, ValueError):
                continue
            if jid > args.min_job_id:
                done.append((jid, j, match))
        if not done:
            die(2, f"no canary report: no loopback provider has a job Completed after "
                   f"#{args.min_job_id} at {args.ap_url}/jobs — send one, then re-run")
        _, job, provider = max(done, key=lambda t: t[0])
        job_id = int(job.get("id"))

    job = next((j for j in rows if str(j.get("id")) == str(job_id)), None)
    if job is None:
        die(2, f"no canary report: job {job_id} is not in {args.ap_url}/jobs")
    state = str(job.get("state") or "")
    if state.lower() != "completed":
        die(2, f"no canary report: job {job_id} is {state!r}, not completed")
    prov = str(job.get("provider") or job.get("lease_provider") or "")
    if prov and not same_account(prov, provider):
        die(
            2,
            f"no canary report: job {job_id} was served by {prov}, not {provider}",
        )

    report = {
        "version": version,
        "ok": True,
        "job_served": True,
        "job_id": job_id,
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
    print(f"canary proof: job #{job_id} is Completed for {provider}")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
