#!/usr/bin/env bash
# release-cut.sh — cut a signed GPN release from an already-built pair of assets.
#
# The build workflows (.github/workflows/release-{unix,windows}.yml) upload
# `gpn-linux.tar.gz` (tag `linux-latest`) and `gpn-windows.zip` (tag
# `windows-latest`) to the build repo. Everything after that used to be ~30 min
# of hand work — which is why 0.3.3 sat built-but-unsigned for days and no node
# auto-updated. This script does it in one idempotent command:
#
#   1. DETECT   compare the built assets' digests/sizes (GitHub API) with the
#               published website/releases.json; "already released X" and exit
#               0 when they match (so a cron every 30 min is a quiet no-op).
#   2. DOWNLOAD both assets (resumable), verify sha256 against the API digest.
#   3. VERSION  read from the `gpn-build-v1 version=…` stamp INSIDE the bundles;
#               --version is an assertion, never a silent guess.
#   4. MIRROR   replace both assets on the public repo (crewlyllc/gpn_coin),
#               rename-then-upload then verify the mirror's digest.
#   5. SIGN     gen-release-manifest.py with the local ed25519 key; self-verify.
#   6. PUBLISH  commit only website/releases.json, push, poll the live URL.
#   7. VERIFY   anonymous download of each mirrored asset, sha256 vs manifest.
#
# WHY SIGNING IS LOCAL: the private key never leaves this box. Putting it in
# GitHub Actions would mean one leaked repo secret (or a compromised action)
# can forge a release that every auto-updating node installs. The build repo
# being writable by CI is fine — an attacker there can only produce an unsigned
# asset, which this script refuses to sign unless the version stamp says so.
# See docs/updates.md -> "Key custody".
#
# Usage:
#   scripts/release-cut.sh [--dry-run] [--version X.Y.Z] [--skip-mirror]
#                          [--min-version X.Y.Z] [--timeout SECS] [--yes]
#
#   --dry-run       do everything except mirror-upload/commit/push (signs into a
#                   temp dir and prints the manifest it *would* publish)
#   --version       assert the version the bundles must contain
#   --skip-mirror   assets are already in the public repo (sync workflow fixed)
#   --min-version   override the cutoff (default: keep the published one)
#   --timeout       seconds to wait for https://gpncoin.ai/releases.json (300)
#   --yes           non-interactive (default; kept for symmetry)
#
# Env (tests / unusual deployments):
#   GPN_GH_API           https://api.github.com
#   GPN_GH_UPLOAD        https://uploads.github.com
#   GPN_SOURCE_REPO      jeremyzguo/gpn_coin      (where CI puts the assets)
#   GPN_MIRROR_REPO      crewlyllc/gpn_coin       (what nodes download)
#   GPN_RELEASES_URL     https://gpncoin.ai/releases.json
#   GPN_RELEASE_DOWNLOAD_BASE  https://github.com/<mirror>/releases/download
#   GPN_RELEASE_KEY      ~/.openclaw/credentials/gpn-release-key.txt
#   GPN_VERIFY_KEY       (optional) expected pubkey hex for the self-check
#   GPN_POLL_INTERVAL    seconds between publish polls (10)
#   GPN_WORK_DIR         where partial downloads are kept for resuming
#                        (~/.cache/gpn-release-cut; ~550 MB when populated)
#
# Exit: 0 = published or nothing to do; 2 = refused (precondition); 1 = failed.
set -euo pipefail
set -o errtrace           # so the ERR trap fires inside functions too
shopt -s inherit_errexit 2>/dev/null || true

