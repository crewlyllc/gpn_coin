#!/usr/bin/env bash
# release-lock.sh — the mechanical "a release owns the repo" lock (#204)
#
# Why this exists: the rule "while a release checklist is open, nothing else may
# be committed" lived in prose (docs/UPDATE-CHECKLIST.md §0) and was bypassed
# three times in one week. Every extra push cancels a queued CI run for the
# release in flight (three of the API's `cancelled` runs on 2026-11-16 were the
# driver's own doc commits), so the rule needs to be a file, not a promise.
#
#   release-lock.sh take <version> [--force]   take the lock (exit 3 if held)
#   release-lock.sh release <version>          release it (manifest published)
#   release-lock.sh status                     is a release in flight?
#   release-lock.sh path                       print the lock path
#
# The lock lives at $HOME/.gpn/release-in-flight and is read by
# scripts/hooks/pre-commit. `take` installs that hook into the repo it is run
# from (idempotent — scripts/install-git-hooks.sh), so the guard is armed by the
# act of locking rather than by a setup step somebody has to remember. A commit
# refuses while a release is open. Genuine emergencies have exactly one
# documented escape: `git commit --no-verify`, which the refusal names.
set -uo pipefail

LOCK="${GPN_RELEASE_LOCK:-$HOME/.gpn/release-in-flight}"

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
}

# A lock only binds on a clone whose .git/hooks/pre-commit exists, and "run the
# installer once per clone" is precisely the remembered step that gets skipped
# (#204: the rule was prose for a week). So taking the lock arms the hook: the
# guard is ON by default at the moment it matters, not behind a setup step.
arm_hook() {
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  [ -x "$here/install-git-hooks.sh" ] || return 0
  top="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null)" || top=""
  [ -n "$top" ] || return 0
  if ! "$here/install-git-hooks.sh" >/dev/null 2>&1; then
    printf 'release-lock: WARNING: lock taken but the pre-commit hook could not be armed\n' >&2
    printf '               a commit in %s is NOT blocked — run scripts/install-git-hooks.sh\n' "$top" >&2
  fi
  return 0
}

cmd="${1:-}"; shift || true

case "$cmd" in
  path)
    printf '%s\n' "$LOCK"
    ;;

  status)
    if [ -f "$LOCK" ]; then
      printf 'release in flight: %s\n' "$(head -n1 "$LOCK")"
      sed -n '2,8p' "$LOCK" | sed 's/^/  /'
      exit 0
    fi
    printf 'no release in flight\n'
    ;;

  take)
    version="${1:-}"; force="${2:-}"
    if [ -z "$version" ]; then
      printf 'release-lock: take needs a version\n' >&2
      exit 2
    fi
    if [ -f "$LOCK" ] && [ "$force" != "--force" ]; then
      holder="$(head -n1 "$LOCK")"
      printf 'release-lock: repo is locked — %s\n' "$holder" >&2
      printf '  held by: %s\n' "$(sed -n '2p' "$LOCK" | sed 's/^holder: //')" >&2
      printf '  since:   %s\n' "$(sed -n '3p' "$LOCK" | sed 's/^since:  //')" >&2
      printf 'release it with: scripts/release-lock.sh release %s\n' "${holder%% *}" >&2
      printf 'or override deliberately: scripts/release-lock.sh take %s --force\n' "$version" >&2
      exit 3
    fi
    mkdir -p "$(dirname "$LOCK")" || exit 4
    tmp="$LOCK.tmp.$$"
    {
      printf '%s\n' "$version"
      printf 'holder: %s@%s pid %s\n' "${USER:-$(id -un)}" "$(hostname)" "$$"
      printf 'since:  %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    } > "$tmp" || exit 4
    mv -f "$tmp" "$LOCK" || exit 4
    arm_hook
    printf 'release-lock: %s taken (%s)\n' "$LOCK" "$version"
    ;;

  release)
    version="${1:-}"
    if [ ! -f "$LOCK" ]; then
      printf 'release-lock: no lock held (nothing to release)\n'
      exit 0
    fi
    holder="$(head -n1 "$LOCK")"
    if [ -n "$version" ] && [ "$holder" != "$version" ]; then
      printf 'release-lock: lock is held by %s, not %s — refusing to release it\n' \
        "$holder" "$version" >&2
      exit 3
    fi
    rm -f "$LOCK"
    printf 'release-lock: released (%s)\n' "$holder"
    ;;

  ""|-h|--help|help)
    usage
    exit 0
    ;;

  *)
    printf 'release-lock: unknown command %s\n' "$cmd" >&2
    usage >&2
    exit 2
    ;;
esac
