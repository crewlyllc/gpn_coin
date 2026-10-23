#!/usr/bin/env bash
# deploy-bin.sh — the ONE lane for building and installing GPN binaries.
#
# Why this exists (2026-10-23, queue item #92):
#   Two agents ran `cargo build --release` from the same target/release tree at
#   the same time and installed at the same time. A parallel build replaced
#   ~/.local/bin/gpn_llm_provider with a binary that did NOT contain the feature
#   under test. The test "passed" against stale code; it was only caught by
#   md5sum + `strings | grep gpu-lock`, by hand.
#
# Rules enforced here:
#   1. Only one deploy may run per host (exclusive flock on ~/.gpn/deploy.lock).
#      A second invocation BLOCKS and then reports that it waited. It never races.
#   2. The artifact must be newer than every tracked source file in the crate.
#   3. The installed binary must contain every --expect-marker substring, else
#      the deploy ABORTS BEFORE install (old binary stays in place).
#   4. Install is a temp-file + rename on the same filesystem (atomic).
#   5. The previous binary is backed up as <name>.bak-<UTC timestamp>.
#   6. A fingerprint (sha256 installed vs built, size) is printed at the end.
#
# Usage:
#   scripts/deploy-bin.sh --bin gpn_llm_provider [--bin gpn_scheduler ...] \
#                         [--unit gpn-provider --unit gpn-scheduler] \
#                         [--expect-marker gpu-lock] \
#                         [--crate-dir gpn-agent] [--target-dir ~/.local/bin] \
#                         [--lock-file ~/.gpn/deploy.lock] [--restart-timeout 30] \
#                         [--no-restart] [--dry-run]
#
#   # test path: install a prebuilt throwaway artifact, no cargo, no systemd
#   scripts/deploy-bin.sh --bin faked --no-build --artifact /tmp/a.bin \
#                         --target-dir /tmp/dbt/bin --no-restart
#
# Exit codes: 0 ok · 2 usage · 3 build failed · 4 stale artifact ·
#             5 missing marker · 6 install failed · 7 unit did not come up
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -t 1 ]; then C_RED=$'\033[31m'; C_YEL=$'\033[33m'; C_BLD=$'\033[1m'; C_OFF=$'\033[0m'
else C_RED=""; C_YEL=""; C_BLD=""; C_OFF=""; fi

BINS=()
UNITS=()
MARKERS=()
CRATE_DIR="$REPO/gpn-agent"
TARGET_DIR="$HOME/.local/bin"
LOCK_FILE="$HOME/.gpn/deploy.lock"
RESTART_TIMEOUT=30
DO_BUILD=1
DO_RESTART=1
DRY_RUN=0
FORCE_FRESH=0
ARTIFACT=""
DEBUG_HOLD=0

usage() { sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit "${1:-2}"; }
die() { local code="$1"; shift; printf '%sFATAL:%s %s\n' "$C_RED" "$C_OFF" "$*" >&2; exit "$code"; }
say() { printf '\n%s== %s ==%s\n' "$C_BLD" "$*" "$C_OFF"; }
info() { printf '   %s\n' "$*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --bin)            BINS+=("${2:?--bin needs a name}"); shift 2 ;;
    --unit)           UNITS+=("${2:?--unit needs a name}"); shift 2 ;;
    --expect-marker)  MARKERS+=("${2:?--expect-marker needs a substring}"); shift 2 ;;
    --crate-dir)      CRATE_DIR="${2:?}"; shift 2 ;;
    --target-dir)     TARGET_DIR="${2:?}"; shift 2 ;;
    --lock-file)      LOCK_FILE="${2:?}"; shift 2 ;;
    --restart-timeout) RESTART_TIMEOUT="${2:?}"; shift 2 ;;
    --artifact)       ARTIFACT="${2:?}"; DO_BUILD=0; shift 2 ;;
    --no-build)       DO_BUILD=0; shift ;;
    --no-restart)     DO_RESTART=0; shift ;;
    --dry-run)        DRY_RUN=1; shift ;;
    --force-freshness-check) FORCE_FRESH=1; shift ;;   # test-only: run the mtime gate with --no-build
    --debug-hold-seconds) DEBUG_HOLD="${2:?}"; shift 2 ;;   # test-only: hold the lock
    -h|--help)        usage 0 ;;
    *) die 2 "unknown argument: $1 (try --help)" ;;
  esac
done