REPO="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$REPO" ] || { echo "REFUSED: not inside a git working tree." >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$SCRIPT_DIR/gen-release-manifest.py"
COMMIT="$SCRIPT_DIR/commit-paths.sh"
PY="${PYTHON:-/usr/bin/python3}"
[ -x "$PY" ] || PY="$(command -v python3)"
# Never litter the repo with __pycache__ from importing the manifest tool.
export PYTHONDONTWRITEBYTECODE=1

MANIFEST_REL="website/releases.json"
MANIFEST="$REPO/$MANIFEST_REL"

GPN_GH_API="${GPN_GH_API:-https://api.github.com}"
GPN_GH_UPLOAD="${GPN_GH_UPLOAD:-https://uploads.github.com}"
SRC_REPO="${GPN_SOURCE_REPO:-jeremyzguo/gpn_coin}"
MIRROR_REPO="${GPN_MIRROR_REPO:-crewlyllc/gpn_coin}"
RELEASES_URL="${GPN_RELEASES_URL:-https://gpncoin.ai/releases.json}"
DL_BASE="${GPN_RELEASE_DOWNLOAD_BASE:-https://github.com/$MIRROR_REPO/releases/download}"
KEY="${GPN_RELEASE_KEY:-$HOME/.openclaw/credentials/gpn-release-key.txt}"
SRC_PAT_FILE="${GPN_GPN_PAT_FILE:-$HOME/.openclaw/credentials/github-gpn-pat.txt}"
DST_PAT_FILE="${GPN_CREWLYLLC_PAT_FILE:-$HOME/.openclaw/credentials/github-crewlyllc-pat.txt}"
POLL_INTERVAL="${GPN_POLL_INTERVAL:-10}"

DRY=0
SKIP_MIRROR=0
DECLARED_VERSION=""
MIN_VERSION=""
MIN_VERSION_SET=0
TIMEOUT=300

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)      DRY=1; shift ;;
    --skip-mirror)  SKIP_MIRROR=1; shift ;;
    --version)      DECLARED_VERSION="${2:?--version needs X.Y.Z}"; shift 2 ;;
    --min-version)  MIN_VERSION="${2:?--min-version needs X.Y.Z}"; MIN_VERSION_SET=1; shift 2 ;;
    --timeout)      TIMEOUT="${2:?--timeout needs seconds}"; shift 2 ;;
    --yes|-y)       shift ;;
    -h|--help)      sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "REFUSED: unknown option '$1'" >&2; exit 2 ;;
  esac
done

case "$DECLARED_VERSION" in
  "") ;;
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "REFUSED: --version must look like X.Y.Z (got '$DECLARED_VERSION')" >&2; exit 2 ;;
esac

# ─────────────────────────────────────────────────────────────── helpers ──
STEP="startup"
TMP=""
CFG_SRC=""
CFG_DST=""

scrub() { # never let a credential reach a log, even in an error path
  local s
  s="$(cat)"
  for f in "$CFG_SRC" "$CFG_DST"; do
    [ -n "$f" ] && [ -f "$f" ] || continue
    s="$(printf '%s\n' "$s" | sed -e 's/gh[pousr]_[A-Za-z0-9]\{20,\}/***TOKEN***/g')"
  done
  printf '%s\n' "$s"
}

cleanup() {
  local rc=$?
  [ -n "$TMP" ] && rm -rf "$TMP"
  [ -n "$CFG_SRC" ] && rm -f "$CFG_SRC"
  [ -n "$CFG_DST" ] && rm -f "$CFG_DST"
  return $rc
}
trap cleanup EXIT
on_err() {
  local rc=$?
  echo "release-cut: FAILED while ${STEP} (exit $rc)" >&2
  exit $rc
}
trap on_err ERR

step() { STEP="$1"; printf '\n== %s\n' "$1"; }
say()  { printf '   %s\n' "$*"; }

make_curl_cfg() { # pat_file out
  local pat
  [ -f "$1" ] || { echo "REFUSED: no GitHub token at $1" >&2; exit 2; }
  pat="$(tr -d '\r\n' < "$1")"
  [ -n "$pat" ] || { echo "REFUSED: empty token at $1" >&2; exit 2; }
  ( umask 077; printf 'header = "Authorization: Bearer %s"\n' "$pat" > "$2" )
  chmod 600 "$2"
}

