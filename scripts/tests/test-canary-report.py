#!/usr/bin/env python3
"""test-canary-report.py — the evidence half of the canary gate (row 275).

The proof must be built from things that cannot be asserted by editing a config:
the version the RUNNING process reports, and a job COMPLETED at the access point
for this provider. Every missing piece must write nothing and exit non-zero.
"""

from __future__ import annotations

import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "..", "canary-report.py")

PASS = 0
FAIL = 0


def ok(msg):
    global PASS
    PASS += 1
    print(f"  ok   — {msg}")


def bad(msg):
    global FAIL
    FAIL += 1
    print(f"  FAIL — {msg}")


class Stub(http.server.BaseHTTPRequestHandler):
    version = "0.3.80"
    jobs = []
    providers = []
    rpc_down = False

    def log_message(self, *_a):
        pass

    def do_POST(self):
        if self.rpc_down:
            self.send_response(500)
            self.end_headers()
            return
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "result": self.version}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.startswith("/providers"):
            payload = {"providers": self.providers}
        elif self.path.startswith("/jobs"):
            payload = {"from": 0, "jobs": self.jobs}
        else:
            self.send_response(404)
            self.end_headers()
            return
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


PROVIDER = "0x664c6e12d0eff26c646b764f733f0468d44deea02f856ab9f1b629af06993d01"


def run(tmp, *extra, job_id=6342):
    return subprocess.run(
        [sys.executable, SCRIPT, "--rpc-port", str(PORT), "--ap-url", f"http://127.0.0.1:{PORT}",
         "--provider", PROVIDER, "--job-id", str(job_id), "--out", os.path.join(tmp, "canary.json"),
         *extra],
        capture_output=True, text=True,
    )


def run_auto(tmp, *extra, min_job_id=0):
    return subprocess.run(
        [sys.executable, SCRIPT, "--auto", "--min-job-id", str(min_job_id),
         "--rpc-port", str(PORT), "--ap-url", f"http://127.0.0.1:{PORT}",
         "--out", os.path.join(tmp, "canary.json"), *extra],
        capture_output=True, text=True,
    )


srv = http.server.HTTPServer(("127.0.0.1", 0), Stub)
PORT = srv.server_port
threading.Thread(target=srv.serve_forever, daemon=True).start()

tmp = tempfile.mkdtemp()
OUT = os.path.join(tmp, "canary.json")

print("== a completed job for this provider is accepted, with the RUNNING version")
Stub.jobs = [{"id": 6342, "provider": PROVIDER, "state": "Completed"}]
r = run(tmp)
if r.returncode == 0:
    ok("exit 0")
else:
    bad(f"expected exit 0, got {r.returncode}: {r.stderr}")
if os.path.exists(OUT):
    ok("report written")
    st = json.load(open(OUT))
    if st.get("version") == "0.3.80":
        ok("version comes from system_version (the running process)")
    else:
        bad(f"version wrong: {st.get('version')!r}")
    if st.get("ok") is True and st.get("job_served") is True:
        ok("ok + job_served set")
    else:
        bad(f"flags wrong: {st}")
    if st.get("job_id") == 6342:
        ok("the job id is recorded, so the proof names its evidence")
    else:
        bad(f"job_id wrong: {st.get('job_id')!r}")
else:
    bad("no report written")

print("== a job that has NOT completed is refused")
os.path.exists(OUT) and os.remove(OUT)
Stub.jobs = [{"id": 6342, "provider": PROVIDER, "state": "Running"}]
r = run(tmp)
if r.returncode == 2:
    ok("exit 2")
else:
    bad(f"expected exit 2, got {r.returncode}: {r.stdout}{r.stderr}")
if not os.path.exists(OUT):
    ok("nothing written")
else:
    bad("a report was written for an unfinished job")
if "Running" in r.stderr:
    ok("names the state it found")
else:
    bad(f"state not named: {r.stderr}")

print("== a job completed for ANOTHER provider is refused")
Stub.jobs = [{"id": 6342, "provider": "0xdeadbeef", "state": "Completed"}]
r = run(tmp)
if r.returncode == 2 and "0xdeadbeef" in r.stderr:
    ok("exit 2 and names the other provider")
else:
    bad(f"expected exit 2 naming the provider, got {r.returncode}: {r.stderr}")

print("== a job that is not in the list at all is refused")
Stub.jobs = [{"id": 1, "provider": PROVIDER, "state": "Completed"}]
r = run(tmp, job_id=99999)
if r.returncode == 2 and "99999" in r.stderr:
    ok("exit 2 and names the missing job")
else:
    bad(f"expected exit 2, got {r.returncode}: {r.stderr}")

print("== no node answering RPC means no proof at all")
Stub.jobs = [{"id": 6342, "provider": PROVIDER, "state": "Completed"}]
Stub.rpc_down = True
r = run(tmp)
if r.returncode == 2:
    ok("exit 2 when the node does not answer")
