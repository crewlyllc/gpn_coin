#!/usr/bin/env bash
# test-snapshot-weekly.sh — W170: the weekly snapshot job must refuse a STALE
# verifier *before* it exports anything or swaps the published asset.
#
# 2026-11-14 incident: the job exported, deleted the previous release asset and
# uploaded a new archive, and only then refused at `verify` because the installed
# gpn_snapshot predated the `checkpoints=` canonical-text change by 39 minutes.
# The mirror carried new bytes with an unverifiable digest for ~3 minutes.
#
# These tests drive the real script with stubbed collaborators (snapshot binary,
# RPC server, curl) so the *ordering* is observed, not asserted from the source:
#   A stale verifier        -> exit 5, and the snapshot binary is never invoked
#   a failing verify        -> exit 4, and curl is never invoked (nothing swapped)
#   the happy path          -> exit 0, archive + metadata published
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/../snapshot-weekly.sh"
[ -x "$SCRIPT" ] || { echo "FATAL: $SCRIPT not found" >&2; exit 2; }

TMP="$(mktemp -d)"
RPC_PID=""
cleanup() {
  [ -n "$RPC_PID" ] && kill "$RPC_PID" 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf '  ok   - %s\n' "$1"; }
no() { FAIL=$((FAIL + 1)); printf '  FAIL - %s\n' "$1"; }

# ── fake chain data + stubbed snapshot binary ───────────────────────────────
mkdir -p "$TMP/base/chains" "$TMP/src/bin" "$TMP/bin" "$TMP/out"
: > "$TMP/src/bin/gpn_snapshot.rs"
# Sources are old; the good stub is new. "Stale" is produced by moving the stub
# back in time, not by faking the comparison.
# (left at "now": the STALE case is produced by moving the BINARY back in time)

write_stub() { # $1 = path
  cat > "$1" <<'STUB'
#!/usr/bin/env bash
echo "call: $*" >> "$STUB_LOG"
cmd="$1"; shift
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    *) shift ;;
  esac
done
case "$cmd" in
  export) [ -n "$out" ] && printf 'stub-archive-bytes' > "$out"; exit 0 ;;
  verify) exit "${STUB_VERIFY_RC:-0}" ;;
esac
exit 0
STUB
  chmod +x "$1"
}

# ── a JSON-RPC node that answers the three calls the script makes ───────────
cat > "$TMP/rpc.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

BLOCK = 16

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):  # quiet
        pass
    def do_POST(self):
        n = int(self.headers.get('content-length', 0))
        req = json.loads(self.rfile.read(n) or b'{}')
        m, p = req.get('method'), req.get('params') or []
        if m == 'chain_getHeader':
            r = {'number': hex(BLOCK)}
        elif m == 'chain_getBlockHash':
            r = '0x' + ('%064x' % (int(p[0]) if p else 0))
        else:
            r = None
        body = json.dumps({'jsonrpc': '2.0', 'id': req.get('id'), 'result': r}).encode()
        self.send_response(200)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

