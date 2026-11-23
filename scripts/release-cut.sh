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
#  3b. CONTENT  scan the bundles' node binary for the marker strings the
#               version is supposed to contain (scripts/verify-release-
#               artifact.py, queue #193). A build from the wrong tree usually
#               has the right version and the wrong code; this is the only
#               step that looks inside the executable, and it runs BEFORE the
#               asset is mirrored or signed, so a wrong tree is never published.
#   4. MIRROR   replace both assets on the public repo (crewlyllc/gpn_coin),
#               rename-then-upload then verify the mirror's digest.
#   5. SIGN     gen-release-manifest.py with the local ed25519 key; self-verify.
#   6. PUBLISH  commit only website/releases.json, push, poll the live URL.
#   7. VERIFY   anonymous download of each mirrored asset, sha256 vs manifest, and
#               every manifest filename must exist exactly in the release it names
#               (queue #246 — the §8 bug: a green tick against a 404). The same
#               check runs standalone, so §8 can be filled from a command:
#
#                 scripts/release-cut.sh --verify-published [--version X.Y.Z]
#
#               It takes no lock and writes nothing: it fetches the live manifest
#               anonymously, requires each asset filename to exist verbatim in the
#               release its URL points at, range-GETs each URL (206 required) and
#               downloads both assets to compare sha256 and size. Exit 0 means a
#               stranger can install it.
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
#   --verify-published  run only the §8 stranger check against the live manifest
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
RELEASE_LOCK_TOOL="$SCRIPT_DIR/release-lock.sh"
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
VERIFY_PUBLISHED=0
DECLARED_VERSION=""
MIN_VERSION=""
MIN_VERSION_SET=0
TIMEOUT=300

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)      DRY=1; shift ;;
    --skip-mirror)  SKIP_MIRROR=1; shift ;;
    --verify-published) VERIFY_PUBLISHED=1; shift ;;   # queue #246: §8 as a command
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

# ── release lock ──────────────────────────────────────────────────────────
# A signed release must span one fixed tree. The queue driver commits to `main`
# every 30 minutes, so without a marker it can land a commit between "detect the
# assets" and "publish the manifest" — which is exactly what forced a manual
# pause on 2026-11-05. The marker is advisory for the driver (it reads the file);
# the flock makes it authoritative for concurrent release-cuts.
RELEASE_LOCK="${GPN_RELEASE_LOCK:-$HOME/.gpn/release.lock}"
LOCK_FD=9

acquire_release_lock() {
  mkdir -p "$(dirname "$RELEASE_LOCK")"
  exec 9>"$RELEASE_LOCK.flock"
  if ! flock -n 9; then
    echo "REFUSED: another release-cut holds $RELEASE_LOCK.flock" >&2
    exit 2
  fi
  # Same path the driver checks. JSON so a reader can be strict about age.
  printf '{"pid":%d,"started_unix":%d,"step":"%s"}\n' "$$" "$(date +%s)" "detect" > "$RELEASE_LOCK"
}

release_lock_step() {
  # The flock marker (concurrent cuts) and the commit lock (the repo) are
  # different questions; keep both current, and never let either failure abort
  # a release step.
  [ -f "$RELEASE_LOCK" ] && printf '{"pid":%d,"started_unix":%d,"step":"%s"}\n' \
    "$$" "$(date +%s)" "$1" > "$RELEASE_LOCK" 2>/dev/null || true
  inflight="${GPN_RELEASE_LOCK:-$HOME/.gpn/release-in-flight}"
  if [ -f "$inflight" ]; then
    # Keep the version line the lock was TAKEN with. This runs at the top of
    # every step, including `detect` before the version is inferred, so writing
    # "${VER:-release}" unconditionally replaced "0.3.61" with "release" and the
    # `release-lock.sh release 0.3.61` at the end then refused ("held by release,
    # not 0.3.61") -- leaving the repo locked after a GOOD release (2026-11-20).
    local held=""
    held="$(head -n1 "$inflight" 2>/dev/null || true)"
    printf '%s\nholder: release-cut pid %s\nsince:  %s\nstep:   %s\n' \
      "${VER:-$held}" "$$" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$1" > "$inflight" 2>/dev/null || true
  fi
  return 0
}

