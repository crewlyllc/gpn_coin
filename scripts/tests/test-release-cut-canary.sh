#!/usr/bin/env bash
# test-release-cut-canary.sh — row 290's filing gate.
#
# `release-cut.sh --verify-published` (§8, and the gate the filing step runs) must
# refuse a release whose canary proof still names the PREVIOUS version — the
# 0.3.82/0.3.83 shape, where the site shipped and the fleet stayed behind. It
# must say so BEFORE fetching 300 MB of assets.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$(cd "$HERE/../.." && pwd)"
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  ok   — $1"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL — $1"; }

TMP="$(mktemp -d)"
STUB_PID=""
cleanup() { [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

# A stub site: releases.json / canary.json are the only things read before the
# gate's verdict, so nothing here has to serve an asset.
cat > "$TMP/stub.py" <<'PY'
import http.server, json, os, sys
STATE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "state.json")
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_a): pass
    def do_GET(self):
        st = json.load(open(STATE))
        p = self.path.split("?")[0]
        if p == "/releases.json":
            payload = {"version": st["site"], "min_version": "0.3.0",
                       "assets": [{"os": "linux", "url": "http://127.0.0.1:1/x", "sha256": "0"*64, "size": 1}]}
        elif p == "/canary.json":
            payload = {"version": st["canary"], "ok": True, "job_served": True, "job_id": 1}
        else:
            self.send_response(404); self.end_headers(); return
        body = json.dumps(payload).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body))); self.end_headers()
        self.wfile.write(body)
srv = http.server.HTTPServer(("127.0.0.1", 0), H)
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "port"), "w").write(str(srv.server_port))
srv.serve_forever()
PY

echo "{\"site\":\"0.3.84\",\"canary\":\"0.3.83\"}" > "$TMP/state.json"
python3 "$TMP/stub.py" & STUB_PID=$!
for _ in $(seq 1 50); do [ -f "$TMP/port" ] && break; sleep 0.1; done
PORT="$(cat "$TMP/port")"

new_repo() {
    rm -rf "$TMP/repo"; mkdir -p "$TMP/repo/scripts" "$TMP/repo/website"
    cp "$SRC/scripts/release-cut.sh" "$SRC/scripts/gen-release-manifest.py" \
       "$SRC/scripts/commit-paths.sh" "$SRC/scripts/release-lock.sh" "$TMP/repo/scripts/"
    echo '{"version":"0.3.83","assets":[]}' > "$TMP/repo/website/releases.json"
    git init -q "$TMP/repo"
    git -C "$TMP/repo" config user.name t; git -C "$TMP/repo" config user.email t@t
    git -C "$TMP/repo" add -A; git -C "$TMP/repo" commit -qm init
    echo key > "$TMP/release-key"
}

run() {
    GPN_RELEASES_URL="http://127.0.0.1:$PORT/releases.json" \
    GPN_CANARY_URL="http://127.0.0.1:$PORT/canary.json" \
    GPN_RELEASE_KEY="$TMP/release-key" \
    GPN_RELEASE_LOCK="$TMP/release.lock" \
    bash "$TMP/repo/scripts/release-cut.sh" --verify-published "$@" 2>&1
}

echo "== a site ahead of its canary proof cannot be filed"
new_repo
set +e; out="$(run --version 0.3.84)"; rc=$?; set -e
[ "$rc" -eq 1 ] && ok "exit 1" || bad "expected exit 1, got $rc: $out"
echo "$out" | grep -q "the fleet is HELD" && ok "names the fleet-held condition" || bad "no HELD message: $out"
echo "$out" | grep -q "proves 0.3.83, not 0.3.84" && ok "quotes the versions it saw" || bad "versions not quoted: $out"
echo "$out" | grep -qi "manifest asset\|fetch_asset\|downloading" && bad "it started fetching assets despite the gate" \
    || ok "refused before any asset download"

echo "== a canary that matches the site passes the gate (and then goes on to §8)"
new_repo
echo "{\"site\":\"0.3.84\",\"canary\":\"0.3.84\"}" > "$TMP/state.json"
set +e; out="$(run --version 0.3.84)"; rc=$?; set -e
echo "$out" | grep -q "canary proof:.*proves 0.3.84" && ok "printed the canary proof's verdict" || bad "gate did not run: $out"
[ "$rc" -ne 0 ] && ok "later §8 failure still exits non-zero (stub serves no assets)" || bad "unexpected success"

echo "== an unreadable canary file is a refusal, not a pass"
new_repo
echo "{\"site\":\"0.3.84\",\"canary\":\"\"}" > "$TMP/state.json"
set +e; out="$(run --version 0.3.84)"; rc=$?; set -e
[ "$rc" -eq 1 ] && ok "exit 1" || bad "expected exit 1, got $rc: $out"
echo "$out" | grep -q "canary proof" && ok "says the proof could not be read" || bad "no explanation: $out"

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