[ "${#BINS[@]}" -gt 0 ] || die 2 "need at least one --bin"
[ -d "$CRATE_DIR" ] || die 2 "crate dir not found: $CRATE_DIR"
command -v flock >/dev/null || die 2 "flock is required"
[ "$DO_RESTART" = 1 ] && [ "${#UNITS[@]}" -eq 0 ] && die 2 "--unit is required unless --no-restart is given"
[ "$DO_BUILD" = 0 ] && [ "${#BINS[@]}" -ne 1 ] && die 2 "--no-build/--artifact installs exactly one binary"

STAMP="$(date -u +%Y%m%d-%H%M%S)"
mkdir -p "$TARGET_DIR" "$(dirname "$LOCK_FILE")"

# ---------------------------------------------------------------- lane lock --
# Held on fd 9 for the whole build+install+restart. -n first so we can report
# contention instead of silently queueing.
exec 9>"$LOCK_FILE"
LOCK_T0="$SECONDS"
if ! flock -n 9; then
  HOLDER="$(cat "$LOCK_FILE" 2>/dev/null | head -1)"
  printf '%s[lane]%s another deploy holds %s%s — WAITING (this deploy will be serialised, not raced)\n' \
    "$C_YEL" "$C_OFF" "$LOCK_FILE" "${HOLDER:+ (holder: $HOLDER)}"
  flock 9
  WAITED=$((SECONDS - LOCK_T0))
  printf '%s[lane]%s acquired after waiting %ss\n' "$C_YEL" "$C_OFF" "$WAITED"
  WAITED_REPORT="yes (${WAITED}s)"
else
  WAITED_REPORT="no"
fi
printf 'pid=%s started=%s bins=%s\n' "$$" "$(date -u +%FT%TZ)" "${BINS[*]}" >"$LOCK_FILE" || true

cleanup() { :; }
trap cleanup EXIT

if [ "$DEBUG_HOLD" -gt 0 ]; then
  info "[test] holding lane lock for ${DEBUG_HOLD}s"
  sleep "$DEBUG_HOLD"
fi

say "deploy lane acquired (waited: $WAITED_REPORT)"
info "crate:   $CRATE_DIR"
info "target:  $TARGET_DIR"
info "bins:    ${BINS[*]}"

# -------------------------------------------------------------------- build --
BUILT=()
BUILT_NAMES=()
for bin in "${BINS[@]}"; do
  if [ "$DO_BUILD" = 1 ]; then
    [ -f "$CRATE_DIR/src/bin/$bin.rs" ] || [ -f "$CRATE_DIR/src/main.rs" ] \
      || die 3 "no source for bin '$bin' under $CRATE_DIR/src"
    say "building $bin"
    if ! ( cd "$CRATE_DIR" && cargo build --release --bin "$bin" ); then
      die 3 "cargo build --release --bin $bin FAILED — nothing installed"
    fi
    src="$CRATE_DIR/target/release/$bin"
  else
    src="$ARTIFACT"
    [ -f "$src" ] || die 3 "artifact not found: $src"
  fi
  [ -x "$src" ] || die 3 "artifact is not executable: $src"
  BUILT+=("$src")
  if [ "$DO_BUILD" = 1 ]; then BUILT_NAMES+=("$(basename "$src")"); else BUILT_NAMES+=("$bin"); fi
done

# ------------------------------------------------------- freshness (build) ---
# Cargo can report "Finished" while the artifact on disk is from an older build
# (parallel build raced us, or the crate was already up to date). Compare mtimes
# against every tracked source file in the crate.
if [ "$DO_BUILD" = 1 ] || [ "$FORCE_FRESH" = 1 ]; then
  say "verifying artifact freshness against tracked sources"
  newest=0; newest_f=""
  while IFS= read -r f; do
    case "$f" in *.rs|*.toml) ;; *) continue ;; esac
    [ -f "$CRATE_DIR/$f" ] || continue
    m=$(stat -c %Y -- "$CRATE_DIR/$f")
    if [ "$m" -gt "$newest" ]; then newest=$m; newest_f=$f; fi
  done < <(cd "$CRATE_DIR" && git ls-files . 2>/dev/null)
  for src in "${BUILT[@]}"; do
    am=$(stat -c %Y -- "$src")
    if [ "$newest" -gt 0 ] && [ "$am" -lt "$newest" ]; then
      die 4 "STALE ARTIFACT: $src (mtime $(date -d @$am -u +%FT%TZ)) is older than source $newest_f ($(date -d @$newest -u +%FT%TZ)). Refusing to install — rebuild."
    fi
    info "$(basename "$src"): mtime ok (artifact >= newest source $newest_f)"
  done