cleanup() {
  local rc=$?
  [ -n "$TMP" ] && rm -rf "$TMP"
  [ -n "$CFG_SRC" ] && rm -f "$CFG_SRC"
  [ -n "$CFG_DST" ] && rm -f "$CFG_DST"
  # The commit lock must not outlive the process that took it. A lock naming a
  # dead pid protects nothing and blocks everything: the 0.3.63 dry run left
  # `holder: release-cut pid <dead>` behind and every later docs commit refused.
  # On success the publish step has already released it; this is the safety net
  # for dry-run, "already released", and failures.
  inflight_l="${GPN_RELEASE_LOCK:-$HOME/.gpn/release-in-flight}"
  if [ -f "$inflight_l" ] && grep -q "release-cut pid $$\b" "$inflight_l" 2>/dev/null; then
    held_v="$(head -n1 "$inflight_l" 2>/dev/null || true)"
    "$SCRIPT_DIR/release-lock.sh" release "$held_v" >/dev/null 2>&1 \
      || rm -f "$inflight_l"
    printf 'release-cut: released the commit lock (%s) on exit %d\n' "${held_v:-?}" "$rc" >&2
  fi
  rm -f "$RELEASE_LOCK"
  return $rc
}
trap cleanup EXIT
on_err() {
  local rc=$?
  echo "release-cut: FAILED while ${STEP} (exit $rc)" >&2
  exit $rc
}
trap on_err ERR

step() { STEP="$1"; release_lock_step "$1"; printf '\n== %s\n' "$1"; }
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

# ── the stranger's view (queue #246) ───────────────────────────────────────
# Every check below is deliberately CREDENTIAL-FREE: no -K config, no token,
# no cookie jar. The 0.3.68 §8 boxes were ticked against a URL that answered
# 404 to anonymous callers, so a check that borrows the release's own token
# proves nothing about what a stranger can fetch.
anon_status() { # url -> HTTP status for a 1 KiB range request (no auth)
  # -L, because GitHub release downloads answer an anonymous request with a 302
  # to objects.githubusercontent.com; a stranger's browser follows it, so a check
  # that stops at the 302 would refuse every healthy release. What must hold is
  # the FINAL code: 206 (a 200 here means an HTML page, not the asset).
  curl -sSL --max-time 30 --retry 2 --retry-delay 2 -r 0-1023 \
    -o /dev/null -w '%{http_code}' "$1" 2>/dev/null || true
}

anon_asset_names() { # repo tag -> one asset name per line (no auth)
  curl -sS --max-time 20 --retry 2 --retry-delay 2 \
    -H 'Accept: application/vnd.github+json' \
    "$GPN_GH_API/repos/$1/releases/tags/$2" 2>/dev/null | "$PY" -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for a in d.get("assets") or []:
    print(a.get("name", ""))
' || true
}

manifest_asset_fields() { # manifest os -> "sha256 size"
  "$PY" -c '
import json, sys
m = json.load(open(sys.argv[1]))
for a in m.get("assets") or []:
    if a.get("os") == sys.argv[2]:
        print(a.get("sha256", ""), a.get("size", ""))
        break
' "$1" "$2"
}

stranger_check() { # <manifest path|url> [expected version] [1 = download+hash]
  local src="$1" want_ver="${2:-}" full="${3:-0}"
  local tmp ver rows os url repo tag name code got f want_sha want_size anames
  tmp="$(mktemp "${TMP:-${TMPDIR:-/tmp}}/stranger.XXXXXX")"
  case "$src" in
    http://*|https://*)
      curl -sS --max-time 30 --retry 3 --retry-delay 2 -o "$tmp" "$src" || {
        echo "ABORT: stranger check could not fetch $src anonymously" >&2
        rm -f "$tmp"; return 1; } ;;
    *) cp -- "$src" "$tmp" || { rm -f "$tmp"; return 1; } ;;
  esac
  ver="$("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1])).get("version",""))' "$tmp" 2>/dev/null || true)"
  if [ -z "$ver" ]; then
    echo "ABORT: stranger check: $src is not a JSON manifest" >&2
    rm -f "$tmp"; return 1
  fi
  if [ -n "$want_ver" ] && [ "$ver" != "$want_ver" ]; then
    echo "ABORT: stranger check: $src serves version $ver, expected $want_ver" >&2
    rm -f "$tmp"; return 1
  fi
  say "stranger check: $src serves version $ver (anonymous, no token, no cookie jar)"
  rows="$("$PY" - "$tmp" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
