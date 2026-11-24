#!/usr/bin/env python3
"""Offline test for scripts/airdrop-contributions.py (GPN-LIST row 252).

Two fake HTTP servers — a Substrate JSON-RPC node and the access point's /jobs —
serve canned storage so the producer's chain decode, AP ledger, SCALE decoding
and fee arithmetic are exercised end to end with no real node, no network.

The assertions are the ones the release box needs:
  * blocks_authored comes from GPUPoW::BlocksAuthored, byte-for-byte;
  * only jobs whose on-chain state is Completed count as served;
  * a job the AP lists but that is still Running counts for nothing;
  * jobs_settled_gpn follows the pallet rule (billing charge capped by the quoted
    price, minus the 5% marketplace fee) and a flat-price job settles at
    price − fee;
  * an account the registry knows but the chain has no contribution for is
    absent, so the export pays it 0.0 (never a registration-count fallback).

Usage: scripts/tests/test-airdrop-contributions.py
"""

import hashlib
import importlib.util
import json
import os
import struct
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
spec = importlib.util.spec_from_file_location("ac", os.path.join(REPO, "scripts", "airdrop-contributions.py"))
ac = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ac)

pass_n = 0
fail_n = 0


def ok(d):
    global pass_n
    pass_n += 1
    print("  \033[32mok\033[0m   " + d)


def bad(d):
    global fail_n
    fail_n += 1
    print("  \033[31mFAIL\033[0m " + d)


def check(d, cond, detail=""):
    (ok if cond else bad)(d + (" — " + detail if detail and not cond else ""))


# ---------------------------------------------------------------- fixture
ALICE = bytes.fromhex("11" * 32)   # wrote blocks, served a completed job
BOB = bytes.fromhex("22" * 32)     # wrote no blocks, served one flat-price job
CAROL = bytes.fromhex("33" * 32)   # registered, did nothing
DAVE = bytes.fromhex("44" * 32)    # AP listed a job that is still Running

ALICE_SS58 = ac.ss58(ALICE)
BOB_SS58 = ac.ss58(BOB)
CAROL_SS58 = ac.ss58(CAROL)


def job_bytes(job_id, state, price_planck):
    """ComputeJob prefix in the pallet's SCALE layout."""
    b = struct.pack("<Q", job_id)
    b += struct.pack("<I", 8000) + struct.pack("<H", 75) + bytes([1])
    b += struct.pack("<I", 0) + struct.pack("<Q", 0)
    b += bytes([0, 1, state])
    b += price_planck.to_bytes(16, "little")
    return "0x" + (b + b"\x00" * 32).hex()


def billing_bytes(price_per_token, max_tokens, actual_tokens):
    return "0x" + (price_per_token.to_bytes(16, "little")
                   + struct.pack("<I", max_tokens) + struct.pack("<I", actual_tokens)).hex()


STORAGE = {}
for who, n in ((ALICE, 1234), (BOB, 0)):
    if n:
        STORAGE[ac.map_key("GPUPoW", "BlocksAuthored", who)] = \
            "0x" + n.to_bytes(8, "little").hex()

# job 1: ALICE, completed, per-token billing charging 2e9 planck, quoted price 512 GPN
STORAGE[ac.map_key("ComputeMarketplace", "Jobs", ac.u64_key(1))] = job_bytes(1, 4, 512 * 10**12)
STORAGE[ac.map_key("ComputeMarketplace", "JobBilling", ac.u64_key(1))] = \
    billing_bytes(10**9, 2, 116)
# job 2: BOB, completed, flat price (no billing row) = 100 GPN
STORAGE[ac.map_key("ComputeMarketplace", "Jobs", ac.u64_key(2))] = job_bytes(2, 4, 100 * 10**12)
# job 3: DAVE, still Running — must not count
STORAGE[ac.map_key("ComputeMarketplace", "Jobs", ac.u64_key(3))] = job_bytes(3, 2, 100 * 10**12)

