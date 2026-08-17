#!/usr/bin/env python3
"""GPN Node Control Panel — backend.

A tiny stdlib-only HTTP server that lets a node operator start/stop the
gpn-node process and see its status from a browser GUI. No dependencies.

Endpoints:
  GET  /            -> the control-panel UI (index.html)
  GET  /api/status  -> running?, head block, peers, gpu, node log tail
  POST /api/start   -> launch gpn-node (dev testnet, mining)
  POST /api/stop    -> stop gpn-node gracefully (SIGTERM)

Run:  python3 server.py [--port 8084] [--node-path <path>]
"""
import json
import os
import signal
import subprocess
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

# --- config ---------------------------------------------------------------
HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT = os.path.abspath(os.path.join(HERE, ".."))
NODE_BIN = os.path.join(PROJECT, "gpn-node", "target", "release", "gpn-node")
LOG_FILE = "/tmp/gpn-node.log"
PORT = 8084

def parse_args():
    global PORT, NODE_BIN
    a = sys.argv[1:]
    if "--port" in a:
        PORT = int(a[a.index("--port") + 1])
    if "--node-path" in a:
        NODE_BIN = a[a.index("--node-path") + 1]

# --- helpers --------------------------------------------------------------
def node_pid():
    """Return the gpn-node pid if running, else None."""
    try:
        out = subprocess.run(["pgrep", "-f", "target/release/gpn-node"],
                             capture_output=True, text=True, timeout=5).stdout
        for line in out.split():
            if line.strip():
                return int(line.strip())
    except Exception:
        pass
    return None

def head_block():
    try:
        r = subprocess.run(
            ["curl", "-s", "-X", "POST", "http://127.0.0.1:9944",
             "-H", "Content-Type: application/json",
             "-d", '{"jsonrpc":"2.0","id":1,"method":"chain_getHeader","params":[]}'],
            capture_output=True, text=True, timeout=5).stdout
        return int(json.loads(r)["result"]["number"], 16)
    except Exception:
        return None

def gpu():
    try:
        out = subprocess.run(["nvidia-smi",
            "--query-gpu=name,power.draw,temperature.gpu,memory.used",
            "--format=csv,noheader"], capture_output=True, text=True, timeout=5).stdout.strip()
        return out
    except Exception:
        return "nvidia-smi unavailable"

def log_tail(n=12):
    try:
        with open(LOG_FILE, "r") as f:
            lines = f.readlines()
        return "".join(lines[-n:])
    except Exception:
        return "(no log yet)"

class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype="application/json"):
        data = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        p = urlparse(self.path).path
        if p in ("/", "/index.html"):
            try:
                with open(os.path.join(HERE, "index.html"), "rb") as f:
                    return self._send(200, f.read(), "text/html")
            except Exception as e:
                return self._send(500, f"index.html missing: {e}")
        if p == "/api/status":
            pid = node_pid()
            return self._send(200, json.dumps({
                "running": pid is not None,
                "pid": pid,
                "head_block": head_block(),
                "gpu": gpu(),
                "log": log_tail(),
            }))
        return self._send(404, json.dumps({"error": "not found"}))

    def do_POST(self):
        p = urlparse(self.path).path
        if p == "/api/start":
            if node_pid():
                return self._send(200, json.dumps({"ok": False, "msg": "already running"}))
            args = [NODE_BIN, "--dev", "--tmp", "--force-authoring", "--rpc-port", "9944"]
            if not os.path.exists(NODE_BIN):
                return self._send(500, json.dumps({"ok": False, "msg": f"node binary not found: {NODE_BIN}"}))
            with open(LOG_FILE, "w") as lf:
                subprocess.Popen(args, stdout=lf, stderr=subprocess.STDOUT,
                                 cwd=os.path.dirname(NODE_BIN))
            time.sleep(6)
            return self._send(200, json.dumps({"ok": True, "pid": node_pid(),
                                               "head_block": head_block()}))
        if p == "/api/stop":
            pid = node_pid()
            if pid is None:
                return self._send(200, json.dumps({"ok": False, "msg": "not running"}))
            try:
                os.kill(pid, signal.SIGTERM)
            except Exception as e:
                return self._send(500, json.dumps({"ok": False, "msg": str(e)}))
            time.sleep(2)
            return self._send(200, json.dumps({"ok": True, "stopped": pid,
                                               "running": node_pid() is not None}))
        return self._send(404, json.dumps({"error": "not found"}))

    def log_message(self, *a):  # quiet
        pass

if __name__ == "__main__":
    parse_args()
    print(f"GPN Node Control Panel → http://127.0.0.1:{PORT}  (node: {NODE_BIN})")
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