for a in d.get("assets") or []:
    url = a.get("url", "")
    parts = url.rstrip("/").split("/")
    try:
        i = parts.index("download")
        if i >= 1 and parts[i-1] == "releases":       # github.com/<o>/<r>/releases/download/<tag>/<name>
            repo, tag, name = f"{parts[i-3]}/{parts[i-2]}", parts[i+1], parts[i+2]
        else:                                          # <base>/download/<o>/<r>/<tag>/<name>
            repo, tag, name = f"{parts[i+1]}/{parts[i+2]}", parts[i+3], parts[i+4]
        print("\t".join([a.get("os", "?"), url, repo, tag, name]))
    except Exception:
        print("\t".join([a.get("os", "?"), url, "-", "-", "-"]))
PY
)" || { rm -f "$tmp"; echo "ABORT: stranger check: unreadable manifest" >&2; return 1; }
  while IFS=$'\t' read -r os url repo tag name; do
    [ -n "$os" ] || continue
    if [ "$name" = "-" ]; then
      echo "ABORT: $os asset URL is not a /releases/download/<tag>/<file> URL: $url" >&2
      rm -f "$tmp"; return 1
    fi
    anames="$(anon_asset_names "$repo" "$tag")"
    if ! printf '%s\n' "$anames" | grep -qxF -- "$name"; then
      echo "ABORT: the manifest names '$name' for $os, but $repo@$tag carries no asset with that exact name. It carries:" >&2
      printf '%s\n' "$anames" | sed 's/^/       /' >&2
      rm -f "$tmp"; return 1
    fi
    code="$(anon_status "$url")"
    if [ "$code" != 206 ]; then
      echo "ABORT: anonymous range GET $url returned '${code:-nothing}', expected 206" >&2
      rm -f "$tmp"; return 1
    fi
    if [ "$full" = 1 ]; then
      f="$TMP/stranger-$os"
      curl -L --fail --silent --show-error --retry 3 --connect-timeout 20 \
        -o "$f" "$url" || {
          echo "ABORT: anonymous download of $url failed" >&2
          rm -f "$tmp"; return 1; }
      read -r want_sha want_size <<< "$(manifest_asset_fields "$tmp" "$os")"
      got="$(sha_of "$f")"
      if [ "$got" != "$want_sha" ] || [ "$(size_of "$f")" != "$want_size" ]; then
        echo "ABORT: $url anonymously served $got / $(size_of "$f") bytes, manifest says $want_sha / $want_size" >&2
        rm -f "$tmp"; return 1
      fi
      say "$os: '$name' downloaded anonymously, $(size_of "$f") bytes, sha256 $got matches the manifest"
    else
      say "$os: '$name' exists exactly in $repo@$tag, anonymous range GET 206"
    fi
  done <<< "$rows"
  rm -f "$tmp"
  return 0
}

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
  verdict="$(ci_verdict_for "$full" "$tok")"
  case "$verdict" in
    completed:success) say "CI green for ${sha:0:7}" ;;
    missing) verify_docs_only_gap "$full" "$sha" "$tok" ;;
    *)
      echo "ABORT: CI for ${sha:0:7} is '$verdict' -- refusing to sign an unverified build" >&2
      exit 1 ;;
  esac
}

# Verdict for one full sha: completed:success | missing | <status>:<conclusion>
ci_verdict_for() { # full_sha token
  curl -sS -H "Authorization: Bearer $2" -H 'Accept: application/vnd.github+json' \
      "$GPN_GH_API/repos/$SRC_REPO/actions/runs?head_sha=$1&per_page=100" | "$PY" -c '
import json,sys
d=json.load(sys.stdin)
ci=[r for r in d.get("workflow_runs",[]) if r.get("name")=="CI"]
if not ci:
    print("missing"); raise SystemExit
ci.sort(key=lambda r: r["created_at"])
print(ci[-1]["status"] + ":" + str(ci[-1].get("conclusion")))'
}

