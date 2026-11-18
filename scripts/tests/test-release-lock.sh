#!/usr/bin/env bash
# test-release-lock.sh — self-test for the mechanical release lock (#204)
#
# The rule "while a release checklist is open, nothing else may be committed" has
# existed in prose since 0.3.55 and has been bypassed three times in one week (the
# cancelled CI runs on 2026-11-16 were the driver's own doc commits). Prose is not a
# mechanism, so this test asserts the mechanism end to end, against a throwaway git
# repository — including a real `git commit`, not a hand-invocation of the hook.
#
# Cases:
#   files               release-lock.sh / hooks/pre-commit / install-git-hooks.sh exist, exec
#   take                take 0.3.59 creates $HOME/.gpn/release-in-flight naming the version
#   take-twice          a second take without --force refuses (exit 3) and names the holder
#   release             release removes the file
#   commit-clean        with no lock, `git commit` succeeds
#   commit-blocked      with the lock, `git commit` fails and names version + --no-verify
#   install             install-git-hooks.sh installs a hook that blocks the same commit
#   release-cut-calls   release-cut.sh actually takes and releases the lock (call site)
#
# Hermetic: $HOME is a temp dir, the repo is a temp git repo, no network.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$HERE/.."
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   %s\n' "$*"; }
bad() { fail=$((fail+1)); printf '  FAIL %s\n' "$*"; }
check() { if [ "$2" = 0 ]; then ok "$1"; else bad "$1"; fi; }
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export HOME="$TMP/home"; mkdir -p "$HOME"
LOCK="$HOME/.gpn/release-in-flight"

R="$TMP/repo"; mkdir -p "$R/scripts/hooks"
cp "$ROOT/release-lock.sh" "$R/scripts/" 2>/dev/null
cp "$ROOT/hooks/pre-commit" "$R/scripts/hooks/" 2>/dev/null
cp "$ROOT/install-git-hooks.sh" "$R/scripts/" 2>/dev/null
cp "$ROOT/release-cut.sh" "$R/scripts/" 2>/dev/null
chmod +x "$R/scripts/"*.sh "$R/scripts/hooks/pre-commit" 2>/dev/null
git -C "$R" init -q
git -C "$R" config user.email t@example.com
git -C "$R" config user.name Test

# ── files ────────────────────────────────────────────────────────────────
missing=""
for f in "$ROOT/release-lock.sh" "$ROOT/hooks/pre-commit" "$ROOT/install-git-hooks.sh"; do
  [ -x "$f" ] || missing="$missing $f"
done
check "the lock tool, the hook and its installer exist and are executable" \
  "$([ -z "$missing" ] && echo 0 || echo 1)"
[ -n "$missing" ] && printf '       missing:%s\n' "$missing"

# ── take / take-twice / release ──────────────────────────────────────────
"$R/scripts/release-lock.sh" take 0.3.59 >/dev/null 2>&1
rc=$?
check "release-lock.sh take 0.3.59 exits 0" "$([ "$rc" = 0 ] && echo 0 || echo 1)"
check "the lock file exists at \$HOME/.gpn/release-in-flight" \
  "$([ -f "$LOCK" ] && echo 0 || echo 1)"
check "the lock names the version it holds" \
  "$(contains "$(cat "$LOCK" 2>/dev/null)" '0.3.59' && echo 0 || echo 1)"

out="$("$R/scripts/release-lock.sh" take 0.3.60 2>&1)"; rc=$?
check "a second take without --force refuses with exit 3" "$([ "$rc" = 3 ] && echo 0 || echo 1)"
check "the refusal names the version already holding it" \
  "$(contains "$out" '0.3.59' && echo 0 || echo 1)"

# Arming: the lock is only a mechanism where the hook exists, so `take` installs it.
check "take arms .git/hooks/pre-commit (default-on, not a setup step)" \
  "$([ -x "$R/.git/hooks/pre-commit" ] && echo 0 || echo 1)"
check "the armed hook is the repo's own hook" \
  "$(cmp -s "$ROOT/hooks/pre-commit" "$R/.git/hooks/pre-commit" && echo 0 || echo 1)"

"$R/scripts/release-lock.sh" release 0.3.59 >/dev/null 2>&1
check "release 0.3.59 removes the lock" "$([ ! -f "$LOCK" ] && echo 0 || echo 1)"

# ── the hook, invoked by real git ────────────────────────────────────────
printf 'a\n' > "$R/a.txt"; git -C "$R" add a.txt >/dev/null 2>&1

out="$(git -C "$R" commit -m 'clean commit' 2>&1)"; rc=$?
check "with no lock, git commit succeeds" "$([ "$rc" = 0 ] && echo 0 || echo 1)"

printf 'b\n' > "$R/b.txt"; git -C "$R" add b.txt >/dev/null 2>&1
"$R/scripts/release-lock.sh" take 0.3.59 >/dev/null 2>&1
before="$(git -C "$R" rev-parse HEAD 2>/dev/null)"
out="$(git -C "$R" commit -m 'commit during a release' 2>&1)"; rc=$?
after="$(git -C "$R" rev-parse HEAD 2>/dev/null)"
check "with the lock held, git commit FAILS" "$([ "$rc" != 0 ] && echo 0 || echo 1)"
check "the refusal message names the version in flight" \
  "$(contains "$out" '0.3.59' && echo 0 || echo 1)"
check "the refusal message names the git-level escape (--no-verify)" \
  "$(contains "$out" 'no-verify' && echo 0 || echo 1)"
check "no commit was created" "$([ "$before" = "$after" ] && echo 0 || echo 1)"

# ── installer (explicit path still works, and re-arms after a wipe) ──────
rm -f "$R/.git/hooks/pre-commit"
( cd "$R" && "$R/scripts/install-git-hooks.sh" >/dev/null 2>&1 )
check "install-git-hooks.sh installs .git/hooks/pre-commit" \
  "$([ -x "$R/.git/hooks/pre-commit" ] && echo 0 || echo 1)"
out="$(git -C "$R" commit -m 'still blocked' 2>&1)"; rc=$?
check "the installed hook blocks the same commit" "$([ "$rc" != 0 ] && echo 0 || echo 1)"
"$R/scripts/release-lock.sh" release 0.3.59 >/dev/null 2>&1
git -C "$R" commit -m 'after release' >/dev/null 2>&1
check "after release the same commit succeeds" "$([ "$?" = 0 ] && echo 0 || echo 1)"

# ── call site: release-cut must actually take it ─────────────────────────
rc=0
grep -q 'release-lock.sh take' "$ROOT/release-cut.sh" 2>/dev/null || rc=1
check "release-cut.sh takes the lock (call site, not a definition)" "$rc"
rc=0
grep -q 'release-lock.sh release' "$ROOT/release-cut.sh" 2>/dev/null || rc=1
check "release-cut.sh releases the lock when the manifest is live" "$rc"

printf '\nPASS=%d FAIL=%d\n' "$pass" "$fail"
[ "$fail" = 0 ] || exit 1