api() { # cfg method path [extra curl args...]
  local cfg=$1 method=$2 path=$3; shift 3
  curl -sS --fail-with-body -X "$method" -K "$cfg" \
    -H 'Accept: application/vnd.github+json' \
    -H 'User-Agent: gpn-release-cut/1' \
    "$@" "$GPN_GH_API$path" 2>&1 | scrub
}

asset_meta() { # cfg repo tag name -> "sha256 size id"
  local cfg=$1 repo=$2 tag=$3 name=$4
  api "$cfg" GET "/repos/$repo/releases/tags/$tag" \
    | "$PY" -c '
import json, sys
name = sys.argv[1]
d = json.load(sys.stdin)
if d.get("message"):
    sys.exit("GitHub API: " + d["message"])
for a in d.get("assets", []):
    if a.get("name") == name:
        dg = a.get("digest") or ""
        if not dg.startswith("sha256:"):
            sys.exit("%s: GitHub reports no sha256 digest; cannot verify bytes" % name)
        print(dg[7:].lower(), a["size"], a["id"])
        break
else:
    sys.exit("%s: not an asset of release %s" % (name, d.get("tag_name", "?")))
' "$name"
}

release_id() { # cfg repo tag
  api "$1" GET "/repos/$2/releases/tags/$3" \
    | "$PY" -c 'import json,sys; d=json.load(sys.stdin); print(d["id"])'
}

release_assets() { # cfg repo tag -> "name id digest"
  api "$1" GET "/repos/$2/releases/tags/$3" \
    | "$PY" -c '
import json, sys
d = json.load(sys.stdin)
for a in d.get("assets", []):
    print(a["name"], a["id"], (a.get("digest") or "-"))
'
}