else
  say "skipping freshness check (--no-build)"
fi

# ------------------------------------------------------- marker assertions ---
# This is the check whose absence made a working fix look broken twice.
if [ "${#MARKERS[@]}" -gt 0 ]; then
  say "asserting markers in BUILT artifact (before install)"
  for src in "${BUILT[@]}"; do
    for m in "${MARKERS[@]}"; do
      if strings -a -- "$src" | grep -qF -- "$m"; then
        info "$(basename "$src"): marker '$m' present"
      else
        die 5 "MARKER MISSING: '$m' not found in $src. NOT installed — this is exactly the stale/wrong-build failure. Re-run the build and check the source actually contains it (grep -rn '$m' $CRATE_DIR/src)."
      fi
    done
  done
fi

if [ "$DRY_RUN" = 1 ]; then
  say "dry run — would install:"
  for i in "${!BUILT[@]}"; do info "${BUILT[$i]} -> $TARGET_DIR/${BUILT_NAMES[$i]}"; done
  exit 0
fi

# ------------------------------------------------------------------ install --
say "installing"
installed_sha=""
for i in "${!BUILT[@]}"; do
  src="${BUILT[$i]}"; name="${BUILT_NAMES[$i]}"
  dst="$TARGET_DIR/$name"
  built_sha=$(sha256sum -- "$src" | cut -d' ' -f1)

  if [ -f "$dst" ]; then
    bak="$dst.bak-$STAMP"
    cp -p -- "$dst" "$bak"
    info "backup: $bak ($(sha256sum -- "$bak" | cut -d' ' -f1 | cut -c1-16)…)"
  else
    info "no existing $dst — no backup needed (first install)"
  fi

  tmp="$TARGET_DIR/.$name.tmp.$$"
  cp -- "$src" "$tmp"
  chmod 0755 -- "$tmp"
  mv -f -- "$tmp" "$dst"          # same filesystem => atomic rename
  sync -f "$dst" 2>/dev/null || true

  ins_sha=$(sha256sum -- "$dst" | cut -d' ' -f1)
  [ "$ins_sha" = "$built_sha" ] || die 6 "post-install sha256 MISMATCH for $name (built $built_sha != installed $ins_sha)"
  info "$name installed: sha256=$(echo "$ins_sha" | cut -c1-16)… size=$(stat -c %s -- "$dst") bytes"
  installed_sha="$ins_sha"
done

# ------------------------------------------------------------------ restart --
if [ "$DO_RESTART" = 1 ]; then
  say "restarting units"
  for u in "${UNITS[@]}"; do
    systemctl --user restart "$u" || die 7 "systemctl --user restart $u failed"
    ok=0
    for _ in $(seq 1 "$RESTART_TIMEOUT"); do
      if [ "$(systemctl --user is-active "$u" 2>/dev/null)" = "active" ]; then ok=1; break; fi
      sleep 1
    done
    if [ "$ok" != 1 ]; then
      systemctl --user status "$u" --no-pager -n 15 || true
      die 7 "$u did not reach active within ${RESTART_TIMEOUT}s"
    fi
    info "$u: active (main pid $(systemctl --user show -p MainPID --value "$u"))"
  done
else
  say "skipping service restart (--no-restart)"
fi

# -------------------------------------------------------------- fingerprint --
say "fingerprint"
for i in "${!BUILT[@]}"; do
  src="${BUILT[$i]}"; name="${BUILT_NAMES[$i]}"
  dst="$TARGET_DIR/$name"
  printf '   %-28s built  %s  %8s B\n' "$name" "$(sha256sum -- "$src" | cut -c1-32)" "$(stat -c %s -- "$src")"
  printf '   %-28s instld %s  %8s B\n' "" "$(sha256sum -- "$dst" | cut -c1-32)" "$(stat -c %s -- "$dst")"
done
[ "${#MARKERS[@]}" -gt 0 ] && info "markers verified: ${MARKERS[*]}"
info "lane waited: $WAITED_REPORT"

cat <<EOF

Deployed: ${BINS[*]} -> $TARGET_DIR
If the running service still looks wrong, verify the LIVE process, not the file:
  systemctl --user show -p MainPID --value gpn-provider | xargs -r ls -l /proc/{}/exe
  strings -a /proc/\$(systemctl --user show -p MainPID --value gpn-provider)/exe | grep -F '<marker>'
EOF
