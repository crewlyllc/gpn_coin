#!/usr/bin/env bash
# test-release-cut.sh — scenario tests for scripts/release-cut.sh.
#
# Runs the real script against a fake GitHub API (fake-github-api.py) and a
# throwaway git repo with a local bare remote, so nothing touches GitHub, the
# production key, or the real 300 MB bundles.
#
#   scripts/tests/test-release-cut.sh            # all scenarios
#   scripts/tests/test-release-cut.sh t3 t5      # selected scenarios
#
# Every scenario builds a fresh temp root: repo + state + server + keys.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SERVER="$HERE/fake-github-api.py"
PY="${PYTHON:-/usr/bin/python3}"
[ -x "$PY" ] || PY="$(command -v python3)"

PASS=0
FAIL=0
CURRENT=""
FAILED=()

note() { printf '   %s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '   \033[32mok\033[0m %s\n' "$*"; }
bad()  { printf '   \033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); FAILED+=("$CURRENT: $*"); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

# ─────────────────────────────────────────────────────────── fixtures ──────
make_bundle() { # file version
  "$PY" - "$1" "$2" <<'PY'
import sys, tarfile, zipfile, io, os
path, ver = sys.argv[1], sys.argv[2]
stamp = f"gpn-build-v1 version={ver} sha=abcdef1 spec=41\n".encode()
payload = stamp + b"fake node bytes " + os.urandom(64)
if path.endswith(".zip"):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_STORED) as z:
        z.writestr("gpn_node.exe", stamp)
        z.writestr("gpn-node.exe", payload)
    z.close()
    with zipfile.ZipFile(path, "a", zipfile.ZIP_STORED) as z:
        z.writestr("README.txt", payload)
else:
    with tarfile.open(path, "w:gz") as t:
        for name, data in (("gpn_node", stamp), ("gpn-node", payload),
                           ("README.txt", payload)):
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            t.addfile(ti, io.BytesIO(data))
PY
}

write_manifest() { # path version file_linux file_win mirror_base
  "$PY" - "$@" <<'PY'
import hashlib, json, sys
from pathlib import Path
path, ver, fl, fw, base = sys.argv[1:6]
def a(osname, tag, name, f):
    p = Path(f)
    return {"os": osname, "arch": "x86_64",
            "url": f"{base}/{tag}/{name}",
            "sha256": hashlib.sha256(p.read_bytes()).hexdigest(),
            "size": p.stat().st_size, "layout": "bundle"}
man = {"version": ver, "min_version": "0.1.0",
       "published_at": "2026-10-30T00:00:00Z",
       "assets": [a("linux", "linux-latest", "gpn-linux.tar.gz", fl),
                  a("windows", "windows-latest", "gpn-windows.zip", fw)]}
Path(path).write_text(json.dumps(man, indent=2) + "\n")
PY
}

setup() { # published_version built_version [--no-published]
  T="$(mktemp -d "${TMPDIR:-/tmp}/release-cut-test.XXXXXX")"
  R="$T/repo"
  mkdir -p "$T/state" "$R/website" "$R/scripts"
  cp "$REPO_ROOT/scripts/release-cut.sh" "$REPO_ROOT/scripts/gen-release-manifest.py" \
     "$REPO_ROOT/scripts/commit-paths.sh" "$R/scripts/"
  chmod +x "$R/scripts/"*
  PUB_VER="$1"; BUILT_VER="$2"; NO_PUB="${3:-}"

  make_bundle "$T/old-linux.tar.gz" "$PUB_VER"
  make_bundle "$T/old-windows.zip" "$PUB_VER"
  make_bundle "$T/gpn-linux.tar.gz" "$BUILT_VER"
  make_bundle "$T/gpn-windows.zip" "$BUILT_VER"
  cp "$T/gpn-linux.tar.gz" "$T/src-linux.tar.gz"
  cp "$T/gpn-windows.zip" "$T/src-windows.zip"

  # throwaway signing key (never the production key)
  "$PY" - "$T/key.txt" "$T/pub.txt" <<'PY'
import sys, pathlib, nacl.signing
sk = nacl.signing.SigningKey.generate()
p = pathlib.Path(sys.argv[1])
p.write_text("# test key\n" + sk.encode().hex() + "\n"); p.chmod(0o600)
pathlib.Path(sys.argv[2]).write_text(sk.verify_key.encode().hex())
PY
  echo "test-src-token-not-a-real-credential" > "$T/src.pat"
  echo "test-dst-token-not-a-real-credential" > "$T/dst.pat"

  "$PY" - "$T" <<'PY'
import json, sys, pathlib
T = pathlib.Path(sys.argv[1])
state = {
  "source": {"linux-latest": [{"name": "gpn-linux.tar.gz", "file": str(T/"src-linux.tar.gz")}],
             "windows-latest": [{"name": "gpn-windows.zip", "file": str(T/"src-windows.zip")}]},
  "mirror": {"linux-latest": [{"name": "gpn-linux.tar.gz", "file": str(T/"old-linux.tar.gz")}],
             "windows-latest": [{"name": "gpn-windows.zip", "file": str(T/"old-windows.zip")}]},
  "site_path": str(T/"repo/website/releases.json"),
  "fail_upload": False,
}
(T/"state/state.json").write_text(json.dumps(state, indent=2))
(T/"state/actions.log").write_text("")
PY

  # A released manifest describes the OLD assets. When the published version
  # equals the built one, the published manifest is by definition over the very
  # same bytes — that is the "nothing left to do" state.
  local man_linux="$T/old-linux.tar.gz" man_win="$T/old-windows.zip"
  if [ "$PUB_VER" = "$BUILT_VER" ]; then
    man_linux="$T/src-linux.tar.gz"; man_win="$T/src-windows.zip"
  fi
  if [ "$NO_PUB" = "--no-published" ]; then
    rm -f "$R/website/releases.json"
  else
    write_manifest "$R/website/releases.json" "$PUB_VER" "$man_linux" \
      "$man_win" "http://x/download/mirror/gpn"
  fi

  git -C "$R" init -q -b main
  git -C "$R" config user.email test@example.com
  git -C "$R" config user.name "release-cut test"
  git -C "$R" add -A
  git -C "$R" commit -qm "initial"
  git init -q --bare "$T/remote.git"
  git -C "$R" remote add origin "$T/remote.git"
  git -C "$R" push -q origin main

  "$PY" "$SERVER" "$T/state" "$T/port" &
  SRV_PID=$!
  for _ in $(seq 1 100); do [ -s "$T/port" ] && break; sleep 0.05; done
  PORT="$(cat "$T/port")"
  BASE="http://127.0.0.1:$PORT"

  export GPN_GH_API="$BASE" GPN_GH_UPLOAD="$BASE" \
         GPN_SOURCE_REPO="src/gpn" GPN_MIRROR_REPO="mirror/gpn" \
         GPN_RELEASES_URL="$BASE/site/releases.json" \
         GPN_RELEASE_DOWNLOAD_BASE="$BASE/download/mirror/gpn" \
         GPN_RELEASE_KEY="$T/key.txt" GPN_VERIFY_KEY="$(cat "$T/pub.txt")" \
         GPN_GPN_PAT_FILE="$T/src.pat" GPN_CREWLYLLC_PAT_FILE="$T/dst.pat" \
         GPN_POLL_INTERVAL=1 GPN_WORK_DIR="$T/work"
  git -C "$R" config --global --get user.email >/dev/null 2>&1 || true
}

teardown() {
  [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null
  wait "$SRV_PID" 2>/dev/null
  SRV_PID=""
  [ -n "${T:-}" ] && rm -rf "$T"
  T=""
}

cut() { # args -> runs the script in the temp repo; output in $OUT, status $RC
  OUT="$(cd "$R" && ./scripts/release-cut.sh "$@" 2>&1)"
  RC=$?
  [ -n "${VERBOSE:-}" ] && printf '%s\n' "$OUT" | sed 's/^/   | /'
  return 0
}

set_state() { # python expression mutating state dict `d`
  "$PY" - "$T/state/state.json" "$1" <<'PY'
import json, sys
p, expr = sys.argv[1], sys.argv[2]
d = json.load(open(p))
exec(expr, {"d": d, "p": p})
json.dump(d, open(p, "w"), indent=2)
PY
}

commits() { git -C "$R" rev-list --count HEAD; }
uploads() { grep -c '^UPLOAD' "$T/state/actions.log" 2>/dev/null || true; }
actions() { cat "$T/state/actions.log"; }

# ─────────────────────────────────────────────────────────── scenarios ─────
t1_nothing_to_do() {
  CURRENT="t1 nothing-to-do"
  setup 0.3.4 0.3.4
  local before; before="$(commits)"
  cut --dry-run
  check '[ "$RC" = 0 ]' "exit 0"
  check 'grep -q "already released 0.3.4" <<< "$OUT"' "prints 'already released 0.3.4'"
  check '[ "$(commits)" = "$before" ]' "no commit created"
  check '[ -z "$(actions)" ]' "no API writes at all"
  check '! grep -q REFUSED <<< "$OUT"' "no refusal"
  teardown
}

t2_detection_and_dry_run() {
  CURRENT="t2 detection + dry-run"
  setup 0.3.4 0.3.5
  local before manifest_before; before="$(commits)"
  manifest_before="$(cat "$R/website/releases.json")"
  cut --dry-run
  check '[ "$RC" = 0 ]' "exit 0"
  check 'grep -q "version 0.3.5 inferred from the linux bundle" <<< "$OUT"' "infers 0.3.5 from the bundle stamp"
  check 'grep -q "would replace published 0.3.4" <<< "$OUT"' "knows it supersedes 0.3.4"
  check 'grep -q "dry run complete" <<< "$OUT"' "ends as a dry run"
  check '[ "$(commits)" = "$before" ]' "no commit created"
  check '[ "$(cat "$R/website/releases.json")" = "$manifest_before" ]' "releases.json untouched"
  check '[ -z "$(actions)" ]' "no uploads / renames / deletes"
  check '[ "$(git -C "$R" status --porcelain)" = "" ]' "working tree clean"
  # the manifest it would publish is signed and self-verified
  check 'grep -q "OK: 0.3.5" <<< "$OUT"' "signed manifest verifies against the expected key"
  teardown
}

t3_digest_mismatch() {
  CURRENT="t3 digest mismatch"
  setup 0.3.4 0.3.5
  set_state 'd["source"]["linux-latest"][0]["digest_override"] = "sha256:" + "d"*64'
  cut
  check '[ "$RC" != 0 ]' "exits non-zero"
  check 'grep -q "sha256 .* != GitHub digest" <<< "$OUT"' "names the mismatch"
  check '[ -z "$(actions)" ]' "aborts BEFORE any upload/rename/delete"
  check '! grep -q UPLOAD "$T/state/actions.log"' "no upload reached the mirror"
  teardown
}

t4_mirror_failure() {
  CURRENT="t4 mirror failure"
  setup 0.3.4 0.3.5
  set_state 'd["fail_upload"] = True'
  local before; before="$(commits)"
  cut
  check '[ "$RC" != 0 ]' "exits non-zero"
  check 'grep -q "FAILED while" <<< "$OUT"' "reports the failing step"
  check '! grep -q "^UPLOAD" "$T/state/actions.log"' "no successful upload"
  check '[ "$(commits)" = "$before" ]' "nothing committed"
  check '[ "$(git -C "$R" status --porcelain -- website/releases.json)" = "" ]' "manifest not rewritten"
  teardown
}

t5_dirty_manifest() {
  CURRENT="t5 dirty releases.json"
  setup 0.3.4 0.3.5
  printf '\n' >> "$R/website/releases.json"
  cut --dry-run
  check '[ "$RC" = 2 ]' "refuses with exit 2 (even for --dry-run)"
  check 'grep -q "REFUSED: website/releases.json is dirty" <<< "$OUT"' "explains why"
  check '[ -z "$(actions)" ]' "no API writes"
  if grep -q "dry run complete" <<< "$OUT"; then bad "proceeded despite a dirty manifest"; else ok "stopped before doing any work"; fi
  teardown
}

t6_full_publish() {
  CURRENT="t6 full publish"
  setup 0.3.4 0.3.5
  cut --timeout 20
  check '[ "$RC" = 0 ]' "exit 0"
  check 'grep -q "released 0.3.5" <<< "$OUT"' "announces 0.3.5"
  check 'grep -q "anonymous download" <<< "$OUT"' "ran the anonymous verification"
  check 'grep -q "matches the signed manifest" <<< "$OUT"' "download sha256 matched"
  check '[ "$(uploads)" = 2 ]' "uploaded exactly two assets"
  check '! grep -q "^PATCH .*old-" <(grep -v "^DELETE" "$T/state/actions.log") || grep -q "^DELETE .*\.old-" "$T/state/actions.log"' "superseded copies cleaned up"
  local man_ver; man_ver="$("$PY" -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$R/website/releases.json")"
  check '[ "$man_ver" = 0.3.5 ]' "releases.json now says 0.3.5"
  check 'grep -q "release: sign 0.3.5" <<< "$(git -C "$R" log --oneline -1)"' "commit message names the version"
  check '[ -z "$(git -C "$R" status --porcelain)" ]' "no uncommitted leftovers"
  check 'git -C "$T/remote.git" log --oneline main | grep -q "release: sign 0.3.5"' "pushed to the remote"
  check '[ "$(git -C "$R" show --pretty=format: --name-only HEAD | grep -v "^$")" = "website/releases.json" ]' "commit touches only the manifest"
  # idempotent second run
  cut
  check '[ "$RC" = 0 ]' "second run exits 0"
  check 'grep -q "already released 0.3.5" <<< "$OUT"' "second run is a quiet no-op"
  teardown
}

t7_version_assertion() {
  CURRENT="t7 --version disagreement"
  setup 0.3.4 0.3.5
  cut --dry-run --version 0.4.0
  check '[ "$RC" != 0 ]' "exits non-zero"
  check 'grep -q "disagrees with the bundles" <<< "$OUT"' "says the assertion failed"
  check '[ -z "$(actions)" ]' "no uploads"
  teardown
}

t8_skip_mirror() {
  CURRENT="t8 --skip-mirror"
  setup 0.3.4 0.3.5
  # pretend the (fixed) sync workflow already copied the new bytes over
  set_state 'd["mirror"]["linux-latest"][0]["file"] = d["source"]["linux-latest"][0]["file"]; d["mirror"]["windows-latest"][0]["file"] = d["source"]["windows-latest"][0]["file"]'
  cut --skip-mirror --timeout 20
  check '[ "$RC" = 0 ]' "exit 0"
  check '[ "$(uploads)" = 0 ]' "did not upload"
  check 'grep -q "skipped (--skip-mirror)" <<< "$OUT"' "says it skipped the mirror"
  teardown
}

ALL=(t1_nothing_to_do t2_detection_and_dry_run t3_digest_mismatch t4_mirror_failure
     t5_dirty_manifest t6_full_publish t7_version_assertion t8_skip_mirror)

want=("$@")
printf '\nrelease-cut.sh — %d scenario(s)\n' "${#ALL[@]}"
for t in "${ALL[@]}"; do
  id="${t%%_*}"
  if [ "${#want[@]}" -gt 0 ] && [[ ! " ${want[*]} " =~ " $id " ]]; then continue; fi
  printf '\n▸ %s\n' "$t"
  "$t"
done

printf '\n%d checks passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
  printf 'failures:\n'; printf '  - %s\n' "${FAILED[@]}"; exit 1
fi
exit 0
