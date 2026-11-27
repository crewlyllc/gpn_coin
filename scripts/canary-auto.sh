#!/usr/bin/env bash
# canary-auto.sh — bridge a published release to the fleet with no human step (row 290).
#
# The gate (`update::canary_gate`) lets a non-canary node install a release only
# when `https://gpncoin.ai/canary.json` proves THAT version. Publishing the proof
# used to be a checklist line somebody had to remember: 0.3.82 and 0.3.83 both
# shipped without it and the whole fleet stayed on 0.3.81 for two releases.
#
# This script closes that loop. Run it on the canary host (this box) from a timer:
#
#   1. read the PUBLISHED version from releases.json (what the fleet must install)
#   2. ask the canary node, over RPC, which version it is RUNNING
#   3. if they differ → exit 1 "waiting" (the updater has not confirmed yet)
#   4. pick the newest job Completed for this host's provider after the last proof
#      (`canary-report.py --auto`), write the report, publish it
#   5. poll the live canary.json until it serves the version (bounded)
#
# Exit codes: 0 bridged (or already bridged) · 1 waiting for evidence (not an
# error; the next tick retries) · 2 refused / misconfigured (needs a human).
#
# Usage: scripts/canary-auto.sh [--version X.Y.Z] [--dry-run] [--no-push]
#                               [--root DIR] [--site-url URL] [--canary-url URL]
#                               [--ap-url URL] [--rpc-port N] [--poll SECS]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SITE_URL="https://gpncoin.ai/releases.json"
CANARY_URL="https://gpncoin.ai/canary.json"
AP_URL="http://127.0.0.1:8092"
RPC_PORT=9944
ASSERT=""
DRY=0
PUSH=1
POLL=240

while [ $# -gt 0 ]; do
    case "$1" in
        --version)   ASSERT="$2"; shift 2 ;;
        --root)      ROOT="$2"; shift 2 ;;
        --site-url)  SITE_URL="$2"; shift 2 ;;
        --canary-url) CANARY_URL="$2"; shift 2 ;;
        --ap-url)    AP_URL="$2"; shift 2 ;;
        --rpc-port)  RPC_PORT="$2"; shift 2 ;;
        --poll)      POLL="$2"; shift 2 ;;
        --dry-run)   DRY=1; shift ;;
        --no-push)   PUSH=0; shift ;;
        -h|--help)   sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

json_field() {  # json_field <url> <field>
    curl -s --max-time 10 "$1?cb=$(date +%s)" \
        | python3 -c 'import sys,json;print(json.load(sys.stdin).get(sys.argv[1],"") or "")' "$2" 2>/dev/null || true
}

version="$(json_field "$SITE_URL" version)"
[ -n "$version" ] || { echo "no published version at $SITE_URL — will retry next tick" >&2; exit 1; }
if [ -n "$ASSERT" ] && [ "$ASSERT" != "$version" ]; then
    echo "asserted $ASSERT but the site publishes $version" >&2
    exit 2
fi

live_canary="$(json_field "$CANARY_URL" version)"
repo_canary="$(python3 - "$ROOT/website/canary.json" <<'PY'
import json,sys
try: print(json.load(open(sys.argv[1])).get("version","") or "")
except Exception: print("")
PY
)"
if [ "$live_canary" = "$version" ] && [ "$repo_canary" = "$version" ]; then
    echo "already bridged: $CANARY_URL serves $version"
    exit 0
fi

node_ver="$(python3 - "$RPC_PORT" <<'PY'
import json,sys,urllib.request
port=sys.argv[1]
body=json.dumps({"id":1,"jsonrpc":"2.0","method":"system_version","params":[]}).encode()
try:
    req=urllib.request.Request(f"http://127.0.0.1:{port}",data=body,
                               headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req,timeout=10) as r:
        print(str(json.load(r).get("result","")).strip().strip('"'))
except Exception:
    print("")
PY
)"
if [ -z "$node_ver" ]; then
    echo "waiting: the canary node did not answer on :$RPC_PORT" >&2
    exit 1
fi
if [ "$node_ver" != "$version" ]; then
    echo "waiting: canary runs $node_ver, the site publishes $version — nothing to prove yet" >&2
    exit 1
fi

# The proof must come from a job served AFTER the last published one. Job ids are
# monotonic, so the previous proof's job_id is the floor: a stale row that already
# proved the OLD version can never bridge the new one.
baseline="$(python3 - "$ROOT/website/canary.json" "$CANARY_URL" <<'PY'
import json,sys,urllib.request
best=0
for p in sys.argv[1:]:
    try:
        if p.startswith("http"):
            with urllib.request.urlopen(p,timeout=10) as r: doc=json.load(r)
        else:
            doc=json.load(open(p))
        best=max(best,int(doc.get("job_id") or 0))
    except Exception:
        pass
print(best)
PY
)"

TMP_REPORT="$(mktemp "${TMPDIR:-/tmp}/canary-report.XXXXXX.json")"
trap 'rm -f "$TMP_REPORT"' EXIT

if ! python3 "$ROOT/scripts/canary-report.py" --auto --min-job-id "$baseline" \
        --rpc-port "$RPC_PORT" --ap-url "$AP_URL" --out "$TMP_REPORT"; then
    echo "waiting: no Completed job for this provider after #$baseline yet" >&2
    exit 1
fi

if [ "$DRY" = "1" ]; then
    echo "dry-run: report for $version at $TMP_REPORT"
    exit 0
fi

publish_args=(--root "$ROOT" --report "$TMP_REPORT")
[ "$PUSH" = "1" ] || publish_args+=(--no-push)
bash "$ROOT/scripts/canary-publish.sh" "${publish_args[@]}"

# The site deploys on push. Tick only on the LIVE file, never the repo copy.
deadline=$(( $(date +%s) + POLL ))
while :; do
    if [ "$(json_field "$CANARY_URL" version)" = "$version" ]; then
        echo "bridged: $CANARY_URL serves $version — the fleet may update"
        exit 0
    fi
    [ "$(date +%s)" -lt "$deadline" ] || break
    sleep 10
done
echo "pushed, but $CANARY_URL does not serve $version yet after ${POLL}s — will re-check next tick" >&2
exit 1