# A docs-only commit never starts a CI run (ci.yml `paths-ignore`), so a bundle
# built from one carries a sha the runs API knows nothing about -- which used to
# look exactly like an unverified build and refused the cut (2026-11-20: the
# 0.3.61 bundles were stamped d2ee08a, three docs-only commits after the last
# green run 409e4d3, and release-cut aborted at 3/6).
#
# The gate's purpose is "never sign a build CI did not verify", so the gap is
# accepted ONLY when it is provable: the bundle sha must be in this checkout,
# its nearest ancestor with a CI run must be GREEN, and every path touched
# between the two must match a `paths-ignore` glob read out of ci.yml itself
# (never a hardcoded copy, so a change to the workflow cannot silently widen
# this). Any code, build file, chainspec, installer, network.json or workflow
# change in the gap still refuses.
verify_docs_only_gap() { # full_sha stamp_sha token
  local full="$1" sha="$2" tok="$3" anc verdict checked=0
  if ! git -C "$REPO" cat-file -e "${full}^{commit}" 2>/dev/null; then
    echo "ABORT: no CI run for commit ${sha:0:7} (${full:0:12}) and that commit is not" >&2
    echo "       in this checkout, so the gap cannot be shown docs-only -- refusing to" >&2
    echo "       sign an unverified build" >&2
    exit 1
  fi
  while read -r anc; do
    [ "$anc" = "$full" ] && continue
    checked=$((checked + 1))
    [ "$checked" -gt 25 ] && break
    verdict="$(ci_verdict_for "$anc" "$tok")"
    case "$verdict" in
      missing) continue ;;
      completed:success)
        if ci_ignored_paths_outside "$anc" "$full"; then
          say "no CI run for ${sha:0:7}: back to ${anc:0:7} every commit touches only"
          say "  paths ci.yml ignores (docs), so those bytes are what CI built -- accepting"
          say "  the green run for ${anc:0:7} as this bundle's verification"
          return 0
        fi
        echo "ABORT: no CI run for ${sha:0:7}, and the gap back to the last verified" >&2
        echo "       commit ${anc:0:7} touches files ci.yml BUILDS -- refusing to sign" >&2
        echo "       an unverified build" >&2
        exit 1 ;;
      *)
        echo "ABORT: no CI run for ${sha:0:7}; its nearest verified ancestor ${anc:0:7}" >&2
        echo "       is '$verdict' -- refusing to sign an unverified build" >&2
        exit 1 ;;
    esac
  done < <(git -C "$REPO" rev-list --first-parent --max-count=26 "$full")
  echo "ABORT: no CI run for commit ${sha:0:7} (${full:0:12}) within 25 commits --" >&2
  echo "       refusing to sign an unverified build" >&2
  exit 1
}

