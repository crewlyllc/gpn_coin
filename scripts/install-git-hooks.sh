#!/usr/bin/env bash
# install-git-hooks.sh — install the repo's hooks into .git/hooks (#204)
#
# Right now that is one hook: scripts/hooks/pre-commit, which refuses a commit
# while a release holds $HOME/.gpn/release-in-flight. Run this once per clone;
# it is idempotent. A machine without the hook is a machine where the release
# lock is only prose — say so rather than relying on discipline.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null || git -C "$PWD" rev-parse --show-toplevel 2>/dev/null)"
if [ -z "$TOP" ]; then
  printf 'install-git-hooks: not inside a git repository\n' >&2
  exit 1
fi

# A worktree has a .git FILE pointing at the real dir; ask git for the hooks path.
HOOKS_DIR="$(git -C "$TOP" rev-parse --git-path hooks)"
case "$HOOKS_DIR" in
  /*) ;;
  *) HOOKS_DIR="$TOP/$HOOKS_DIR" ;;
esac
mkdir -p "$HOOKS_DIR" || exit 1

installed=0
for src in "$HERE"/hooks/*; do
  [ -f "$src" ] || continue
  name="$(basename "$src")"
  dest="$HOOKS_DIR/$name"
  if [ -f "$dest" ] && ! cmp -s "$src" "$dest"; then
    printf 'install-git-hooks: replacing existing %s (was not ours)\n' "$dest"
  fi
  cp "$src" "$dest" || exit 1
  chmod +x "$dest" || exit 1
  printf 'installed %s\n' "$dest"
  installed=$((installed+1))
done

[ "$installed" -gt 0 ] || { printf 'install-git-hooks: nothing to install\n' >&2; exit 1; }