sha_of() { "$PY" -c 'import hashlib,sys
h=hashlib.sha256()
with open(sys.argv[1],"rb") as f:
    for b in iter(lambda: f.read(1<<20), b""):
        h.update(b)
print(h.hexdigest())' "$1"; }

size_of() { stat -c %s -- "$1"; }

bundle_stamp() { # path -> version ("" when unstamped)
  "$PY" - "$GEN" "$1" <<'PY'
import importlib.util, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("genrel", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
print(m.read_bundle_stamp(Path(sys.argv[2])).get("version", ""))
PY
}

bundle_commit() { # path -> the commit the bundle was built from ("" when unstamped)
  "$PY" - "$GEN" "$1" <<'PY'
import importlib.util, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("genrel", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
st = m.read_bundle_stamp(Path(sys.argv[2]))
print(st.get("sha", "") or st.get("commit", ""))
PY
}

# Refuse to SIGN an artifact whose commit never passed CI.
#
# The check lives here, not in the build workflow: on a single runner the
# workflow waited for CI while CI sat queued behind that same workflow -- a
# deadlock that stalled 0.3.14 for 40 minutes (2026-11-03). Building is cheap;
# PUBLISHING is irreversible, so the gate belongs at signing time.
require_green_ci() { # sha (short or full)
  local sha="$1"
  if [ -z "$sha" ]; then
    say "no commit sha in the bundle stamp -- CI cannot be verified"
    return 0
  fi
  if [ "$sha" = "unknown" ]; then
    echo "ABORT: the bundle stamp says sha=unknown -- refusing to sign an" >&2
    echo "       unverified build (rebuild from a git checkout)." >&2
    exit 1
  fi
  case "$sha" in
    *[!0-9a-f]*) echo "ABORT: bundle stamp carries '$sha', which is not a hex commit" >&2
                 echo "       sha -- refusing to sign an unverified build." >&2
                 exit 1 ;;
  esac
  local tok="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
  local tokfile="${GPN_GH_TOKEN_FILE:-$HOME/.openclaw/credentials/github-gpn-pat.txt}"
  if [ -z "$tok" ] && [ -f "$tokfile" ]; then tok="$(tr -d '\n' < "$tokfile")"; fi
  if [ -z "$tok" ]; then
    echo "ABORT: no GitHub token available to verify CI for $sha" >&2
    exit 1
  fi
  # `build.rs` stamps the SHORT sha (`gpn-build-v1 … sha=325b0f7`), but the
  # workflow-runs API's `head_sha` filter only matches a FULL 40-char sha —
  # asking it with the short form returns zero runs, so every real bundle used
  # to look like "no CI run" and release-cut could never sign (2026-11-03).
  # Resolve first; a sha that cannot be resolved is a source problem, and an
  # unverifiable build is still refused below.
  local full="$sha"
  if [ "${#sha}" -lt 40 ]; then
    full="$(curl -sS -H "Authorization: Bearer $tok" -H 'Accept: application/vnd.github+json' \
        "$GPN_GH_API/repos/$SRC_REPO/commits/$sha" | "$PY" -c '
import json,sys
try:
    d = json.load(sys.stdin)
except Exception:
    d = {}
print(d.get("sha") or "")')" || full=""
    case "$full" in
      "") echo "ABORT: could not resolve commit ${sha} to a full sha, so its CI" >&2
          echo "       cannot be verified -- refusing to sign." >&2
          exit 1 ;;
      *[!0-9a-f]*) echo "ABORT: commit ${sha} resolved to '$full', which is not a" >&2
                   echo "       sha -- refusing to sign." >&2
                   exit 1 ;;
    esac
    say "commit ${sha} resolves to ${full}"
  fi
  local verdict
  verdict="$(curl -sS -H "Authorization: Bearer $tok" -H 'Accept: application/vnd.github+json' \
      "$GPN_GH_API/repos/$SRC_REPO/actions/runs?head_sha=$full&per_page=100" | "$PY" -c '
import json,sys
d=json.load(sys.stdin)
ci=[r for r in d.get("workflow_runs",[]) if r.get("name")=="CI"]
if not ci:
    print("missing"); raise SystemExit
ci.sort(key=lambda r: r["created_at"])
print(ci[-1]["status"] + ":" + str(ci[-1].get("conclusion")))')"
  case "$verdict" in
    completed:success) say "CI green for ${sha:0:7}" ;;
    missing)
      echo "ABORT: no CI run for commit ${sha:0:7} (${full:0:12}) -- refusing to" >&2
      echo "       sign an unverified build" >&2
      exit 1 ;;
    *)
      echo "ABORT: CI for ${sha:0:7} is '$verdict' -- refusing to sign an unverified build" >&2
      exit 1 ;;
  esac
}

published_version() {
  [ -f "$MANIFEST" ] || { echo ""; return 0; }
  "$PY" -c 'import json,sys; print(json.load(open(sys.argv[1])).get("version",""))' "$MANIFEST"
}

published_min_version() {
  [ -f "$MANIFEST" ] || { echo ""; return 0; }
  "$PY" -c 'import json,sys; print(json.load(open(sys.argv[1])).get("min_version",""))' "$MANIFEST"
}

# 0 = the published manifest already describes exactly these two assets
published_matches() { # sha_linux size_linux sha_win size_win
  [ -f "$MANIFEST" ] || return 3
  "$PY" - "$MANIFEST" "$@" <<'PY'
import json, sys
man = json.load(open(sys.argv[1]))
want = {("linux", sys.argv[2].lower(), int(sys.argv[3])),
        ("windows", sys.argv[4].lower(), int(sys.argv[5]))}
got = {(a.get("os"), (a.get("sha256") or "").lower(), int(a.get("size", 0)))
       for a in man.get("assets", [])}
sys.exit(0 if got == want else 1)
PY
}

