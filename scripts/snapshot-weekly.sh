#!/usr/bin/env bash
# snapshot-weekly.sh — export a chain snapshot and publish it to the PUBLIC mirror
# (crewlyllc/gpn_coin, release tag `snapshot-latest`) so a fresh node can bootstrap
# in minutes instead of syncing ~200k blocks.
#
# Usage: scripts/snapshot-weekly.sh [--no-upload] [--out DIR]
# Exit codes: 0 ok · 2 usage · 3 export failed · 4 upload failed
set -euo pipefail

REPO="crewlyllc/gpn_coin"
TAG="snapshot-latest"
ASSET="gpn-testnet-snapshot.tar.gz"
BASE_PATH="${GPN_BASE_PATH:-$HOME/.gpn/node}"
OUT_DIR="${TMPDIR:-/tmp}"
UPLOAD=1

while [ $# -gt 0 ]; do
  case "$1" in
    --no-upload) UPLOAD=0; shift ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    *) echo "usage: $0 [--no-upload] [--out DIR]" >&2; exit 2 ;;
  esac
done

TOKEN_FILE="$HOME/.openclaw/credentials/github-crewlyllc-pat.txt"
SNAP_BIN="$HOME/.local/bin/gpn_snapshot"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="$OUT_DIR/gpn-testnet-snapshot-$STAMP.tar.gz"
META="$OUT.meta.json"

[ -x "$SNAP_BIN" ] || { echo "FATAL: $SNAP_BIN not found" >&2; exit 3; }
[ -d "$BASE_PATH/chains" ] || { echo "FATAL: no chain data under $BASE_PATH" >&2; exit 3; }

echo "[snapshot] exporting from $BASE_PATH (freeze: mining pauses for consistency)"
mkdir -p "$OUT_DIR"
if ! "$SNAP_BIN" export --base-path "$BASE_PATH" --out "$OUT" --freeze; then
  echo "FATAL: export failed" >&2; exit 3
fi