# 0 = every path changed between two commits matches a ci.yml paths-ignore glob
# 1 = something CI builds changed (or the globs cannot be read: then we refuse)
ci_ignored_paths_outside() { # base head
  local ci="$REPO/.github/workflows/ci.yml"
  [ -f "$ci" ] || return 1
  "$PY" - "$ci" "$1" "$2" "$REPO" <<'PY'
import fnmatch, re, subprocess, sys
ci_path, base, head, repo = sys.argv[1:5]
lines = open(ci_path).read().splitlines()
start = None
for i, l in enumerate(lines):
    if re.match(r'^\s{2}push:\s*$', l):
        start = i
        break
if start is None:
    sys.exit(1)
pats, in_pi = [], False
for l in lines[start + 1:]:
    if re.match(r'^\s{2}\S', l):        # next key under `on:` -> push block ended
        break
    if re.match(r'^\s*paths-ignore:\s*$', l):
        in_pi = True
        continue
    if not in_pi:
        continue
    m = re.match(r"^\s*-\s*['\"]?([^'\"]*?)['\"]?\s*$", l)
    if m:
        pats.append(m.group(1))
    elif l.strip() and not l.strip().startswith('#'):
        break
if not pats:
    sys.exit(1)
diff = subprocess.run(["git", "-C", repo, "diff", "--name-only", f"{base}..{head}"],
                      capture_output=True, text=True)
if diff.returncode != 0:
    sys.exit(1)
def ignored(p):
    for pat in pats:
        if fnmatch.fnmatchcase(p, pat):
            return True
        if pat.startswith('**/') and fnmatch.fnmatchcase(p, pat[3:]):
            return True
    return False
touched = [f for f in diff.stdout.splitlines() if f.strip()]
bad = [f for f in touched if not ignored(f)]
if bad:
    print("       not docs-only: " + ", ".join(bad[:5]), file=sys.stderr)
    sys.exit(1)
sys.exit(0)
PY
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

# ───────────────────────────── verify-as-a-stranger mode (#246) ────
# The §8 boxes used to be done by hand, and were ticked against a URL that
# returned 404 to anonymous callers. This mode IS that check, as a command:
# anonymous, filename-exact, and it downloads both assets to compare sha256 and
# size with the manifest. No lock, no repo writes — it reads the live manifest.
if [ "$VERIFY_PUBLISHED" = 1 ]; then
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/gpn-stranger.XXXXXX")"
  step "verify as a stranger (§8)"
  if stranger_check "$RELEASES_URL" "$DECLARED_VERSION" 1; then
    say "PASS: both manifest assets are publicly downloadable by exact filename and match their sha256/size"
    exit 0
  fi
  exit 1
fi

# ─────────────────────────────────────────────────────────────── 1 detect ──
acquire_release_lock
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

# Take the repo lock now: from here until the signed manifest is live, nothing
# else may be committed (#204). Every extra push in this window cancels a queued
# CI run for this release. `scripts/hooks/pre-commit` refuses those commits.
if [ -x "$RELEASE_LOCK_TOOL" ] && [ "$DRY" = 0 ]; then
  $SCRIPT_DIR/release-lock.sh take "$VER" || {
    rc=$?
    echo "ABORT: could not take the release lock for $VER (exit $rc)." >&2
    echo "       Another release is in flight; publish it, or release the lock" >&2
    echo "       deliberately: scripts/release-lock.sh release <version>" >&2
    exit 2
  }
  say "repo locked for $VER (\$HOME/.gpn/release-in-flight) — no commits may land"
else
  echo "WARNING: $RELEASE_LOCK_TOOL is missing or not executable — the repo is NOT" >&2
  echo "         locked, so any commit during this release cancels its CI run." >&2
fi

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

# ──────────────────────────────────────────────── 3b artifact content ──
# The version stamp says what the build *called* itself; only the executable
# says what it *contains*. 0.3.50 shipped fork-choice code that existed in the
# tree but in no published build (queue #192), so "the strings are in git" is
# not evidence. This gate reads the bytes release-cut already downloaded, and
# refuses before mirroring: once an asset is on the public repo, nodes can
# download it, so the check must be *upstream* of that upload.
step "3b/6 artifact carries the code"
"$PY" "$SCRIPT_DIR/verify-release-artifact.py" --no-manifest \
  --bundle "linux=$F_LINUX" --bundle "windows=$F_WIN" \
  --version "$VER" --source-rev "${C_LINUX:-${C_WIN:-HEAD}}" || {
    echo "ABORT: the bundles do not carry the code for $VER (see the markers above)" >&2
    echo "       a release that ships the wrong tree is worse than no release." >&2
    exit 1
  }

# ─────────────────────────────────────────────────────────────── 4 mirror ──
step "4/6 mirror the assets to $MIRROR_REPO"
mirror_one() { # tag name file sha size
  local tag=$1 name=$2 file=$3 sha=$4 size=$5 rid aid msha ts tmp tsha tmpid
  rid="$(release_id "$CFG_DST" "$MIRROR_REPO" "$tag")"
  ts="$(date +%s)"
  tmp="$name.new-$ts"
  # Upload the new bytes under a TEMPORARY name first, while the served name
  # still points at the previous build. The pre-0.3.70 order (rename the old
  # asset aside, then upload) left the download URL 404ing for the whole
  # multi-minute upload — observed 2026-11-22 14:16, and a download in that
  # window yields nothing (or, worse, releases.json still describing the old
  # build while the bytes changed underneath). Uploading under a temp name
  # means the served asset is *always* a complete, verified build.
  say "$tag: uploading $name ($size bytes) as $tmp (the served name keeps the previous build)…"
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
    "$GPN_GH_UPLOAD/repos/$MIRROR_REPO/releases/$rid/assets?name=$tmp" 2>&1)" || {
    echo "ABORT: upload of $name failed: $(printf '%s' "$up_body" | scrub | head -c 400)" >&2
    # Never leave a partial temp asset behind to be mistaken for a build.
    tmpid="$(asset_meta "$CFG_DST" "$MIRROR_REPO" "$tag" "$tmp" 2>/dev/null | cut -d' ' -f3 || true)"
    [ -n "$tmpid" ] && api "$CFG_DST" DELETE "/repos/$MIRROR_REPO/releases/assets/$tmpid" >/dev/null || true
    exit 1
  }
  read -r tsha _ tmpid <<< "$(asset_meta "$CFG_DST" "$MIRROR_REPO" "$tag" "$tmp")"
  [ "$tsha" = "$sha" ] || {
    echo "ABORT: uploaded $tmp has digest $tsha, expected $sha — the served $name is untouched" >&2
    api "$CFG_DST" DELETE "/repos/$MIRROR_REPO/releases/assets/$tmpid" >/dev/null || true
    exit 1
  }
  say "$tag: uploaded and verified $tmp (${tsha:0:16}…) — served name still the previous build"

  # Now make the switch: rename the old asset aside, then rename the verified
  # temp asset onto the served name. Two metadata calls, milliseconds apart —
  # the only inconsistent window left, and it cannot 404 into nothing: at every
  # instant the release holds a complete asset under one name or the other.
  while read -r aname aid adig; do
    [ "$aname" = "$name" ] || continue
    api "$CFG_DST" PATCH "/repos/$MIRROR_REPO/releases/assets/$aid" \
      -H 'Content-Type: application/json' \
      -d "$("$PY" -c 'import json,sys; print(json.dumps({"name": sys.argv[1]}))' "$name.old-$ts")" >/dev/null
    say "$tag: renamed the previous $name aside as $name.old-$ts"
  done < <(release_assets "$CFG_DST" "$MIRROR_REPO" "$tag")

  api "$CFG_DST" PATCH "/repos/$MIRROR_REPO/releases/assets/$tmpid" \
    -H 'Content-Type: application/json' \
    -d "$("$PY" -c 'import json,sys; print(json.dumps({"name": sys.argv[1]}))' "$name")" >/dev/null
  say "$tag: $tmp is now served as $name"

  msha="$(asset_meta "$CFG_DST" "$MIRROR_REPO" "$tag" "$name" | cut -d' ' -f1)"
  [ "$msha" = "$sha" ] || {
    echo "ABORT: mirror digest for $name is $msha, expected $sha" >&2; exit 1; }
  say "$tag: verified mirror digest ${msha:0:16}…"

  # drop superseded copies now that the new one is verified
  while read -r aname aid adig; do
    case "$aname" in
      "$name".old-*) api "$CFG_DST" DELETE "/repos/$MIRROR_REPO/releases/assets/$aid" >/dev/null ;;
      # A temp asset from an interrupted earlier cut is not a build anyone
      # should download; remove it once the real name is verified.
      "$name".new-*) api "$CFG_DST" DELETE "/repos/$MIRROR_REPO/releases/assets/$aid" >/dev/null ;;
    esac
  done < <(release_assets "$CFG_DST" "$MIRROR_REPO" "$tag")
}

