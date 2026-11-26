#!/usr/bin/env bash
# canary-publish.sh — publish the canary's proof so the rest of the fleet may
# update (row 275).
#
# The gate lives in the node (`update::canary_gate`): every node that is not the
# canary refuses to install a new release until it can read a proof that is
#   * about THAT version,
#   * `ok: true`   (the canary is running it), and
#   * `job_served: true` (the canary actually did work on it).
#
# The canary node writes that proof to `<state-dir>/update/canary.json` itself
# (gpn_node, canary mode). This script is the only thing that publishes it, and
# it refuses anything that is not green — a half-proven release must not unblock
# the fleet.
#
# Usage:
#   scripts/canary-publish.sh                     # publish the local report
#   scripts/canary-publish.sh --report PATH       # publish a copied report
#   scripts/canary-publish.sh --dry-run           # check only, write nothing
#   scripts/canary-publish.sh --root DIR          # repo root (tests)
#   scripts/canary-publish.sh --no-push           # commit locally, do not push
#
# Exit: 0 published (or dry-run OK), 2 not green / unreadable, 3 commit/push failed.
set -euo pipefail

REPORT="${HOME}/.gpn/update/canary.json"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY=0
PUSH=1

while [ $# -gt 0 ]; do
    case "$1" in
        --report) REPORT="$2"; shift 2 ;;
        --root)   ROOT="$2"; shift 2 ;;
        --dry-run) DRY=1; shift ;;
        --no-push) PUSH=0; shift ;;
        -h|--help) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [ ! -f "$REPORT" ]; then
    echo "no canary report at $REPORT — the canary has not confirmed anything yet" >&2
    exit 2
fi

verdict="$(python3 - "$REPORT" <<'PY'
import json, sys
p = sys.argv[1]
try:
    st = json.load(open(p))
except Exception as e:
    print("BAD unreadable report: %s" % e); raise SystemExit(0)
ver = str(st.get("version") or "").strip()
if not ver:
    print("BAD report has no version"); raise SystemExit(0)
if not st.get("ok"):
    why = str(st.get("reason") or "").strip() or "no reason given"
    print("BAD the canary did not confirm %s (%s)" % (ver, why)); raise SystemExit(0)
if not st.get("job_served"):
    print("BAD the canary is running %s but has served no job on it yet" % ver); raise SystemExit(0)
print("OK %s" % ver)
PY
)"

kind="${verdict%% *}"
value="${verdict#* }"
if [ "$kind" != "OK" ]; then
    echo "refusing to publish: $value" >&2
    exit 2
fi
VERSION="$value"

DEST="$ROOT/website/canary.json"
echo "canary proof for $VERSION is green"

if [ "$DRY" = "1" ]; then
    echo "dry-run: would write $DEST and push"
    exit 0
fi

mkdir -p "$(dirname "$DEST")"
TMP="$DEST.tmp-$$"
cp "$REPORT" "$TMP"
mv "$TMP" "$DEST"
echo "wrote $DEST"

cd "$ROOT"
if [ -z "$(git status --porcelain -- website/canary.json)" ]; then
    echo "canary.json already published for $VERSION — nothing to commit"
    exit 0
fi
git add website/canary.json
git -c user.name="gpn-canary" -c user.email="canary@gpncoin.ai" \
    commit -q -m "canary: $VERSION proven on a node (running + a job served)" \
    -- website/canary.json
SHA="$(git rev-parse --short HEAD)"
echo "committed $SHA"

if [ "$PUSH" = "0" ]; then
    echo "not pushing (--no-push)"
    exit 0
fi
# The self-hosted runner's checkout carries a stale GitHub App extraheader that
# overrides ~/.git-credentials (403 "Write access not granted"); blank it here so
# the push authenticates as the repo owner.
PAT_FILE="${HOME}/.openclaw/credentials/github-gpn-pat.txt"
if [ -f "$PAT_FILE" ]; then
    URL="https://jeremyzguo:$(cat "$PAT_FILE")@github.com/jeremyzguo/gpn_coin.git"
    git -c "http.https://github.com/.extraheader=" push "$URL" HEAD:main
else
    git push origin HEAD:main
fi
echo "published canary proof for $VERSION ($SHA) — the fleet may now update"