srv = HTTPServer(('127.0.0.1', 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PY
/usr/bin/python3 "$TMP/rpc.py" > "$TMP/rpc.port" &
RPC_PID=$!
for _ in $(seq 1 100); do [ -s "$TMP/rpc.port" ] && break; sleep 0.05; done
RPC_PORT="$(head -1 "$TMP/rpc.port" 2>/dev/null || true)"
[ -n "$RPC_PORT" ] || { echo "FATAL: stub RPC did not start" >&2; exit 2; }

# ── curl stub: records every call, never touches the network ────────────────
cat > "$TMP/bin/curl" <<'STUB'
#!/usr/bin/env bash
echo "curl: $*" >> "$STUB_CURL_LOG"
for a in "$@"; do [ "$a" = "-w" ] && { printf '201'; exit 0; }; done
printf '{"id":1}'
exit 0
STUB
chmod +x "$TMP/bin/curl"

run() { # $1=stub path, $2=verify rc; sets RC, OUT_TXT
  STUB_LOG="$TMP/stub.log" STUB_CURL_LOG="$TMP/curl.log" STUB_VERIFY_RC="$2" \
  GPN_SNAPSHOT_BIN="$1" GPN_SNAPSHOT_SRC="$TMP/src" GPN_BASE_PATH="$TMP/base" \
  GPN_RPC="http://127.0.0.1:$RPC_PORT" \
  PATH="$TMP/bin:$PATH" \
  bash "$SCRIPT" --out "$TMP/out" > "$TMP/run.txt" 2>&1
  RC=$?
  OUT_TXT="$(cat "$TMP/run.txt")"
  STUB_CALLS=0; [ -f "$TMP/stub.log" ] && STUB_CALLS=$(wc -l < "$TMP/stub.log")
  CURL_CALLS=0; [ -f "$TMP/curl.log" ] && CURL_CALLS=$(wc -l < "$TMP/curl.log")
}

echo "snapshot-weekly preflight (W170)"

# ── 1: stale verifier refuses before doing anything ────────────────────────
write_stub "$TMP/bin/snap-stale"
touch -d '2 hours ago' "$TMP/bin/snap-stale"
rm -f "$TMP/stub.log" "$TMP/curl.log"
run "$TMP/bin/snap-stale" 0
[ "$RC" -eq 5 ] && ok "stale verifier exits 5 (got $RC)" || no "stale verifier should exit 5, got $RC"
grep -q 'STALE' <<<"$OUT_TXT" && ok "message says STALE" || no "message does not say STALE: $OUT_TXT"
grep -q 'gpn_snapshot.rs is newer' <<<"$OUT_TXT" && ok "names the newer source file" || no "does not name the newer source"
[ "$STUB_CALLS" -eq 0 ] && ok "the snapshot binary was never invoked (no export)" || no "export ran before the refusal ($STUB_CALLS calls)"
[ "$CURL_CALLS" -eq 0 ] && ok "no HTTP call at all" || no "curl ran before the refusal ($CURL_CALLS calls)"
[ ! -e "$TMP/out/snapshot.json" ] && ok "no metadata written" || no "metadata written despite refusal"

# ── 2: a failing verify refuses BEFORE the archive swap ────────────────────
write_stub "$TMP/bin/snap-good"
rm -f "$TMP/stub.log" "$TMP/curl.log"
run "$TMP/bin/snap-good" 1
[ "$RC" -eq 4 ] && ok "failing verify exits 4 (got $RC)" || no "failing verify should exit 4, got $RC"
grep -q 'nothing was published' <<<"$OUT_TXT" && ok "says nothing was published" || no "message does not say nothing was published"
[ "$CURL_CALLS" -eq 0 ] && ok "curl never called — no asset deleted, nothing uploaded ($CURL_CALLS)" || no "curl was called $CURL_CALLS time(s) before verify passed"
grep -q 'call: export' "$TMP/stub.log" && ok "export did run (the export stage is not what refused)" || no "export did not run"

# ── 3: happy path publishes archive then metadata ──────────────────────────
rm -f "$TMP/stub.log" "$TMP/curl.log"
run "$TMP/bin/snap-good" 0
[ "$RC" -eq 0 ] && ok "happy path exits 0 (got $RC)" || no "happy path exited $RC: $(tail -3 <<<"$OUT_TXT")"
grep -q 'signed metadata self-check OK' <<<"$OUT_TXT" && ok "self-check reported OK" || no "no self-check line"
grep -q 'call: verify' "$TMP/stub.log" && ok "the shipped verifier was used" || no "verifier never invoked"
UPLOAD_LINE=$(grep -n 'name=gpn-testnet-snapshot.tar.gz' "$TMP/curl.log" | head -1 | cut -d: -f1)
META_LINE=$(grep -n 'name=snapshot.json' "$TMP/curl.log" | head -1 | cut -d: -f1)
DELETE_LINE=$(grep -n 'DELETE' "$TMP/curl.log" | head -1 | cut -d: -f1)
[ -n "$UPLOAD_LINE" ] && ok "archive uploaded" || no "archive not uploaded"
[ -n "$META_LINE" ] && ok "metadata uploaded" || no "metadata not uploaded"
if [ -n "$UPLOAD_LINE" ] && [ -n "$META_LINE" ] && [ "$UPLOAD_LINE" -lt "$META_LINE" ]; then
  ok "archive goes up before the digest that describes it"
else
  no "metadata uploaded before the archive (race window inverted)"
fi

echo
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
