#!/usr/bin/env bash
# Verify the keyless cosign signature published next to gpn-windows.zip.
#
# Why this exists: for the user-owned PRIVATE repo jeremyzguo/gpn_coin GitHub does not
# persist native attestations ("Feature not available for user-owned private
# repositories"), so the release-windows.yml gate signs the zip keyless (Fulcio/Rekor)
# and publishes gpn-windows.zip.bundle beside it. The required out-of-band check is
# therefore `cosign verify-blob --bundle`, not `gh attestation verify`.
#
# Usage: scripts/verify-windows-bundle.sh [tag]      (default tag: windows-latest)
# Requires: cosign on PATH, curl, python3, a GitHub token in $GITHUB_TOKEN or
#           ~/.openclaw/credentials/github-gpn-pat.txt (private repo asset download).
set -euo pipefail

TAG="${1:-windows-latest}"
REPO="jeremyzguo/gpn_coin"
WORK="${TMPDIR:-/tmp}/gpn-verify-$TAG"
IDENTITY='^https://github\.com/jeremyzguo/gpn_coin/\.github/workflows/release-windows\.yml@.*$'
ISSUER='https://token.actions.githubusercontent.com'

TOKEN="${GITHUB_TOKEN:-$(cat "$HOME/.openclaw/credentials/github-gpn-pat.txt" 2>/dev/null || true)}"
[ -n "$TOKEN" ] || { echo "no GitHub token found" >&2; exit 2; }
command -v cosign >/dev/null || { echo "cosign not on PATH" >&2; exit 2; }

mkdir -p "$WORK"
cd "$WORK"

api() { curl -sS -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github+json" "$@"; }

echo "== release $REPO $TAG"
api "https://api.github.com/repos/$REPO/releases/tags/$TAG" |
  python3 -c '
import json,sys
d=json.load(sys.stdin)
for a in d["assets"]:
    print(a["name"], a["size"], a.get("digest") or a.get("updated_at"))
' | tee assets.txt

pick() {  # $1 = asset name -> asset id, or empty if absent
  api "https://api.github.com/repos/$REPO/releases/tags/$TAG" |
    python3 -c 'import json,sys;d=json.load(sys.stdin);ids=[a["id"] for a in d["assets"] if a["name"]==sys.argv[1]];print(ids[0] if ids else "")' "$1"
}
ZIP_ID=$(pick gpn-windows.zip)
BUN_ID=$(pick gpn-windows.zip.bundle)
[ -n "$ZIP_ID" ] || { echo "NO ZIP: $TAG has no gpn-windows.zip asset" >&2; exit 3; }
[ -n "$BUN_ID" ] || { echo "NO BUNDLE: $TAG has no gpn-windows.zip.bundle - unattested, release does not proceed" >&2; exit 3; }

for pair in "$ZIP_ID:gpn-windows.zip" "$BUN_ID:gpn-windows.zip.bundle"; do
  id="${pair%%:*}"; name="${pair#*:}"
  curl -sSL -H "Authorization: Bearer $TOKEN" -H "Accept: application/octet-stream" \
    -o "$name" "https://api.github.com/repos/$REPO/releases/assets/$id"
  echo "downloaded $name $(stat -c%s "$name") bytes"
done

echo "== sha256 of the published zip"
sha256sum gpn-windows.zip

echo "== cosign verify-blob (keyless, workflow identity pinned)"
cosign verify-blob \
  --bundle gpn-windows.zip.bundle \
  --certificate-identity-regexp "$IDENTITY" \
  --certificate-oidc-issuer "$ISSUER" \
  gpn-windows.zip

echo "VERIFIED: $REPO $TAG gpn-windows.zip"