if [ "$SKIP_MIRROR" = 1 ]; then
  say "skipped (--skip-mirror): trusting that the sync workflow already copied them"
elif [ "$DRY" = 1 ]; then
  say "DRY RUN: would upload to $MIRROR_REPO"
  say "  linux-latest   gpn-linux.tar.gz   $Z_LINUX bytes sha256=${S_LINUX:0:16}…"
  say "  windows-latest gpn-windows.zip    $Z_WIN bytes sha256=${S_WIN:0:16}…"
  say "  windows-latest gpn-windows.zip.bundle (provenance, mirrored with the zip)"
else
  mirror_one linux-latest gpn-linux.tar.gz "$F_LINUX" "$S_LINUX" "$Z_LINUX"
  mirror_one windows-latest gpn-windows.zip "$F_WIN" "$S_WIN" "$Z_WIN"

  # ── 4b. the provenance bundle travels with the zip it signs ──────────────
  # Until 2026-11-20 this step mirrored only the two installers. The old zip
  # was renamed aside but the *previous* .bundle stayed on windows-latest, so
  # 0.3.63's mirror carried a bundle whose payload digest was 0.3.62's zip:
  # `cosign verify-blob` on the served pair failed, and nothing caught it,
  # because releases.json never mentions the bundle. The bundle is the only
  # provenance a stranger can check — a missing or stale one refuses the cut.
  B_NAME="gpn-windows.zip.bundle"
  B_LINE="$(release_assets "$CFG_SRC" "$SRC_REPO" windows-latest \
    | awk -v n="$B_NAME" '$1==n{print $2, substr($3,8)}')"
  [ -n "$B_LINE" ] || {
    echo "ABORT: $SRC_REPO windows-latest carries no $B_NAME." >&2
    echo "       Mirroring the zip without provenance: refusing." >&2
    exit 1
  }
  read -r B_ID B_SHA <<< "$B_LINE"
  F_BUNDLE="$TMP/$B_NAME"
  curl -L --fail --silent --show-error --retry 3 --retry-delay 3 --retry-all-errors \
    --connect-timeout 20 -K "$CFG_SRC" -H 'Accept: application/octet-stream' \
    -o "$F_BUNDLE" "$SRC_BASE/$B_ID" 2>&1 | scrub || {
      echo "ABORT: download of $B_NAME failed" >&2; exit 1; }
  [ "$(sha_of "$F_BUNDLE")" = "$B_SHA" ] || {
    echo "ABORT: $B_NAME does not match the source digest — refusing" >&2; exit 1; }
  mirror_one windows-latest "$B_NAME" "$F_BUNDLE" "$B_SHA" "$(size_of "$F_BUNDLE")"