# ────────────────────────────────────────────────────────── preconditions ──
step "0/6 preconditions"
[ -f "$GEN" ] || { echo "REFUSED: $GEN missing" >&2; exit 2; }
[ -f "$COMMIT" ] || { echo "REFUSED: $COMMIT missing" >&2; exit 2; }
[ -f "$KEY" ] || { echo "REFUSED: no release key at $KEY (see scripts/gpn-release-key.sh)" >&2; exit 2; }
# Someone else's edit to the manifest must not be signed or committed by us.
if [ -n "$(git -C "$REPO" status --porcelain -- "$MANIFEST_REL")" ]; then
  echo "REFUSED: $MANIFEST_REL is dirty in the working tree." >&2
  echo "  That is someone else's edit. Commit or restore it first:" >&2
  echo "    git -C $REPO checkout -- $MANIFEST_REL   # or commit it yourself" >&2
  exit 2
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/gpn-release-cut.XXXXXX")"
# A stable work dir so an interrupted run resumes instead of restarting a
# 300 MB download. The sidecar pins which build the partial file belongs to.
WORK="${GPN_WORK_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/gpn-release-cut}"
mkdir -p "$WORK"
CFG_SRC="$TMP/curl-src.cfg"
CFG_DST="$TMP/curl-dst.cfg"
make_curl_cfg "$SRC_PAT_FILE" "$CFG_SRC"
if [ "$DRY" = 0 ] && [ "$SKIP_MIRROR" = 0 ]; then
  make_curl_cfg "$DST_PAT_FILE" "$CFG_DST"
else
  # reads still work with the source token; no writes happen in dry-run
  cp "$CFG_SRC" "$CFG_DST"
fi
say "repo        $REPO"
say "source      $SRC_REPO (CI build output)"
say "mirror      $MIRROR_REPO (public downloads)"
say "published   $(published_version || true)"
[ "$DRY" = 1 ] && say "mode        DRY RUN (no upload, no commit, no push)"

# ─────────────────────────────────────────────────────────────── 1 detect ──
step "1/6 detect the built assets"
read -r S_LINUX Z_LINUX _ <<< "$(asset_meta "$CFG_SRC" "$SRC_REPO" linux-latest gpn-linux.tar.gz)"
read -r S_WIN   Z_WIN   _ <<< "$(asset_meta "$CFG_SRC" "$SRC_REPO" windows-latest gpn-windows.zip)"
[ -n "$S_LINUX" ] && [ -n "$S_WIN" ] || { echo "ABORT: could not read asset digests" >&2; exit 1; }
say "linux-latest   gpn-linux.tar.gz   $Z_LINUX bytes  sha256=${S_LINUX:0:16}…"
say "windows-latest gpn-windows.zip    $Z_WIN bytes  sha256=${S_WIN:0:16}…"

if published_matches "$S_LINUX" "$Z_LINUX" "$S_WIN" "$Z_WIN"; then
  PUB="$(published_version)"
  if [ -n "$DECLARED_VERSION" ] && [ "$DECLARED_VERSION" != "$PUB" ]; then
    echo "REFUSED: --version $DECLARED_VERSION, but the built assets are already" >&2
    echo "         published as $PUB. Rebuild with the new version." >&2
    exit 2
  fi
  echo "already released $PUB — nothing to do."
  exit 0
fi
say "published manifest does NOT match the built assets"