AP_JOBS = {"from": 1, "jobs": [
    {"id": 1, "provider": "0x" + ALICE.hex(), "state": "Completed"},
    {"id": 2, "provider": "0x" + BOB.hex(), "state": "Completed"},
    {"id": 3, "provider": "0x" + DAVE.hex(), "state": "Running"},
]}


class RpcHandler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        m, p = body["method"], body.get("params", [])
        if m == "chain_getHeader":
            res = {"number": "0x64"}
        elif m == "state_getKeys":
            res = [k for k in STORAGE if k.startswith(p[0])]
        elif m == "state_queryStorageAt":
            res = [{"block": "0x00", "changes": [[k, STORAGE.get(k)] for k in p[0]]}]
        elif m == "state_getStorage":
            res = STORAGE.get(p[0])
        else:
            res = None
        out = json.dumps({"jsonrpc": "2.0", "id": body["id"], "result": res}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


class ApHandler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        out = json.dumps(AP_JOBS).encode() if self.path == "/jobs" else b'{"error":"not found"}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


def serve(handler):
    srv = HTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, "http://127.0.0.1:%d" % srv.server_address[1]


def main():
    rpc_srv, rpc_url = serve(RpcHandler)
    ap_srv, ap_url = serve(ApHandler)
    try:
        rows = ac.collect(ac.Rpc(rpc_url), ap_url)
        a = rows.get("0x" + ALICE.hex(), {})
        b = rows.get("0x" + BOB.hex(), {})
        c = rows.get("0x" + CAROL.hex(), {})
        d = rows.get("0x" + DAVE.hex(), {})

        check("blocks_authored decoded from GPUPoW::BlocksAuthored",
              a.get("blocks_authored") == 1234, str(a))
        check("a completed job counts as served", a.get("jobs_served") == 1, str(a))
        check("a Running job does not count",
              d.get("jobs_served", 0) == 0, str(d))
        # 2e9 charge − 5% fee = 1.9e9 planck
        check("per-token billing settles at charge − 5% fee",
              a.get("jobs_settled_gpn") == 1_900_000_000, str(a))
        check("flat-price job settles at price − 5% fee",
              b.get("jobs_settled_gpn") == 95 * 10**12, str(b))
        check("a chain account with no contribution is not in the rows or zeroed",
              c == {}, str(c))

        # sidecar shape, keyed by the registry's own spelling
        reg = {ALICE_SS58: {"ref": "AAAA1111"}, CAROL_SS58: {"ref": "AAAA1111"}}
        tmpdir = "/tmp/ac-test-%d" % os.getpid()
        os.makedirs(tmpdir, exist_ok=True)
        reg_path = os.path.join(tmpdir, "referral-registry.json")
        with open(reg_path, "w") as f:
            json.dump(reg, f)
        doc, missing = ac.build_doc(rows, list(reg), 100, rpc_url, ap_url)
        check("sidecar carries the v1 schema", doc["schema"] == ac.SCHEMA)
        check("sidecar is keyed by the registry's address spelling",
              ALICE_SS58 in doc["accounts"], json.dumps(doc["accounts"])[:200])
        check("registry address with no contribution is reported as missing",
              missing == [CAROL_SS58], str(missing))
        check("no zero-contribution account is emitted", len(doc["accounts"]) == 2,
              json.dumps(doc["accounts"])[:200])

        # the real CLI writes a file the Rust export can read
        out = os.path.join(tmpdir, "airdrop-contributions.json")
        sys.argv = ["airdrop-contributions.py", "--rpc", rpc_url, "--ap", ap_url,
                    "--registry", reg_path, "--out", out]
        rc = ac.main()
        written = json.load(open(out))
        check("CLI writes the sidecar and exits 0 when something was measured", rc == 0, str(rc))
        check("written sidecar keeps the schema", written["schema"] == ac.SCHEMA)
        check("written sidecar records the fee it applied",
              written["sources"]["fee_bps"] == 500, str(written["sources"]))
    finally:
        rpc_srv.shutdown()
        ap_srv.shutdown()

    print("\n%d passed, %d failed" % (pass_n, fail_n))
    return 1 if fail_n else 0


if __name__ == "__main__":
    sys.exit(main())
