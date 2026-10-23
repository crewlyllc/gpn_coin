#!/usr/bin/env bash
# commit-paths.sh — commit ONLY the paths you name. Never `git add -A`.
#
# Why this exists (2026-10-23, queue item #92):
#   Another session's files were swept into an unrelated commit by `git add -A`
#   while several agents shared one working tree.
#
# What it does:
#   1. Refuses `.`, `-A`, `--all`, or any glob that would stage the whole tree.
#   2. Warns (loudly) about every dirty path in the working tree that you did NOT
#      name — including files modified after the commit started, or owned by a
#      different user (i.e. clearly another session's work).
#   3. Stages exactly `git add -- <paths>`.
#   4. Re-reads the staged set and ABORTS if anything is staged that you did not
#      name (e.g. leftovers from an earlier `git add`), telling you how to fix it.
#   5. Commits.
#
#   6. Runs the same `cargo fmt --all -- --check` gate CI runs, for every crate
#      you are committing into, and ABORTS if one of YOUR paths is unformatted
#      (another session's unformatted file only warns). Why: 2026-10-23, item
#      #98 — #93/#94/#96/#97 were pushed without `cargo fmt`, so CI's first
#      gate (`rustfmt (check, agent)`) went red on main and no test ever ran.
#
# Usage:
#   scripts/commit-paths.sh -m "message" <path> [<path> ...]
#   scripts/commit-paths.sh -m "message" --dry-run <path> ...
#   scripts/commit-paths.sh -m "message" --skip-fmt <path> ...
#
# Optional: install as a pre-commit hook (defence in depth; the hook only sees
# the index, so it cannot know your intent — it just refuses bulk-looking sets):
#   ln -sf ../../scripts/commit-paths.sh .git/hooks/pre-commit   # not recommended verbatim
# Prefer calling this script directly in agent/CI flows.
set -euo pipefail

# Operate on the git repo of the CALLER's cwd, not this script's own checkout.
REPO="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$REPO" ] || { echo "REFUSED: not inside a git working tree." >&2; exit 2; }
cd "$REPO"

MSG=""
DRY=0
SKIP_FMT=0
PATHS=()
START_EPOCH="$(date +%s)"

while [ $# -gt 0 ]; do
  case "$1" in
    -m|--message) MSG="${2:?$1 needs a message}"; shift 2 ;;
    --dry-run)    DRY=1; shift ;;
    --skip-fmt)   SKIP_FMT=1; shift ;;
    -h|--help)    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -A|--all|.)   echo "REFUSED: '$1' stages the whole tree. Name the paths explicitly." >&2; exit 2 ;;
    -*)           echo "REFUSED: unknown option '$1'" >&2; exit 2 ;;
    *)            PATHS+=("$1"); shift ;;
  esac
done

[ "${#PATHS[@]}" -gt 0 ] || { echo "REFUSED: no paths given. Name exactly what you are committing." >&2; exit 2; }
[ -n "$MSG" ] || { echo "REFUSED: -m \"message\" is required." >&2; exit 2; }

# ------------------------------------------------------------- path sanity --
for p in "${PATHS[@]}"; do
  case "$p" in
    *'*'*|*'?'*|*'['*) echo "REFUSED: glob '$p' — pass literal paths." >&2; exit 2 ;;
  esac
  if [ ! -e "$p" ]; then
    # deleted files are legitimate; require the path to at least be known to git
    git ls-files --error-unmatch -- "$p" >/dev/null 2>&1 \
      || { echo "REFUSED: '$p' is neither on disk nor tracked." >&2; exit 2; }
  fi
done

# -------------------------------------------------- other-session warnings --
warn_other_sessions() {
  local me uid_mine dirty
  me="$(id -un)"; uid_mine="$(id -u)"
  dirty=$(git status --porcelain --untracked-files=all 2>/dev/null | sed 's/^...//' || true)
  [ -n "$dirty" ] || return 0

  local warned=0
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    local named=0 p
    for p in "${PATHS[@]}"; do [ "$f" = "$p" ] && { named=1; break; }; done
    [ "$named" = 1 ] && continue
    [ -e "$f" ] || continue
    local mt owner
    mt=$(stat -c %Y -- "$f"); owner=$(stat -c %U -- "$f")
    local why=""
    if [ "$mt" -gt "$START_EPOCH" ]; then why="$why (modified after this commit started)"; fi
    if [ "$owner" != "$me" ]; then why="$why (owned by $owner, not $me)"; fi
    if [ $warned = 0 ]; then
      printf '\033[33mWARNING: working tree has changes NOT in this commit:\033[0m\n' >&2
      warned=1
    fi
    printf '  - %s%s\n' "$f" "$why" >&2
  done <<< "$dirty"
  if [ $warned = 1 ]; then
    printf '\033[33mThese are probably another session'"'"'s work. They are NOT being staged.\033[0m\n' >&2
  fi
}