# ───────────────────────────────────────────────────────────── 2 download ──
step "2/6 download and verify the built assets"
# Fetch through the API asset endpoint (Accept: application/octet-stream): it
# works whether or not the source repo is public, and it is the same bytes the
# digest describes. `-C -` makes an interrupted run resumable.
SRC_BASE="$GPN_GH_API/repos/$SRC_REPO/releases/assets"
fetch_asset() { # api_asset_id file expected_sha label
  local id=$1 file=$2 want=$3 label=$4 got
  # resume only within the same build; a changed digest starts from zero
  if [ "$(cat "$file.expected" 2>/dev/null || true)" != "$want" ]; then
    rm -f "$file"; printf '%s' "$want" > "$file.expected"
  fi
  say "downloading $label…"
  curl -L --fail --silent --show-error --retry 3 --retry-delay 3 --retry-all-errors \
    --connect-timeout 20 -C - -K "$CFG_SRC" \
    -H 'Accept: application/octet-stream' \
    -o "$file" "$SRC_BASE/$id" 2>&1 | scrub || {
      echo "ABORT: download failed for $label" >&2; exit 1; }
  got="$(sha_of "$file")"
  say "$label  $(size_of "$file") bytes  sha256=$got"
  if [ "$got" != "$want" ]; then
    echo "ABORT: $label sha256 $got != GitHub digest $want" >&2
    echo "       (the upload was truncated or the API digest is stale)" >&2
    exit 1
  fi
}
F_LINUX="$WORK/gpn-linux.tar.gz"
F_WIN="$WORK/gpn-windows.zip"
ID_LINUX="$(asset_meta "$CFG_SRC" "$SRC_REPO" linux-latest gpn-linux.tar.gz | cut -d' ' -f3)"
ID_WIN="$(asset_meta "$CFG_SRC" "$SRC_REPO" windows-latest gpn-windows.zip | cut -d' ' -f3)"
fetch_asset "$ID_LINUX" "$F_LINUX" "$S_LINUX" gpn-linux.tar.gz
fetch_asset "$ID_WIN" "$F_WIN" "$S_WIN" gpn-windows.zip

# ────────────────────────────────────────────────────────────── 3 version ──
step "3/6 determine the version"
V_LINUX="$(bundle_stamp "$F_LINUX")"
V_WIN="$(bundle_stamp "$F_WIN")"
VER=""
if [ -n "$V_LINUX" ] && [ -n "$V_WIN" ] && [ "$V_LINUX" != "$V_WIN" ]; then
  echo "ABORT: bundles were built from different versions ($V_LINUX vs $V_WIN)" >&2
  echo "       — they are not one release; rebuild both from the same commit." >&2
  exit 1
fi
[ -n "$V_LINUX" ] && VER="$V_LINUX"
[ -z "$VER" ] && [ -n "$V_WIN" ] && VER="$V_WIN"
if [ -n "$VER" ]; then
  say "version $VER inferred from the linux bundle's gpn-build-v1 stamp"
  if [ -n "$DECLARED_VERSION" ] && [ "$DECLARED_VERSION" != "$VER" ]; then
    echo "ABORT: --version $DECLARED_VERSION disagrees with the bundles ($VER)" >&2
    echo "       the manifest would describe a build nobody has." >&2
    exit 1
  fi
else
  if [ -z "$DECLARED_VERSION" ]; then
    echo "ABORT: the bundles carry no gpn-build-v1 stamp, so the version cannot" >&2
    echo "       be inferred. Pass --version X.Y.Z explicitly (it is then an" >&2
    echo "       assertion, printed, not a guess)." >&2
    exit 1
  fi
  VER="$DECLARED_VERSION"
  say "version $VER from --version (bundles carry no build stamp)"
fi
case "$VER" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "ABORT: '$VER' is not a MAJOR.MINOR.PATCH version" >&2; exit 1 ;;
esac

# The bundles must come from the same commit, and that commit must have passed CI.
C_LINUX="$(bundle_commit "$F_LINUX")"
C_WIN="$(bundle_commit "$F_WIN")"
if [ -n "$C_LINUX" ] && [ -n "$C_WIN" ] && [ "$C_LINUX" != "$C_WIN" ]; then
  echo "ABORT: bundles were built from different commits (${C_LINUX:0:7} vs ${C_WIN:0:7})" >&2
  echo "       -- one version number must mean one codebase." >&2
  exit 1
fi
if [ "${ALLOW_RED:-0}" = 1 ]; then
  say "ALLOW_RED=1 -- skipping the CI check (emergency only)"
else
  require_green_ci "${C_LINUX:-$C_WIN}"
fi

PUB="$(published_version || true)"
if [ -n "$PUB" ]; then
  "$PY" - "$PUB" "$VER" <<'PY' || { echo "ABORT: $VER is not newer than the published $PUB" >&2; exit 1; }
