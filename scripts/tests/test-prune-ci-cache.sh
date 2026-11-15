#!/usr/bin/env bash
# test-prune-ci-cache.sh — the disk guard must prune when the disk is FULL.
#
# The defect being pinned (found 2026-11-14): `df --output=pcent` prints the
# USED percentage, the script stored it as `free_pct`, subtracted to get
# `used_pct` (actually free), and then refused to prune when that was *below*
# the floor. Net effect: it pruned when there was plenty of space and declined
# exactly when the disk was full — a no-op guard, which is the one failure mode
# that matters (a full root FS halted the chain on 2026-10-19).
#
# Both directions are asserted, so a future "fix" that flips it back fails:
#   * free < floor  => the debug trees ARE removed
#   * free >= floor => the debug trees are LEFT ALONE
#
# `df` and `pgrep` are stubbed on PATH; nothing real is measured or deleted.
#
#   scripts/tests/test-prune-ci-cache.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/prune-ci-cache.sh"

PASS=0
FAIL=0
FAILED=()
CURRENT=""

note() { printf '   %s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '   \033[32mok\033[0m %s\n' "$*"; }
bad()  { printf '   \033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); FAILED+=("$CURRENT: $*"); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

TMPROOT="$(mktemp -d)"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

# ─────────────────────────────────────────────────────────────── stubs ─────
# FAKE_USED_PCT is what `df --output=pcent` reports: the USED percentage,
# exactly like the real tool. The whole bug lives in how the script reads it,
# so the stub must not be "helpful" and report free space instead.
mkdir -p "$TMPROOT/bin"
cat > "$TMPROOT/bin/df" <<'EOF'
#!/usr/bin/env bash
pct="${FAKE_USED_PCT:-0}"
printf 'Use%%\n%s%%\n' "$pct"
EOF
cat > "$TMPROOT/bin/pgrep" <<'EOF'
#!/usr/bin/env bash
exit 1           # no cargo/rustc running
EOF
chmod +x "$TMPROOT/bin/df" "$TMPROOT/bin/pgrep"

# A cache tree with one prunable debug dir and one prunable incremental dir.
make_cache() {
  local cache="$1"
  rm -rf "$cache"
  mkdir -p "$cache/gpn-agent-target/debug/deps" "$cache/gpn-agent-target/incremental"
  printf 'x' > "$cache/gpn-agent-target/debug/deps/blob.o"
  printf 'x' > "$cache/gpn-agent-target/incremental/blob"
  # Release trees must never be touched — seed one as a control.
  mkdir -p "$cache/gpn-agent-target/release/deps"
  printf 'keep' > "$cache/gpn-agent-target/release/deps/keep.o"
}

run_script() { # cache -> stdout in $OUT, exit in $RC
  set +e
  OUT=$(PATH="$TMPROOT/bin:$PATH" \
        FAKE_USED_PCT="$USED" \
        GPN_CI_CACHE="$1" \
        GPN_TARGET_ROOTS="${2:-$TMPROOT/no-such-checkout}" \
        MIN_AGE_HOURS=0 \
        FREE_FLOOR_PCT=15 \
        bash "$SCRIPT" 2>&1)
  RC=$?
  set -e
}

# ─────────────────────────────────────────────────── t1: disk is FULL ─────
CURRENT="t1 (92% used / 8% free => must prune)"
note "$CURRENT"
USED=92
CACHE="$TMPROOT/t1-cache"
make_cache "$CACHE"
run_script "$CACHE"

check '[ "$RC" -eq 0 ]' "exits 0"
check 'printf "%s" "$OUT" | grep -q "pruning debug caches"' "says it is pruning"
check 'printf "%s" "$OUT" | grep -q "8% free"' "reports the real free percentage (8%), not the used one"
check '[ ! -d "$CACHE/gpn-agent-target/debug" ]' "removed the debug tree"
check '[ ! -d "$CACHE/gpn-agent-target/incremental" ]' "removed the incremental tree"
check '[ -d "$CACHE/gpn-agent-target/release" ]' "left the release cache alone"

# ─────────────────────────────────────────── t2: disk has room => no prune ─
CURRENT="t2 (10% used / 90% free => must NOT prune)"
note "$CURRENT"
USED=10
CACHE="$TMPROOT/t2-cache"
make_cache "$CACHE"
run_script "$CACHE"

check '[ "$RC" -eq 0 ]' "exits 0"
check 'printf "%s" "$OUT" | grep -q "nothing to reclaim"' "says there is nothing to reclaim"
check '[ -d "$CACHE/gpn-agent-target/debug" ]' "kept the debug tree"
check '[ -d "$CACHE/gpn-agent-target/incremental" ]' "kept the incremental tree"

# ──────────────────────────────────── t3: exactly at the floor => no prune ─
CURRENT="t3 (85% used / 15% free => at the floor, must NOT prune)"
note "$CURRENT"
USED=85
CACHE="$TMPROOT/t3-cache"
make_cache "$CACHE"
run_script "$CACHE"

check '[ "$RC" -eq 0 ]' "exits 0"
check 'printf "%s" "$OUT" | grep -q "nothing to reclaim"' "treats 'at the floor' as healthy"

# ───────────────────────────── t5: a checkout's own target/ dir (#184) ─────
# The guard used to look only inside ~/.ci-cache. Checkouts build into
# <crate>/target, which is a different tree entirely: 66 G of it went unseen on
# 2026-11-15 while the cache was pruned and the disk still hit the floor.
CURRENT="t5 (92% used + checkout target dirs => prune debug, keep release)"
note "$CURRENT"
USED=92
CHECKOUT="$TMPROOT/t5-checkout"
rm -rf "$CHECKOUT"
mkdir -p "$CHECKOUT/gpn-agent/target/debug/deps" "$CHECKOUT/gpn-agent/target/release/deps"
mkdir -p "$CHECKOUT/gpn-node/target/debug"
mkdir -p "$CHECKOUT/not-a-target/debug"   # decoy: name only, never a Cargo profile
printf 'x' > "$CHECKOUT/gpn-agent/target/debug/deps/blob.o"
printf 'keep' > "$CHECKOUT/gpn-agent/target/release/deps/keep.o"
printf 'keep' > "$CHECKOUT/not-a-target/debug/keep.txt"
run_script "$TMPROOT/t5-cache-absent" "$CHECKOUT"

check '[ "$RC" -eq 0 ]' "exits 0 with no cache dir at all"
check 'printf "%s" "$OUT" | grep -q "pruning debug caches"' "still prunes when only a checkout exists"
check '[ ! -d "$CHECKOUT/gpn-agent/target/debug" ]' "removed the checkout's debug tree"
check '[ ! -d "$CHECKOUT/gpn-node/target/debug" ]' "removed the second crate's debug tree"
check '[ -d "$CHECKOUT/gpn-agent/target/release" ]' "left the checkout's release tree alone"
check '[ -d "$CHECKOUT/not-a-target/debug" ]' "left a directory that is only *named* debug"

# ────────────────────────────────────────────── t4: no cache dir => quiet ─
CURRENT="t4 (no cache directory)"
note "$CURRENT"
USED=92
run_script "$TMPROOT/does-not-exist"
check '[ "$RC" -eq 0 ]' "exits 0 without a cache dir"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'failures:\n'
  for f in "${FAILED[@]}"; do printf '  - %s\n' "$f"; done
  exit 1
fi
exit 0