fi

# ───────────────────────────────────────────────────── 4c public check ──
# Row 245: the version must not be advertised on any surface until an
# anonymous stranger can fetch the exact bytes the manifest will describe. This
# runs BEFORE signing/publishing, so releases.json never names a version whose
# asset is missing or different — the 2026-11-22 window (URL 404, then a URL
# serving a zip whose sha the manifest did not match) is impossible here.
step "4c/6 the public assets serve the built bytes"
URL_LINUX="$DL_BASE/linux-latest/gpn-linux.tar.gz"
URL_WIN="$DL_BASE/windows-latest/gpn-windows.zip"
if [ "$DRY" = 1 ]; then
  say "DRY RUN: skipping — a dry run does not mirror, so the public URL still serves the previous build"
else
# Filename-exact, anonymous (queue #246): the manifest this cut is about to sign
# names these two files, so the release it points at must carry them under those
# exact spellings. On 2026-11-22 windows-latest held only
# `gpn-windows.zip.old-1795385826` while the manifest named `gpn-windows.zip`;
# the generic 404 never said which side was wrong.
for pair in "linux-latest:gpn-linux.tar.gz" "windows-latest:gpn-windows.zip"; do
  tag="${pair%%:*}"; want_name="${pair#*:}"
  names="$(anon_asset_names "$MIRROR_REPO" "$tag")"
  printf '%s\n' "$names" | grep -qxF -- "$want_name" || {
    echo "ABORT: $MIRROR_REPO@$tag does not carry '$want_name' exactly, so the manifest would name an asset that 404s. It carries:" >&2
    printf '%s\n' "$names" | sed 's/^/       /' >&2
    exit 1; }
  say "$tag carries $want_name (exact filename)"
