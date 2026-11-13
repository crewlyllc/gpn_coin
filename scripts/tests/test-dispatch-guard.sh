#!/usr/bin/env bash
# test-dispatch-guard.sh — scenario tests for the in-flight guard in
# scripts/dispatch-release.sh (W145, 2026-11-12).
#
# The defect being pinned: the release lane refused two *signings* (release-cut's
# flock) but nothing refused two *dispatches*. Two drivers could each read "no
# builds running" and queue ~40 min of duplicate runner time for the same tag;
# W142 only avoided that by luck. This suite proves the guard refuses before the
# first `workflow_dispatch` call, and that it does NOT refuse a clean dispatch.
#
# Runs the real script against a local fake GitHub API and a throwaway git repo
# with a local bare remote, so nothing touches GitHub or the real runner lane.
#
#   scripts/tests/test-dispatch-guard.sh            # all scenarios
#   scripts/tests/test-dispatch-guard.sh t2 t4      # selected scenarios
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/dispatch-release.sh"
PY="${PYTHON:-/usr/bin/python3}"
[ -x "$PY" ] || PY="$(command -v python3)"

PASS=0
FAIL=0
CURRENT=""
FAILED=()

note() { printf '   %s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '   \033[32mok\033[0m %s\n' "$*"; }
bad()  { printf '   \033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); FAILED+=("$CURRENT: $*"); dump; }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

# ─────────────────────────────────────────────────────── fake GitHub API ────
# Serves /repos/<repo>/actions/runs and records every dispatch POST, which is
# the only thing the assertions care about: "was a workflow_dispatch sent?"

FAKE_API_PID=""
TMPROOT=""
cleanup() {
  [ -n "$FAKE_API_PID" ] && kill "$FAKE_API_PID" 2>/dev/null
  [ -n "$TMPROOT" ] && rm -rf "$TMPROOT"
}
trap cleanup EXIT

# fresh fixture per scenario: git repo + bare remote + state dir + fake API
setup() {
  TMPROOT="$(mktemp -d)"
  STATE="$TMPROOT/state"; mkdir -p "$STATE"

  git init -q --bare "$TMPROOT/remote.git"
  git init -q "$TMPROOT/repo"
  git -C "$TMPROOT/repo" config user.email t@t
  git -C "$TMPROOT/repo" config user.name t
  printf 'x\n' >"$TMPROOT/repo/f"
  git -C "$TMPROOT/repo" add f
  git -C "$TMPROOT/repo" commit -qm init
  git -C "$TMPROOT/repo" remote add origin "$TMPROOT/remote.git"
  SHA="$(git -C "$TMPROOT/repo" rev-parse HEAD)"

  mkdir -p "$TMPROOT/bin"
  printf 'https://jeremyzguo:fake-token@example.invalid\n' >"$TMPROOT/bin/creds"
  printf 'fake-token' >"$TMPROOT/bin/token"

  # The fake API lives in the throwaway root, not in the repo (nothing to
  # clean up and nothing to accidentally commit).
  cat >"$TMPROOT/fake-runs-api.py" <<'PY'
import json, os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = sys.argv[1]          # dir: runs.json (body for actions/runs), log
PORT_FILE = sys.argv[2]


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _json(self, code, body):
        raw = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        if self.path.startswith("/repos/") and "/actions/runs" in self.path:
            p = os.path.join(STATE, "runs.json")
            body = open(p, "rb").read() if os.path.exists(p) else b'{"workflow_runs":[]}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self._json(404, {"message": "no"})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        if n:
            self.rfile.read(n)
        with open(os.path.join(STATE, "dispatch.log"), "a") as f:
            f.write(self.path + "\n")
        self._json(204, {})


srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
open(PORT_FILE, "w").write(str(srv.server_address[1]))
srv.serve_forever()
PY

  printf '{"workflow_runs":[]}' >"$STATE/runs.json"

  : >"$STATE/dispatch.log"
  "$PY" "$TMPROOT/fake-runs-api.py" "$STATE" "$TMPROOT/port" >/dev/null 2>&1 &
  FAKE_API_PID=$!
  for _ in $(seq 1 100); do
    [ -s "$TMPROOT/port" ] && break
    sleep 0.05
  done
  API="http://127.0.0.1:$(cat "$TMPROOT/port")"
}

teardown() {
  kill "$FAKE_API_PID" 2>/dev/null
  FAKE_API_PID=""
  rm -rf "$TMPROOT"
  TMPROOT=""
}

# Run the real script with the fixture's repo/token/API/lock. `git -C repo` is
# inside the temp repo, so the script's own `git -C "$repo_root"` calls address
# it — the script is copied in so `$(dirname $0)/..` resolves to the fixture.
run_dispatch() {
  # The script resolves its repo as `dirname $0/..`, so it must sit one level
  # below the fixture repo — copy it to <repo>/scripts/, not <repo>/.
  mkdir -p "$TMPROOT/repo/scripts"
  cp "$SCRIPT" "$TMPROOT/repo/scripts/dispatch-release.sh"
  local script="$TMPROOT/repo/scripts/dispatch-release.sh"
  ( cd "$TMPROOT/repo" && \
    GPN_REPO=fake/repo \
    GPN_GH_API="$API" \
    GPN_RELEASE_LOCK="$TMPROOT/lock/release.lock" \
    GPN_GITHUB_TOKEN_FILE="$TMPROOT/bin/token" \
    HOME="$TMPROOT/home" \
    bash "$script" "$@" ) >"$TMPROOT/out" 2>"$TMPROOT/err"
  RC=$?
}

dispatches() { [ -f "$STATE/dispatch.log" ] && wc -l <"$STATE/dispatch.log" || echo 0; }
out() { cat "$TMPROOT/out" "$TMPROOT/err"; }
# On a failing check, show what the script actually said — a bare "exit 0"
# failure tells a future reader nothing about which line refused.
dump() {
  [ -n "${TMPROOT:-}" ] || return 0
  printf '     --- rc=%s ---\n' "${RC:-?}"
  { cat "$TMPROOT/out" 2>/dev/null; cat "$TMPROOT/err" 2>/dev/null; } | sed 's/^/     /' | tail -25
}

# ────────────────────────────────────────────────────────────── scenarios ──
t1_clean_dispatch_proceeds() {
  CURRENT=t1; note "t1: no cut, no build → dispatch happens"
  setup
  run_dispatch "$SHA"
  check '[ "$RC" = 0 ]' "exit 0"
  check '[ "$(dispatches)" = 3 ]' "3 workflow_dispatch POSTs (got $(dispatches))"
  teardown
}

t2_held_flock_refuses() {
  CURRENT=t2; note "t2: a release-cut holds the flock (lock JSON absent) → refuse"
  setup
  mkdir -p "$TMPROOT/lock"
  # Hold the flock exactly as release-cut.sh does: fd on the .flock file.
  ( exec 9>"$TMPROOT/lock/release.lock.flock"; flock -n 9 || exit 9; sleep 20 ) &
  HOLDER=$!
  sleep 0.3
  run_dispatch "$SHA"
  check '[ "$RC" = 3 ]' "exit 3 (got $RC)"
  check '[ "$(dispatches)" = 0 ]' "no workflow_dispatch sent (got $(dispatches))"
  check 'grep -q "holds .*release.lock.flock" "$TMPROOT/err"' "names the flock in the refusal"
  kill "$HOLDER" 2>/dev/null; wait "$HOLDER" 2>/dev/null
  teardown
}

t3_running_cut_refuses() {
  CURRENT=t3; note "t3: a release-cut.sh process is running → refuse"
  setup
  cp "$(command -v sleep)" "$TMPROOT/bin/release-cut.sh"
  "$TMPROOT/bin/release-cut.sh" 20 &
  CUT=$!
  sleep 0.2
  run_dispatch "$SHA"
  check '[ "$RC" = 3 ]' "exit 3 (got $RC)"
  check '[ "$(dispatches)" = 0 ]' "no workflow_dispatch sent (got $(dispatches))"
  check 'grep -q "release-cut.sh is already running" "$TMPROOT/err"' "names the running cut"
  kill "$CUT" 2>/dev/null; wait "$CUT" 2>/dev/null
  teardown
}

t4_inflight_build_refuses() {
  CURRENT=t4; note "t4: a build is already running for this commit → refuse"
  setup
  "$PY" - "$STATE/runs.json" "$SHA" <<'PY'
import json, sys
path, sha = sys.argv[1], sys.argv[2]
json.dump({"workflow_runs": [
    {"head_sha": sha, "path": ".github/workflows/release-linux-selfhosted.yml",
     "status": "in_progress", "conclusion": None},
    {"head_sha": sha, "path": ".github/workflows/release-windows.yml",
     "status": "queued", "conclusion": None},
]}, open(path, "w"))
PY
  run_dispatch "$SHA"
  check '[ "$RC" = 3 ]' "exit 3 (got $RC)"
  check '[ "$(dispatches)" = 0 ]' "no workflow_dispatch sent (got $(dispatches))"
  check 'grep -q "already running for" "$TMPROOT/err"' "names the in-flight build"
  teardown
}

t5_completed_build_does_not_refuse() {
  CURRENT=t5; note "t5: a FINISHED build for this commit is not in flight → dispatch"
  setup
  "$PY" - "$STATE/runs.json" "$SHA" <<'PY'
import json, sys
path, sha = sys.argv[1], sys.argv[2]
json.dump({"workflow_runs": [
    {"head_sha": sha, "path": ".github/workflows/release-linux-selfhosted.yml",
     "status": "completed", "conclusion": "failure"},
]}, open(path, "w"))
PY
  run_dispatch "$SHA"
  check '[ "$RC" = 0 ]' "exit 0 (a failed build is a human problem, not a race)"
  check '[ "$(dispatches)" = 3 ]' "3 dispatches (got $(dispatches))"
  teardown
}

t6_other_commit_does_not_refuse() {
  CURRENT=t6; note "t6: another commit's build in flight → still dispatch ours"
  setup
  "$PY" - "$STATE/runs.json" <<'PY'
import json, sys
json.dump({"workflow_runs": [
    {"head_sha": "0" * 40, "path": ".github/workflows/release-linux-selfhosted.yml",
     "status": "in_progress", "conclusion": None},
]}, open(sys.argv[1], "w"))
PY
  run_dispatch "$SHA"
  check '[ "$RC" = 0 ]' "exit 0"
  check '[ "$(dispatches)" = 3 ]' "3 dispatches (got $(dispatches))"
  teardown
}

t7_force_overrides() {
  CURRENT=t7; note "t7: --force overrides every check (the escape hatch works)"
  setup
  mkdir -p "$TMPROOT/lock"
  ( exec 9>"$TMPROOT/lock/release.lock.flock"; flock -n 9 || exit 9; sleep 20 ) &
  HOLDER=$!
  sleep 0.3
  run_dispatch --force "$SHA"
  check '[ "$RC" = 0 ]' "exit 0 (got $RC)"
  check '[ "$(dispatches)" = 3 ]' "3 dispatches despite the held lock (got $(dispatches))"
  check 'grep -q -- "--force: skipping" "$TMPROOT/out"' "says it skipped the guard"
  kill "$HOLDER" 2>/dev/null; wait "$HOLDER" 2>/dev/null
  teardown
}

t8_bad_sha_still_refuses_first() {
  CURRENT=t8; note "t8: the sha validation still comes first"
  setup
  run_dispatch "deadbeef"
  check '[ "$RC" = 2 ]' "exit 2 for a short sha (got $RC)"
  check '[ "$(dispatches)" = 0 ]' "no dispatch"
  teardown
}

# ─────────────────────────────────────────────────────────────────── main ──
want=("$@")
run() {
  if [ "${#want[@]}" -gt 0 ]; then
    local hit=0 w
    for w in "${want[@]}"; do case "$1" in "$w"*) hit=1 ;; esac; done
    [ "$hit" = 1 ] || return 0
  fi
  "$1"
}

printf '== dispatch-release in-flight guard (W145)\n'
run t1_clean_dispatch_proceeds
run t2_held_flock_refuses
run t3_running_cut_refuses
run t4_inflight_build_refuses
run t5_completed_build_does_not_refuse
run t6_other_commit_does_not_refuse
run t7_force_overrides
run t8_bad_sha_still_refuses_first

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'failed: %s\n' "${FAILED[@]}"
  exit 1
fi
