#!/usr/bin/env bash
# test-canary-auto.sh — row 290: publishing the canary proof with no human step.
#
# What must be true:
#   * nothing is published while the canary node still runs the OLD version, or
#     while the only Completed job is the one that already bridged the last
#     release (a stale proof must never bridge a new version);
#   * when the node runs the published version AND a NEW job is Completed, the
#     proof is written and published in one commit, naming that job;
#   * a version that is already bridged is a quiet no-op, not a second commit.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/../canary-auto.sh"
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  ok   — $1"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL — $1"; }

TMP="$(mktemp -d)"
STUB_PID=""
cleanup() { [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

cat > "$TMP/stub.py" <<'PY'
import http.server, json, os, sys

STATE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "state.json")

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_a): pass
    def _send(self, payload, code=200):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        self._send({"jsonrpc": "2.0", "id": 1, "result": json.load(open(STATE))["node"]})
    def do_GET(self):
        st = json.load(open(STATE))
        p = self.path.split("?")[0]
        if p == "/releases.json":
            self._send({"version": st["site"]})
        elif p == "/canary.json":
            self._send(st["canary"])
        elif p == "/providers":
            self._send({"providers": [{"address": st["provider"],
                                       "status_url": "http://127.0.0.1:8093",
                                       "channel_url": "http://127.0.0.1:8095/channel/job"}]})
        elif p == "/jobs":
            self._send({"from": 0, "jobs": st["jobs"]})
        else:
            self._send({"error": "not found"}, 404)

srv = http.server.HTTPServer(("127.0.0.1", 0), H)
with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "port"), "w") as f:
    f.write(str(srv.server_port))
srv.serve_forever()
PY

PROVIDER="5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj"

state() {  # state <site> <node> <canary-version> <canary-job> <jobs-json>
    python3 - "$TMP/state.json" "$PROVIDER" <<PY
import json,sys
json.dump({
  "site": "$1", "node": "$2", "provider": sys.argv[2],
  "canary": {"version": "$3", "job_id": int("$4"), "ok": True, "job_served": True},
  "jobs": json.loads('''$5'''),
}, open(sys.argv[1], "w"))
PY
}

python3 "$TMP/stub.py" & STUB_PID=$!
for _ in $(seq 1 50); do [ -f "$TMP/port" ] && break; sleep 0.1; done
PORT="$(cat "$TMP/port")"

new_repo() {
    rm -rf "$TMP/repo"; git init -q "$TMP/repo"
    git -C "$TMP/repo" config user.name t; git -C "$TMP/repo" config user.email t@t
    mkdir -p "$TMP/repo/website" "$TMP/repo/scripts"
    cp "$HERE/../canary-report.py" "$HERE/../canary-publish.sh" "$TMP/repo/scripts/"
    echo '{"version":"0.3.83","ok":true,"job_served":true,"job_id":6602}' > "$TMP/repo/website/canary.json"
    git -C "$TMP/repo" add -A; git -C "$TMP/repo" commit -qm init
}

run() {  # run <extra args...>
    bash "$SCRIPT" --root "$TMP/repo" \
        --site-url "http://127.0.0.1:$PORT/releases.json" \
        --canary-url "http://127.0.0.1:$PORT/canary.json" \
        --ap-url "http://127.0.0.1:$PORT" --rpc-port "$PORT" "$@" 2>&1
}

echo "== the node still runs the old version → nothing is published"
new_repo
state 0.3.84 0.3.83 0.3.83 6602 '[{"id":6610,"provider":"5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj","state":"Completed"}]'
set +e; out="$(run --no-push --poll 0)"; rc=$?; set -e
[ "$rc" -eq 1 ] && ok "exit 1 (waiting), not a failure" || bad "expected exit 1, got $rc: $out"
echo "$out" | grep -q "canary runs 0.3.83" && ok "says what the node runs vs what the site publishes" || bad "no version diff in: $out"
[ "$(git -C "$TMP/repo" rev-list --count HEAD)" -eq 1 ] && ok "no commit" || bad "a commit was made"

echo "== only the job that bridged the LAST release is Completed → refused"
new_repo
state 0.3.84 0.3.84 0.3.83 6602 '[{"id":6602,"provider":"5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj","state":"Completed"}]'
set +e; out="$(run --no-push --poll 0)"; rc=$?; set -e
[ "$rc" -eq 1 ] && ok "exit 1: a stale job cannot bridge a new version" || bad "expected exit 1, got $rc: $out"
echo "$out" | grep -q "after #6602" && ok "names the baseline the job must beat" || bad "baseline not named: $out"
[ "$(git -C "$TMP/repo" rev-list --count HEAD)" -eq 1 ] && ok "no commit" || bad "a commit was made"

echo "== node runs the published version and a NEW job is Completed → published"
new_repo
state 0.3.84 0.3.84 0.3.83 6602 '[{"id":6602,"provider":"5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj","state":"Completed"},{"id":6611,"provider":"5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj","state":"Completed"}]'
set +e; out="$(run --no-push --poll 0)"; rc=$?; set -e
[ "$rc" -eq 1 ] && ok "exit 1 only because the stub site cannot deploy (poll timeout, as designed)" || bad "expected exit 1 from the poll, got $rc: $out"
echo "$out" | grep -q "pushed, but" && ok "says it published and the live file has not flipped yet" || bad "no publish report: $out"
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if d.get("version")=="0.3.84" and d.get("job_id")==6611 else 1)' "$TMP/repo/website/canary.json" \
    && ok "report names 0.3.84 with the NEW job #6611" || bad "report wrong: $(cat "$TMP/repo/website/canary.json")"
[ "$(git -C "$TMP/repo" rev-list --count HEAD)" -eq 2 ] && ok "exactly one publish commit" || bad "unexpected commit count"
git -C "$TMP/repo" log -1 --format=%s | grep -q "canary: 0.3.84 proven" && ok "commit message names the version" || bad "commit message wrong"

echo "== already bridged → quiet no-op"
new_repo
state 0.3.84 0.3.84 0.3.84 6611 '[{"id":6611,"provider":"5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj","state":"Completed"}]'
echo '{"version":"0.3.84","ok":true,"job_served":true,"job_id":6611}' > "$TMP/repo/website/canary.json"
git -C "$TMP/repo" add -A; git -C "$TMP/repo" commit -qm "canary: 0.3.84"
set +e; out="$(run --no-push)"; rc=$?; set -e
[ "$rc" -eq 0 ] && ok "exit 0" || bad "expected exit 0, got $rc: $out"
echo "$out" | grep -q "already bridged" && ok "says it is already bridged" || bad "no idempotence message: $out"
[ "$(git -C "$TMP/repo" rev-list --count HEAD)" -eq 2 ] && ok "no second commit" || bad "commit count moved"

echo "== an asserted version that the site does not publish is refused"
set +e; out="$(run --version 9.9.9 --no-push)"; rc=$?; set -e
[ "$rc" -eq 2 ] && ok "exit 2 on a version mismatch" || bad "expected exit 2, got $rc: $out"

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