done
for pair in "linux:$URL_LINUX:$S_LINUX" "windows:$URL_WIN:$S_WIN"; do
  os="${pair%%:*}"; rest="${pair#*:}"; url="${rest%:*}"; want="${rest##*:}"
  code="$(anon_status "$url")"
  if [ "$code" != 206 ]; then
    echo "ABORT: anonymous range GET $url returned '${code:-nothing}', expected 206." >&2
    echo "       The asset is not publicly readable, so no manifest may name it." >&2
    exit 1
  fi
  f="$TMP/public-$os"
  curl -L --fail --silent --show-error --retry 3 --connect-timeout 20 \
    -o "$f" "$url" 2>&1 | scrub || {
      echo "ABORT: anonymous download failed for $url" >&2; exit 1; }
  got="$(sha_of "$f")"
  [ "$got" = "$want" ] || {
    echo "ABORT: $url anonymously serves sha256 $got, but the build is $want" >&2
    echo "       Refusing to sign a manifest that describes different bytes." >&2
    exit 1
  }
  say "$os: anonymous GET 206 + sha256 ${got:0:16}… + $(size_of "$f") bytes match the build"
done
fi

# ───────────────────────────────────────────────────────────────── 5 sign ──
step "5/6 sign the manifest"
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
  # A dry run commits nothing, so it must not hold the commit lock either:
  # until 2026-11-20 it took it in acquire_release_lock and exited here without
  # releasing, which left ~/.gpn/release-in-flight naming a dead pid and made
  # every later docs commit refuse (hit by the 0.3.63 cut).
  exit 0
fi

# ────────────────────────────────────────────────────────────── 6 publish ──
step "6/6 publish and verify"
# The publish commit must NOT be refused by the lock this release is holding: the
# #204 guard stops *foreign* commits, and this commit is the release's own. Without
# --no-verify the hook aborts step 6 after the mirror was already replaced, leaving
# public assets at the new version and releases.json at the old one (#134).
"$COMMIT" --no-verify -m "release: sign $VER (linux $((Z_LINUX/1000000)).$((Z_LINUX/100000%10))MB ${S_LINUX:0:8}, windows $((Z_WIN/1000000)).$((Z_WIN/100000%10))MB ${S_WIN:0:8})" "$MANIFEST_REL"
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
# The bytes were already fetched anonymously in 4c (and those are the digests in
# the signed manifest). This post-publish check proves the file that just went
# ".live" still describes them, and that the URL is still publicly readable —
# re-downloading 550 MB a second time would prove nothing extra.
for pair in "linux|$URL_LINUX|$S_LINUX|$Z_LINUX" "windows|$URL_WIN|$S_WIN|$Z_WIN"; do
  IFS='|' read -r os url want wsize <<< "$pair"
  code="$(anon_status "$url")"
  [ "$code" = 206 ] || {
    echo "ABORT: after publish, anonymous range GET $url returned '${code:-nothing}', expected 206" >&2
    exit 1; }
  read -r msha msize <<< "$("$PY" - "$OUT" "$os" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
for a in m["assets"]:
    if a["os"] == sys.argv[2]:
        print(a["sha256"], a["size"])
PY
)"
  if [ "$msha" != "$want" ] || [ "$msize" != "$wsize" ]; then
    echo "ABORT: the live manifest says $msha / $msize bytes for $os, but the cut built $want / $wsize" >&2
    exit 1
  fi
  say "$os: ${msha:0:16}… matches the signed manifest ($msize bytes), anonymous GET 206"
done

# The same check as §8, against the manifest that is actually live: every asset
# must exist under its exact filename in the release its URL points at, and be
# readable anonymously. Post-publish the bytes were already fetched in 4c, so
# this does not download them again — only the post-4c fact (the live manifest
# names what the tag really carries) is re-proven.
stranger_check "$RELEASES_URL" "$VER" 0 || {
  echo "ABORT: the published manifest does not survive the stranger check." >&2
  exit 1; }

# 6b release the repo lock: the manifest is live and verified, so the release no
# longer owns the tree. Failure to release is loud but not fatal — the release did
# publish, and a stale lock must be visible rather than silently kept.
if [ -x "$RELEASE_LOCK_TOOL" ]; then
  $SCRIPT_DIR/release-lock.sh release "$VER" || \
    echo "WARNING: the repo lock is still held — run scripts/release-lock.sh status" >&2
fi

echo
echo "released $VER — https://gpncoin.ai/releases.json"