import sys
a = tuple(int(x) for x in sys.argv[1].split("."))
b = tuple(int(x) for x in sys.argv[2].split("."))
sys.exit(0 if b > a else 1)
PY
  say "would replace published $PUB with $VER"
fi

# ─────────────────────────────────────────────────────────────── 4 mirror ──
step "4/6 mirror the assets to $MIRROR_REPO"
mirror_one() { # tag name file sha size
  local tag=$1 name=$2 file=$3 sha=$4 size=$5 rid aid msha
  rid="$(release_id "$CFG_DST" "$MIRROR_REPO" "$tag")"
  # Rename the current asset instead of deleting it first: an upload failure
  # then leaves a complete mirror (the old build), never a 404 for the fleet.
  while read -r aname aid adig; do
    [ "$aname" = "$name" ] || continue
    api "$CFG_DST" PATCH "/repos/$MIRROR_REPO/releases/assets/$aid" \
      -H 'Content-Type: application/json' \
      -d "$("$PY" -c 'import json,sys; print(json.dumps({"name": sys.argv[1]}))' "$name.old-$(date +%s)")" >/dev/null
    say "$tag: renamed the previous $name aside"
  done < <(release_assets "$CFG_DST" "$MIRROR_REPO" "$tag")

  say "$tag: uploading $name ($size bytes)…"
  # 2026-11-06: one 324 MB upload died here with exit 22 and no visible body
  # (the output was piped to /dev/null). A 324 MB body takes ~2 min, which is
  # long enough for the connection to be reset mid-upload, so retry -- and keep
  # the body, because "exit 22" with nothing else is not a diagnosis.
  up_body="$(curl -sS --fail-with-body \
    --retry 3 --retry-delay 5 --retry-all-errors --max-time 1800 \
    -K "$CFG_DST" -X POST \
    -H 'Content-Type: application/octet-stream' \
    -H 'Accept: application/vnd.github+json' \
    --data-binary "@$file" \
    "$GPN_GH_UPLOAD/repos/$MIRROR_REPO/releases/$rid/assets?name=$name" 2>&1)" || {
    echo "ABORT: upload of $name failed: $(printf '%s' "$up_body" | scrub | head -c 400)" >&2
    exit 1
  }
  msha="$(asset_meta "$CFG_DST" "$MIRROR_REPO" "$tag" "$name" | cut -d' ' -f1)"
  [ "$msha" = "$sha" ] || {
    echo "ABORT: mirror digest for $name is $msha, expected $sha" >&2; exit 1; }
  say "$tag: verified mirror digest ${msha:0:16}…"

  # drop superseded copies now that the new one is verified
  while read -r aname aid adig; do
    case "$aname" in "$name".old-*) api "$CFG_DST" DELETE "/repos/$MIRROR_REPO/releases/assets/$aid" >/dev/null ;;
    esac
  done < <(release_assets "$CFG_DST" "$MIRROR_REPO" "$tag")
}

if [ "$SKIP_MIRROR" = 1 ]; then
  say "skipped (--skip-mirror): trusting that the sync workflow already copied them"
elif [ "$DRY" = 1 ]; then
  say "DRY RUN: would upload to $MIRROR_REPO"
  say "  linux-latest   gpn-linux.tar.gz   $Z_LINUX bytes sha256=${S_LINUX:0:16}…"
  say "  windows-latest gpn-windows.zip    $Z_WIN bytes sha256=${S_WIN:0:16}…"
else
  mirror_one linux-latest gpn-linux.tar.gz "$F_LINUX" "$S_LINUX" "$Z_LINUX"
  mirror_one windows-latest gpn-windows.zip "$F_WIN" "$S_WIN" "$Z_WIN"
fi

# ───────────────────────────────────────────────────────────────── 5 sign ──
step "5/6 sign the manifest"
URL_LINUX="$DL_BASE/linux-latest/gpn-linux.tar.gz"
URL_WIN="$DL_BASE/windows-latest/gpn-windows.zip"
OUT="$MANIFEST"
[ "$DRY" = 1 ] && OUT="$TMP/releases.json"