warn_other_sessions

# ------------------------------------------------------------- fmt gate ----
# CI's very first gate is `cargo fmt --all -- --check` per crate. A commit that
# fails it turns main red before a single test runs (item #98). Check only the
# crates you are committing into, and only block on YOUR files.
fmt_gate() {
  [ "$SKIP_FMT" = 1 ] && { echo "(fmt gate skipped by --skip-fmt)" >&2; return 0; }
  local crates=() c crate
  for p in "${PATHS[@]}"; do
    for c in $(find . -maxdepth 2 -name Cargo.toml -printf '%h\n' 2>/dev/null); do
      c=${c#./}   # `find .` yields ./gpn-agent; named paths are repo-relative
      case "$p" in
        "$c"/*) case " ${crates[*]-} " in *" $c "*) ;; *) crates+=("$c") ;; esac ;;
      esac
    done
  done
  [ "${#crates[@]}" -gt 0 ] || return 0

  local mine="" theirs="" out offenders f named
  for crate in "${crates[@]}"; do
    out=$( cd "$crate" && cargo fmt --all -- --check 2>&1 || true )
    offenders=$(printf '%s\n' "$out" | sed -n 's/^Diff in \(.*\):[0-9]*:$/\1/p' | sort -u)
    [ -n "$offenders" ] || continue
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      f=${f#./}
      f=${f#"$REPO"/}   # rustfmt prints absolute paths; named paths are repo-relative
      named=0
      for p in "${PATHS[@]}"; do [ "$f" = "$p" ] && { named=1; break; }; done
      if [ "$named" = 1 ]; then mine="$mine $f"; else theirs="$theirs $f"; fi
    done <<< "$offenders"
  done

  if [ -n "$theirs" ]; then
    printf '\033[33mWARNING: unformatted files NOT in this commit (another session?):%s\033[0m\n' "$theirs" >&2
    printf '  leave them alone; they will fail CI when committed.\n' >&2
  fi
  if [ -n "$mine" ]; then
    echo "REFUSED: these paths you are committing are not rustfmt-clean:$mine" >&2
    echo "CI runs 'cargo fmt --all -- --check' first, so this would turn main red." >&2
    echo "Fix: cd gpn-agent && cargo fmt --all   (then re-run this script)" >&2
    exit 4
  fi
}

fmt_gate

# --------------------------------------------------------------- staging ----
echo "staging ${#PATHS[@]} path(s): ${PATHS[*]}"
git add -- "${PATHS[@]}"

# ------------------------------------------------- verify staged set == args --
staged=$(git diff --cached --name-only)
if [ -z "$staged" ]; then
  echo "Nothing staged — nothing to commit." >&2; exit 1
fi
bad=""
while IFS= read -r f; do
  [ -n "$f" ] || continue
  named=0
  for p in "${PATHS[@]}"; do [ "$f" = "$p" ] && { named=1; break; }; done
  [ "$named" = 0 ] && bad="$bad $f"
done <<< "$staged"
if [ -n "$bad" ]; then
  echo "REFUSED: the index contains paths you did not name:$bad" >&2
  echo "That is the 'git add -A swept in another session' failure." >&2
  echo "Fix with: git reset -- ${bad}   (or unstage everything: git reset)" >&2
  exit 3
fi

echo "will commit:"
git diff --cached --stat

if [ "$DRY" = 1 ]; then
  echo "(dry run — not committing)"
  git reset -q -- "${PATHS[@]}" 2>/dev/null || true
  exit 0
fi

git commit -m "$MSG" -- "${PATHS[@]}"
echo "committed: $(git rev-parse --short HEAD)"