else:
    bad(f"expected exit 2, got {r.returncode}: {r.stderr}")
if not os.path.exists(OUT):
    ok("nothing written")
else:
    bad("a report was written without a running version")
Stub.rpc_down = False

print("== row 290: --auto finds this host's provider and its newest job after the baseline")
os.path.exists(OUT) and os.remove(OUT)
SS58 = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj"
Stub.providers = [
    {"address": SS58, "status_url": "http://127.0.0.1:8093",
     "channel_url": "http://127.0.0.1:8095/channel/job"},
    {"address": "5HgbpiTYE27CtBQF7gRWLz6Qt3HBxyi4tR2M75FC1oydH48z",
     "status_url": "http://10.0.0.9:8093", "channel_url": "http://10.0.0.9:8095/channel/job"},
]
Stub.jobs = [
    {"id": 6600, "provider": SS58, "state": "Completed"},        # at/below the baseline
    {"id": 6601, "provider": SS58, "state": "Running"},
    {"id": 6602, "provider": "0xdeadbeef", "state": "Completed"},  # another provider
    {"id": 6604, "provider": SS58, "state": "Completed"},        # the one to pick
]
r = run_auto(tmp, min_job_id=6600)
if r.returncode == 0:
    ok("exit 0")
else:
    bad(f"expected exit 0, got {r.returncode}: {r.stdout}{r.stderr}")
st = json.load(open(OUT)) if os.path.exists(OUT) else {}
if st.get("job_id") == 6604:
    ok("picked the newest Completed job for the loopback provider (#6604)")
else:
    bad(f"picked {st.get('job_id')!r}, expected 6604")
if st.get("job_served") is True and st.get("ok") is True:
    ok("the auto proof is as green as the manual one")
else:
    bad(f"flags wrong: {st}")

print("== row 290: --auto accepts the SAME account in both encodings (hex flag vs ss58 row)")
os.path.exists(OUT) and os.remove(OUT)
Stub.jobs = [{"id": 6604, "provider": SS58, "state": "Completed"}]
r = subprocess.run(
    [sys.executable, SCRIPT, "--rpc-port", str(PORT), "--ap-url", f"http://127.0.0.1:{PORT}",
     "--provider", PROVIDER, "--job-id", "6604", "--out", OUT],
    capture_output=True, text=True,
)
if r.returncode == 0:
    ok("hex 0x664c… matches ss58 5ENqS… — one account, one proof")
else:
    bad(f"rejected an equal account: {r.stderr}")

print("== row 290: several loopback providers on one host are not 'ambiguous'")
os.path.exists(OUT) and os.remove(OUT)
OTHER = "5HgbpiTYE27CtBQF7gRWLz6Qt3HBxyi4tR2M75FC1oydH48z"
Stub.providers = [
    {"address": SS58, "status_url": "http://127.0.0.1:8093"},
    {"address": OTHER, "channel_url": "http://127.0.0.1:8095/channel/job"},
]
Stub.jobs = [
    {"id": 6604, "provider": SS58, "state": "Completed"},
    {"id": 6605, "provider": OTHER, "state": "Completed"},
]
r = run_auto(tmp, min_job_id=6604)
if r.returncode == 0 and json.load(open(OUT)).get("job_id") == 6605:
    ok("picked the newest job on the host (a second provider's #6605)")
else:
    bad(f"expected #6605 on the local sibling provider, got rc={r.returncode}: {r.stdout}{r.stderr}")

print("== row 290: --auto refuses when every job predates the last proof")
os.path.exists(OUT) and os.remove(OUT)
Stub.jobs = [{"id": 6600, "provider": SS58, "state": "Completed"}]
r = run_auto(tmp, min_job_id=6600)
if r.returncode == 2 and "after" in r.stderr:
    ok("exit 2, names the missing post-proof job")
else:
    bad(f"expected exit 2, got {r.returncode}: {r.stderr}")
if not os.path.exists(OUT):
    ok("nothing written — a stale job cannot bridge a release")
else:
    bad("a stale job produced a proof")

print("== row 290: --auto refuses when the access point lists no loopback provider")
Stub.providers = [{"address": SS58, "status_url": "http://10.0.0.9:8093"}]
r = run_auto(tmp, min_job_id=0)
if r.returncode == 2 and "loopback" in r.stderr:
    ok("exit 2, says this host is not a provider here")
else:
    bad(f"expected exit 2, got {r.returncode}: {r.stderr}")
Stub.providers = []

print("== an empty version is not a proof")
Stub.version = ""
r = run(tmp)
if r.returncode == 2:
    ok("exit 2 on an empty system_version")
else:
    bad(f"expected exit 2, got {r.returncode}: {r.stderr}")
Stub.version = "0.3.80"

srv.shutdown()
print()
print(f"{PASS} passed, {FAIL} failed")
sys.exit(0 if FAIL == 0 else 1)