if [ "$MIN_VERSION_SET" = 1 ]; then
  MIN_ARGS=(--min-version "$MIN_VERSION")
else
  KEEP_MIN="$(published_min_version || true)"
  if [ -n "$KEEP_MIN" ]; then
    MIN_ARGS=(--min-version "$KEEP_MIN")
    say "min_version $KEEP_MIN kept from the published manifest (no fleet cutoff change)"
  else
    MIN_ARGS=()
    say "min_version: no published value — using gen-release-manifest's policy default"
  fi
fi

EXPECT_ARGS=()
[ -n "${GPN_VERIFY_KEY:-}" ] && EXPECT_ARGS=(--expect-key "$GPN_VERIFY_KEY")

"$PY" "$GEN" \
  --bundle "linux/x86_64=$URL_LINUX" \
  --bundle "windows/x86_64=$URL_WIN" \
  --bundle-digest "linux/x86_64=$S_LINUX:$Z_LINUX" \
  --bundle-digest "windows/x86_64=$S_WIN:$Z_WIN" \
  --version "$VER" \
  "${MIN_ARGS[@]}" \
  --key "$KEY" \
  --out "$OUT" \
  "${EXPECT_ARGS[@]}"

"$PY" "$GEN" --verify "$OUT" "${EXPECT_ARGS[@]}"

if [ "$DRY" = 1 ]; then
  say "DRY RUN: signed a manifest in $OUT (repository untouched)"
  say "would publish:"
  sed 's/^/   | /' "$OUT"
  echo
  echo "dry run complete — nothing was uploaded, committed or pushed."
  exit 0
fi

# ────────────────────────────────────────────────────────────── 6 publish ──
step "6/6 publish and verify"
"$COMMIT" -m "release: sign $VER (linux $((Z_LINUX/1000000)).$((Z_LINUX/100000%10))MB ${S_LINUX:0:8}, windows $((Z_WIN/1000000)).$((Z_WIN/100000%10))MB ${S_WIN:0:8})" "$MANIFEST_REL"
say "pushing…"
git -C "$REPO" push origin HEAD:main 2>&1 | scrub || {
  echo "ABORT: push failed; the signed manifest is committed locally but not live." >&2; exit 1; }

say "waiting for $RELEASES_URL to serve $VER (up to ${TIMEOUT}s)…"
deadline=$(( $(date +%s) + TIMEOUT ))
while :; do
  live="$(curl -sS --max-time 20 "$RELEASES_URL" 2>/dev/null | "$PY" -c 'import json,sys
try: print(json.load(sys.stdin).get("version",""))
except Exception: print("")' || true)"
  if [ "$live" = "$VER" ]; then say "live manifest is $live"; break; fi
  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "ABORT: $RELEASES_URL still serves '${live:-nothing}' after ${TIMEOUT}s" >&2
    echo "       the commit is pushed; the site deploy is lagging or broken." >&2
    exit 1
  fi
  sleep "$POLL_INTERVAL"
done

say "anonymous download verification…"
for pair in "linux:$URL_LINUX:$S_LINUX" "windows:$URL_WIN:$S_WIN"; do
  os="${pair%%:*}"; rest="${pair#*:}"; url="${rest%:*}"; want="${rest##*:}"
  f="$TMP/verify-$os"
  curl -L --fail --silent --show-error --retry 2 --connect-timeout 20 -o "$f" "$url" || {
    echo "ABORT: anonymous download failed for $url" >&2; exit 1; }
  got="$(sha_of "$f")"
  if [ "$got" != "$want" ]; then
    echo "ABORT: $url serves sha256 $got, manifest says $want" >&2; exit 1
  fi
  say "$os: ${got:0:16}… matches the signed manifest ($(size_of "$f") bytes)"
done

echo
echo "released $VER — https://gpncoin.ai/releases.json"
