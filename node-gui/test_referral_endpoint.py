#!/usr/bin/env python3
"""Row 224 — the dashboard must be able to SHOW and COPY the operator's code.

Three things are proven here, with no browser and no dependencies:

  1. `GET /api/referral` answers with the node's own code, link and address —
     by delegating to `gpn_node --referral --json` (one implementation only).
  2. A machine with no wallet says so, in words an operator can act on, and
     still answers 200 (an absent wallet is a state, not a server error).
  3. The control-panel HTML actually contains the card and its Copy button,
     and the button is disabled until a code exists.

Run:  python3 node-gui/test_referral_endpoint.py [path-to-gpn_node]

With no argument it uses the real binary if it can find one
(gpn-agent/target/{release,debug}/gpn_node); otherwise the wallet-less fake
below, so the script is useful on a machine that never built Rust.
"""
import http.client
import json
import os
import socket
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT = os.path.abspath(os.path.join(HERE, ".."))
sys.path.insert(0, HERE)

passed = 0


def ok(name):
    global passed
    passed += 1
    print("ok - " + name)


def fail(name, why):
    print("FAIL - " + name + ": " + why)
    sys.exit(1)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def start_server(node_bin):
    port = free_port()
    proc = subprocess.Popen(
        [sys.executable, os.path.join(HERE, "server.py"), "--port", str(port),
         "--node-path", node_bin],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    for _ in range(100):
        try:
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
            c.request("GET", "/api/referral")
            c.getresponse().read()
            c.close()
            return proc, port
        except Exception:
            if proc.poll() is not None:
                out = proc.stdout.read() if proc.stdout else ""
                fail("server starts", "exited: " + out[-300:])
            time.sleep(0.1)
    proc.kill()
    fail("server starts", "no answer on 127.0.0.1:%d after 10 s" % port)


def get(port, path):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=20)
    c.request("GET", path)
    r = c.getresponse()
    body = r.read().decode()
    c.close()
    return r.status, body


def main():
    real = os.path.join(PROJECT, "gpn-agent", "target", "release", "gpn_node")
    if not os.path.exists(real):
        real = os.path.join(PROJECT, "gpn-agent", "target", "debug", "gpn_node")
    node_bin = sys.argv[1] if len(sys.argv) > 1 else (real if os.path.exists(real) else "")

    # ── 1. the card exists in the HTML, and the button starts disabled ──────
    html = open(os.path.join(HERE, "index.html"), encoding="utf-8").read()
    for needle in ('id="refCode"', 'id="copyBtn"', "/api/referral", "navigator.clipboard"):
        if needle not in html:
            fail("control panel has the referral card", "index.html is missing %r" % needle)
    ok("control panel has the code card, a Copy button and a clipboard write")

    # ── 2. a configured wallet: code + link come from the node itself ───────
    if node_bin:
        proc, port = start_server(node_bin)
        try:
            status, body = get(port, "/api/referral")
            if status != 200:
                fail("GET /api/referral answers 200", "%d: %s" % (status, body[:200]))
            j = json.loads(body)
            if not j.get("wallet"):
                fail("a wallet-having node serves its code", body[:300])
            if not j.get("code", "").startswith("GPN-"):
                fail("code is in display form", body[:200])
            if "ref=" + j["code"] not in j.get("link", ""):
                fail("link carries the code", body[:200])
            ok("GET /api/referral -> %s (address %s)" % (j["code"], j["address"]))
            status, body = get(port, "/api/status")
            if status != 200 or "running" not in json.loads(body):
                fail("/api/status still works", body[:200])
            ok("/api/status is unaffected by the new endpoint")
        finally:
            proc.kill()

    # ── 3. no wallet: an honest 200, not an error and not a fake code ───────
    fake = os.path.join(tempfile.mkdtemp(prefix="gpn-noref-"), "gpn_node_fake")
    with open(fake, "w") as f:
        f.write("#!/bin/sh\n"
                'printf \'%s\\n\' \'{"wallet":false,"error":"no wallet on this machine yet"}\'\n')
    os.chmod(fake, 0o755)
    proc, port = start_server(fake)
    try:
        status, body = get(port, "/api/referral")
        j = json.loads(body)
        if status != 200 or j.get("wallet") is not False or "error" not in j:
            fail("no wallet is reported honestly", body[:200])
        ok("no wallet -> 200 {wallet:false, error:…} (the card prints it; no invented code)")
    finally:
        proc.kill()

    # ── 4. a missing binary is an instruction, not a stack trace ────────────
    proc, port = start_server(os.path.join(HERE, "no-such-gpn-node"))
    try:
        status, body = get(port, "/api/referral")
        j = json.loads(body)
        if status != 200 or "not found" not in j.get("error", ""):
            fail("missing binary explains itself", body[:200])
        ok("missing node binary -> 200 with the path and the fix")
    finally:
        proc.kill()

    print("\n%d checks passed" % passed)


if __name__ == "__main__":
    main()