SIZE=$(stat -c %s "$OUT")
SHA=$(sha256sum "$OUT" | cut -d' ' -f1)
BLOCK=$(python3 -c 'import json,sys,urllib.request
url=sys.argv[1]
r=urllib.request.Request(url,data=json.dumps({"id":1,"jsonrpc":"2.0","method":"chain_getHeader","params":[]}).encode(),headers={"content-type":"application/json"})
print(int(json.load(urllib.request.urlopen(r,timeout=5))["result"]["number"],16))' "${GPN_RPC:-http://127.0.0.1:9944}" 2>/dev/null || echo "?")
echo "[snapshot] built $OUT"
echo "[snapshot] block=$BLOCK size=$SIZE sha256=$SHA"

if [ "$UPLOAD" -eq 0 ]; then echo "[snapshot] --no-upload: done"; exit 0; fi
[ -r "$TOKEN_FILE" ] || { echo "FATAL: no mirror PAT at $TOKEN_FILE" >&2; exit 4; }
TOKEN="$(tr -d '\n' < "$TOKEN_FILE")"
API="https://api.github.com/repos/$REPO"

# ensure the release exists (rolling tag)
REL_ID=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API/releases/tags/$TAG" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])' 2>/dev/null || true)
if [ -z "${REL_ID:-}" ]; then
  echo "[snapshot] creating release $TAG"
  REL_ID=$(curl -sf -X POST -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github+json" \
    "$API/releases" -d "{\"tag_name\":\"$TAG\",\"name\":\"Chain snapshot (rolling)\",\"body\":\"Automated weekly chain snapshot. Import with: gpn_snapshot import --snapshot <file> --base-path <dir> --pruning archive\"}" \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')
fi
[ -n "${REL_ID:-}" ] || { echo "FATAL: cannot resolve release" >&2; exit 4; }

# drop any previous asset of the same name (GitHub keeps them otherwise)
OLD=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API/releases/$REL_ID/assets?per_page=100" \
  | python3 -c "import sys,json;print(next((a['id'] for a in json.load(sys.stdin) if a['name']=='$ASSET'),''))" 2>/dev/null || true)
if [ -n "${OLD:-}" ]; then
  echo "[snapshot] deleting previous asset id=$OLD"
  curl -sf -X DELETE -H "Authorization: Bearer $TOKEN" "$API/releases/assets/$OLD" >/dev/null || true
fi

echo "[snapshot] uploading $ASSET ($((SIZE/1024/1024)) MiB) — this can take several minutes"
CODE=$(curl -s -o /tmp/snap-upload.out -w '%{http_code}' --max-time 3600 -X POST \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/gzip" \
  --data-binary "@$OUT" \
  "https://uploads.github.com/repos/$REPO/releases/$REL_ID/assets?name=$ASSET")
if [ "$CODE" != "201" ] && [ "$CODE" != "200" ]; then
  echo "FATAL: upload failed HTTP $CODE: $(head -c 300 /tmp/snap-upload.out)" >&2
  exit 4
fi

DL="https://github.com/$REPO/releases/download/$TAG/$ASSET"

# ── Signed metadata (the ONLY trust anchor an installer may use) ─────────────
# An installer must not scrape this release body: text on a page is not signed.
# Publish a small ed25519-signed `snapshot.json` as an asset next to the archive,
# over the same URL the installer will download — that is what
# `gpn_snapshot verify --meta snapshot.json` checks before importing anything.
META_OUT="$OUT_DIR/snapshot.json"
if ! python3 "$(dirname "$0")/snapshot-metadata.py" sign \
      --url "$DL" --sha256 "$SHA" --size "$SIZE" --block "$BLOCK" \
      --built "$STAMP UTC" --out "$META_OUT"; then
  echo "FATAL: could not sign snapshot.json — refusing to publish an unverifiable snapshot" >&2
  exit 4
fi

# Cross-check with the verifier a node actually ships (catches signer/verifier drift).
VERIFY_BIN="${GPN_SNAPSHOT_BIN:-$HOME/.local/bin/gpn_snapshot}"
if [ -x "$VERIFY_BIN" ]; then
  if "$VERIFY_BIN" verify --meta "$META_OUT" --snapshot "$OUT" > /tmp/snap-verify.out 2>&1; then
    echo "[snapshot] signed metadata self-check OK"
  else
    echo "FATAL: gpn_snapshot verify rejected the signed metadata:" >&2
    cat /tmp/snap-verify.out >&2
    exit 4
  fi
else
  echo "[snapshot] WARN: $VERIFY_BIN not found — publishing WITHOUT the local verify self-check" >&2
fi

# Upload the metadata first, then (re)point the release at it: an installer that
# races the weekly job must never see new bytes with the previous digest.
OLD_META=$(curl -sf -H "Authorization: Bearer $TOKEN" "$API/releases/$REL_ID/assets?per_page=100" \
  | python3 -c "import sys,json;print(next((a['id'] for a in json.load(sys.stdin) if a['name']=='snapshot.json'),''))" 2>/dev/null || true)
if [ -n "${OLD_META:-}" ]; then
  curl -sf -X DELETE -H "Authorization: Bearer $TOKEN" "$API/releases/assets/$OLD_META" >/dev/null || true
fi
CODE=$(curl -s -o /tmp/snap-meta-upload.out -w '%{http_code}' --max-time 120 -X POST \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  --data-binary "@$META_OUT" \
  "https://uploads.github.com/repos/$REPO/releases/$REL_ID/assets?name=snapshot.json")
if [ "$CODE" != "201" ] && [ "$CODE" != "200" ]; then
  echo "FATAL: snapshot.json upload failed HTTP $CODE: $(head -c 300 /tmp/snap-meta-upload.out)" >&2
  exit 4
fi
rm -f "$META_OUT"

# The release body stays for humans; it is NOT a trust anchor (see above).
curl -sf -X PATCH -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github+json" \
  "$API/releases/$REL_ID" \
  -d "{\"body\":\"Automated weekly chain snapshot.\\n\\nblock: $BLOCK\\nsize: $SIZE\\nsha256: $SHA\\nbuilt: $STAMP UTC\\n\\nSigned metadata (what installers verify): https://github.com/$REPO/releases/download/$TAG/snapshot.json\\n\\nImport manually:\\n\`\`\`\\ngpn_snapshot import --snapshot $ASSET --base-path <node dir> --pruning archive\\n\`\`\`\"}" >/dev/null || true

echo "[snapshot] published $DL"
echo "[snapshot] published https://github.com/$REPO/releases/download/$TAG/snapshot.json"
echo "[snapshot] block=$BLOCK size=$SIZE sha256=$SHA"
rm -f "$OUT" "$META"
