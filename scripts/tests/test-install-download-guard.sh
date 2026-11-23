#!/usr/bin/env bash
# test-install-download-guard.sh -- row 247, the first half.
#
# "The one-command Windows install has no failure detection between download and
#  install, so a failed download silently re-installs the OLD build" is a claim
# about the three website snippets that ship that command. They are PowerShell
# inside HTML, so the shipped text is extracted from the page and executed with
# pwsh: the same lines a Windows user pastes, with curl and the manifest replaced
# by local stand-ins. Not a re-typed copy -- the page's own code block.
#
# Cases (each must fail the way the page says it fails, and the whole block must
# pass when the artifact is the one the manifest describes):
#   1. curl exits non-zero          -> "download failed (curl exit 7)"
#   2. truncated / 404-sized body   -> "download is 9 bytes, manifest says ..."
#   3. right size, wrong bytes      -> "sha256 mismatch -- refusing to extract"
#   4. correct artifact             -> no throw; guard block runs to the end
# Plus: all three surfaces (download.html, index.html quickstart, join.html) must
# carry the same guard, or a user who arrives through a join link is unprotected
# -- which is how the 0.3.64 install was reported in the first place.
#
# Run: bash scripts/tests/test-install-download-guard.sh
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
pwsh="${PWSH:-$(command -v pwsh || echo /tmp/pwsh/pwsh)}"
if [ ! -x "$pwsh" ]; then echo "FAIL  pwsh not found (set PWSH=)" >&2; exit 1; fi
if ! command -v python3 >/dev/null; then echo "FAIL  python3 not found" >&2; exit 1; fi

checks=0; failed=0
pass() { checks=$((checks+1)); echo "PASS  $1${2:+ -- $2}"; }
fail() { checks=$((checks+1)); failed=$((failed+1)); echo "FAIL  $1${2:+ -- $2}"; }
has() { # name, file, literal
  if grep -qF -- "$3" "$2"; then pass "$1"; else fail "$1" "missing in $(basename "$2"): $3"; fi
}

echo "== the guard is in every surface that ships the command =="
for page in download.html index.html join.html; do
  f="$repo/website/$page"
  has "$page: curl fails fast (-fL)"            "$f" 'curl.exe -fL'
  has "$page: aborts on curl exit code"         "$f" 'download failed (curl exit $LASTEXITCODE)'
  has "$page: aborts on short body"             "$f" 'manifest says $($asset.size)'
  has "$page: verifies sha256 before extract"   "$f" 'sha256 mismatch'
  has "$page: extracts to bin.pending"          "$f" 'C:\gpn\bin.pending'
done

echo
echo "== execute the shipped download.html block against local stand-ins =="
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
python3 - "$repo/website/download.html" "$tmp/block.ps1" <<'PY'
import html, re, sys
src, out = sys.argv[1], sys.argv[2]
text = open(src, encoding='utf-8').read()
m = re.search(r'<pre id="cmd-win-node"><code>(.*?)</code></pre>', text, re.S)
if not m:
    sys.exit("download.html: cmd-win-node block not found")
body = re.sub(r'<[^>]+>', '', m.group(1))
body = html.unescape(body)
lines = body.splitlines()
# Stop before Expand-Archive: extraction is Windows-only and is not the claim.
cut = next((i for i, l in enumerate(lines) if 'Expand-Archive' in l), len(lines))
keep = [l for l in lines[:cut] if l.strip()]
code = '\n'.join(keep)
# The two Windows-only dependencies become locals: the manifest read and curl.
code = re.sub(r'^\$m = Invoke-RestMethod\s+\S+\s*$',
              '$m = Get-Content -Raw $env:FAKE_MANIFEST | ConvertFrom-Json', code, flags=re.M)
code = code.replace(r'C:\gpn\gpn-windows.zip', '$env:FAKE_OUT')
code = code.replace(r'C:\gpn', '$env:FAKE_DIR')
code = code.replace('curl.exe', 'Invoke-FakeCurl')
prelude = '''function Invoke-FakeCurl {
  param([switch]$fL, [string]$o, [string]$url)
  $global:LASTEXITCODE = 0
  switch ($env:FAKE_CURL_MODE) {
    'fail' { $global:LASTEXITCODE = 7; return }
    default { Copy-Item -LiteralPath $env:FAKE_CURL_SRC -Destination $o -Force }
  }
}
'''
open(out, 'w', encoding='utf-8').write(prelude + code + '\n')
if 'Invoke-FakeCurl' not in code or '$env:FAKE_OUT' not in code:
    sys.exit("block rewrite produced unexpected text")
PY

if [ ! -s "$tmp/block.ps1" ]; then echo "FAIL  could not extract the block"; exit 1; fi

# A stand-in artifact: its real size and hash are what a correct manifest says.
printf 'this is a stand-in for gpn-windows.zip, not a real build\n' > "$tmp/real-artifact"
real_size=$(stat -c %s "$tmp/real-artifact")
real_sha=$(sha256sum "$tmp/real-artifact" | cut -d' ' -f1)
printf 'Not Found' > "$tmp/short-body"

manifest() { # size sha url
  printf '{"version":"0.3.70","assets":[{"os":"windows","url":"%s","size":%s,"sha256":"%s"}]}\n' "$3" "$1" "$2" > "$tmp/releases.json"
}

run_case() { # name expected-substring mode manifest-size manifest-sha body-to-write
  local name="$1" want="$2" mode="$3" size="$4" sha="$5" src="${6:-$tmp/real-artifact}"
  manifest "$size" "$sha" "file://$tmp/real-artifact"
  rm -f "$tmp/out.zip"
  FAKE_CURL_MODE="$mode" FAKE_CURL_SRC="$src" FAKE_MANIFEST="$tmp/releases.json" \
    FAKE_OUT="$tmp/out.zip" FAKE_DIR="$tmp/dir" \
    "$pwsh" -NoProfile -File "$tmp/block.ps1" >"$tmp/log" 2>&1
  local rc=$?
  if [ "$want" = "NONE" ]; then
    if [ $rc -eq 0 ] && ! grep -qi 'refusing\|download failed\|mismatch' "$tmp/log"; then
      pass "$name" "exit 0, no guard fired"
    else
      fail "$name" "expected clean run, exit $rc: $(tr '\n' ' ' < "$tmp/log" | cut -c1-200)"
    fi
  else
    if grep -qF -- "$want" "$tmp/log"; then
      pass "$name" "refused: $(grep -oF -- "$want" "$tmp/log" | head -1)"
    else
      fail "$name" "expected '$want', exit $rc: $(tr '\n' ' ' < "$tmp/log" | cut -c1-200)"
    fi
  fi
}

run_case "curl failure aborts instead of extracting"      "download failed (curl exit 7)"        fail "$real_size" "$real_sha"
run_case "9-byte body aborts before extract"              "download is 9 bytes, manifest says"   copy 999999      "$real_sha" "$tmp/short-body"
run_case "right size but wrong bytes aborts"              "sha256 mismatch"                      copy "$real_size" "0000000000000000000000000000000000000000000000000000000000000000"
run_case "the artifact the manifest describes is accepted" NONE                                  copy "$real_size" "$real_sha"

echo
echo "$checks checks, $failed failed"
[ "$failed" -eq 0 ]
