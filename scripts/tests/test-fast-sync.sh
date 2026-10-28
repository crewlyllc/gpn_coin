#!/usr/bin/env bash
# test-fast-sync.sh — offline tests for install.sh's fast-sync (weekly snapshot)
# path. No network, no cargo, no real chain: a tiny fake snapshot is exported
# with the real `gpn_snapshot` and served from a local HTTP server.
#
# Asserts:
#   1. success        → snapshot imported, chain DB in place, "bootstrapped" said
#   2. hash mismatch  → falls back, NOTHING imported
#   3. 404 metadata   → falls back, NOTHING imported
#   4. 404 archive    → falls back, NOTHING imported
#   5. --no-fast-sync → skipped cleanly
#   6. GPN_FAST_SYNC=0 → skipped cleanly
#   7. pre-existing DB (no archive marker) → never touched
#   8. Windows launcher (generated + scripts/run-windows.cmd) carries the same
#      guard sequence (text-level only: no pwsh/cmd on this host)
#
# Usage: scripts/tests/test-fast-sync.sh [--keep]
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SNAP_BIN="${GPN_SNAPSHOT_BIN:-$REPO/gpn-agent/target/release/gpn_snapshot}"
KEEP=0
[ "${1:-}" = "--keep" ] && KEEP=1

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  \033[32mok\033[0m   %s\n' "$*"; }
bad()  { fail=$((fail+1)); printf '  \033[31mFAIL\033[0m %s\n' "$*"; }
check() { # check <desc> <cmd...>
  local d="$1"; shift
  if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi
}

[ -x "$SNAP_BIN" ] || { echo "FATAL: $SNAP_BIN missing (cargo build --release --bin gpn_snapshot)" >&2; exit 2; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/gpn-fs-test.XXXXXX")"
cleanup() {
  [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null
  [ "$KEEP" = 1 ] && { echo "kept: $ROOT"; return; }
  rm -rf "$ROOT"
}
trap cleanup EXIT

WWW="$ROOT/www"; mkdir -p "$WWW"

# ── a tiny fake chain DB + snapshot ───────────────────────────────────────────
SRC_BASE="$ROOT/fake-node"
mkdir -p "$SRC_BASE/chains/gpn_testnet/db/full"
printf 'alpha' > "$SRC_BASE/chains/gpn_testnet/db/full/000001.sst"
printf 'beta'  > "$SRC_BASE/chains/gpn_testnet/db/full/000002.sst"
printf 'CURRENT\n' > "$SRC_BASE/chains/gpn_testnet/db/CURRENT"
"$SNAP_BIN" export --base-path "$SRC_BASE" --out "$WWW/gpn-testnet-snapshot.tar.gz" \
  --rpc http://127.0.0.1:1 >/dev/null 2>&1 \
  || { echo "FATAL: fake export failed" >&2; exit 2; }

SIZE=$(stat -c %s "$WWW/gpn-testnet-snapshot.tar.gz")
SHA=$(sha256sum "$WWW/gpn-testnet-snapshot.tar.gz" | cut -d' ' -f1)
URL="https://example.invalid/gpn-testnet-snapshot.tar.gz"

sign() { # sign <out> <sha> <size>
  /usr/bin/python3 "$REPO/scripts/snapshot-metadata.py" sign \
    --url "$URL" --sha256 "$2" --size "$3" --block 4242 --built "20261028-000000 UTC" \
    --out "$1" >/dev/null
}
sign "$WWW/snapshot.json" "$SHA" "$SIZE"
# A document signed over DIFFERENT bytes than the archive we serve.
sign "$WWW/snapshot-badhash.json" "00$(printf 'ab%.0s' $(seq 1 31))" "$SIZE"

# ── local HTTP server ─────────────────────────────────────────────────────────
/usr/bin/python3 -u -m http.server 0 --bind 127.0.0.1 --directory "$WWW" >"$ROOT/http.log" 2>&1 &
SRV_PID=$!
for _ in $(seq 1 50); do
  PORT=$(sed -n 's/.*port \([0-9]\+\).*/\1/p' "$ROOT/http.log" | head -1)
  [ -n "${PORT:-}" ] && break
  sleep 0.1
done
[ -n "${PORT:-}" ] || { echo "FATAL: local HTTP server did not start" >&2; cat "$ROOT/http.log"; exit 2; }

# ── a prefix that satisfies every non-fast-sync step of install.sh ────────────
make_prefix() { # make_prefix <dir>
  local p="$1"
  mkdir -p "$p/src/.git" "$p/src/gpn-node/target/release" "$p/src/gpn-agent/target/release" \
           "$p/ollama/bin" "$p/fakebin"
  : > "$p/ollama/bin/ollama"; chmod +x "$p/ollama/bin/ollama"
  # cargo/git stubs: the installer must not actually build or clone in a test.
  printf '#!/bin/sh\nexit 0\n' > "$p/fakebin/cargo"
  printf '#!/bin/sh\nexit 0\n' > "$p/fakebin/git"
  chmod +x "$p/fakebin/cargo" "$p/fakebin/git"
  # Wallet stub (installer runs `gpn_node --ensure-wallet`).
  cat > "$p/src/gpn-node/target/release/gpn-node" <<'EOF'
#!/bin/sh
case " $* " in *" --ensure-wallet "*) echo "payout account: 5FakeFakeFakeFakeFakeFakeFakeFakeFakeFakeFakeFak" ;; esac
exit 0
EOF
  chmod +x "$p/src/gpn-node/target/release/gpn-node"
  # Same for the agent's gpn_node, plus a delegating gpn_snapshot.
  cp "$p/src/gpn-node/target/release/gpn-node" "$p/src/gpn-agent/target/release/gpn_node"
  cat > "$p/src/gpn-agent/target/release/gpn_snapshot" <<EOF
#!/bin/sh
exec "$SNAP_BIN" "\$@"
EOF
  chmod +x "$p/src/gpn-agent/target/release/gpn_node" "$p/src/gpn-agent/target/release/gpn_snapshot"
}

run_install() { # run_install <prefix> <meta-url> [extra env as K=V... | -- install args]
  local p="$1" meta="$2"; shift 2
  local envs=()
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done
  [ "${1:-}" = "--" ] && shift
  env PATH="$p/fakebin:$PATH" GPN_PREFIX="$p" GPN_BIN="$p/bin" \
      GPN_SNAPSHOT_URL="http://127.0.0.1:$PORT/gpn-testnet-snapshot.tar.gz" \
      GPN_SNAPSHOT_META_URL="$meta" "${envs[@]}" \
      bash "$REPO/install.sh" "$@" >"$p/out.log" 2>&1
  echo $? > "$p/rc"
}

echo "== fast sync tests (http://127.0.0.1:$PORT) =="

# 1 ── success ────────────────────────────────────────────────────────────────
P="$ROOT/p1"; make_prefix "$P"
run_install "$P" "http://127.0.0.1:$PORT/snapshot.json"
check "success: installer exit 0"                    test "$(cat "$P/rc")" = 0
check "success: chain DB imported into \$GPN_BASE_PATH" test -f "$P/node/chains/gpn_testnet/db/full/000001.sst"
check "success: archive-pruning marker written"      test -f "$P/node/.gpn-pruning-archive"
check "success: says it bootstrapped (with block)"   grep -q "bootstrapped from the weekly snapshot at block 4242" "$P/out.log"
check "success: no temp dir left behind"             bash -c '! ls -d "$1"/.fast-sync.* >/dev/null 2>&1' _ "$P"

# 2 ── hash mismatch: signed metadata describes other bytes ───────────────────
P="$ROOT/p2"; make_prefix "$P"
run_install "$P" "http://127.0.0.1:$PORT/snapshot-badhash.json"
check "hash mismatch: installer still exit 0"          test "$(cat "$P/rc")" = 0
check "hash mismatch: NOTHING imported"                test ! -e "$P/node/chains"
check "hash mismatch: one-line fallback message"       grep -q "fast sync unavailable (snapshot failed verification" "$P/out.log"

# 3 ── 404 metadata ───────────────────────────────────────────────────────────
P="$ROOT/p3"; make_prefix "$P"
run_install "$P" "http://127.0.0.1:$PORT/does-not-exist.json"
check "404 meta: installer still exit 0"               test "$(cat "$P/rc")" = 0
check "404 meta: NOTHING imported"                     test ! -e "$P/node/chains"
check "404 meta: fallback message"                     grep -q "snapshot metadata not available" "$P/out.log"

# 4 ── 404 archive (valid metadata) ───────────────────────────────────────────
P="$ROOT/p4"; make_prefix "$P"
run_install "$P" "http://127.0.0.1:$PORT/snapshot.json" GPN_SNAPSHOT_URL="http://127.0.0.1:$PORT/nope.tar.gz"
check "404 archive: installer still exit 0"            test "$(cat "$P/rc")" = 0
check "404 archive: NOTHING imported"                  test ! -e "$P/node/chains"
check "404 archive: fallback message"                  grep -q "snapshot download failed" "$P/out.log"

# 5 ── --no-fast-sync ─────────────────────────────────────────────────────────
P="$ROOT/p5"; make_prefix "$P"
run_install "$P" "http://127.0.0.1:$PORT/snapshot.json" -- --no-fast-sync
check "--no-fast-sync: exit 0"                         test "$(cat "$P/rc")" = 0
check "--no-fast-sync: nothing imported"               test ! -e "$P/node/chains"
check "--no-fast-sync: says so"                        grep -q "fast sync disabled (--no-fast-sync)" "$P/out.log"
check "--no-fast-sync: never fetched metadata"         bash -c '! grep -q "fetching signed snapshot metadata" "$1"' _ "$P/out.log"

# 6 ── GPN_FAST_SYNC=0 ────────────────────────────────────────────────────────
P="$ROOT/p6"; make_prefix "$P"
run_install "$P" "http://127.0.0.1:$PORT/snapshot.json" GPN_FAST_SYNC=0
check "GPN_FAST_SYNC=0: exit 0"                        test "$(cat "$P/rc")" = 0
check "GPN_FAST_SYNC=0: nothing imported"              test ! -e "$P/node/chains"
check "GPN_FAST_SYNC=0: says so"                       grep -q "fast sync disabled (GPN_FAST_SYNC=0)" "$P/out.log"

# 7 ── pre-existing DB from another pruning mode: never touched ───────────────
P="$ROOT/p7"; make_prefix "$P"
mkdir -p "$P/node/chains/gpn_testnet/db/full"
printf 'PRECIOUS' > "$P/node/chains/gpn_testnet/db/full/000001.sst"
BEFORE=$(find "$P/node/chains" -type f -exec sha256sum {} + | sort)
run_install "$P" "http://127.0.0.1:$PORT/snapshot.json"
AFTER=$(find "$P/node/chains" -type f -exec sha256sum {} + | sort)
check "existing DB: exit 0"                            test "$(cat "$P/rc")" = 0
check "existing DB: content unchanged"                 test "$BEFORE" = "$AFTER"
check "existing DB: sentinel intact"                   grep -q PRECIOUS "$P/node/chains/gpn_testnet/db/full/000001.sst"
check "existing DB: marker NOT added"                  test ! -e "$P/node/.gpn-pruning-archive"
check "existing DB: warns it did not touch it"         grep -q "NOT touching it" "$P/out.log"
check "existing DB: did not download anything"         bash -c '! grep -q "fetching signed snapshot metadata" "$1"' _ "$P/out.log"

# ── 8. Windows launcher: same guard sequence (text assertions only) ──────────
echo "== windows launcher (text-level; no pwsh/cmd here) =="
guard_seq() { # guard_seq <file> -> "1:2:...:8" if the guard sequence appears in order
  local f="$1" out="" last=0 i=1 ln m
  local markers=(
    'if "%GPN_FAST_SYNC%"=="0"'
    'existing chain DB'
    'curl.exe -fsSL'
    'verify --meta "%GPN_FS_TMP%\snapshot.json" >'
    'GPN_SNAP_SIZE% * 3'
    'verify --meta "%GPN_FS_TMP%\snapshot.json" --snapshot'
    '--pruning archive'
    'move "%GPN_FS_TMP%\stage\chains"'
  )
  for m in "${markers[@]}"; do
    ln=$(grep -n -F -- "$m" "$f" 2>/dev/null | head -1 | cut -d: -f1)
    if [ -n "$ln" ] && [ "$ln" -gt "$last" ]; then
      last=$ln; out="${out}${i}:"
    else
      out="${out}x:"; fi
    i=$((i+1))
  done
  printf '%s' "${out%:}"
}

extract_win() { # pull the generated start-node.cmd out of the workflow here-string
  awk '/REM AUTO-GENERATED by .github\/workflows\/release-windows.yml/{f=1} f&&/^ *'"'"'@$/{exit} f' \
    "$REPO/.github/workflows/release-windows.yml" | sed 's/^          //'
}
WIN_GEN="$ROOT/start-node.cmd"; extract_win > "$WIN_GEN"
check "windows: launcher extracted from workflow"      test -s "$WIN_GEN"
for f in "$WIN_GEN" "$REPO/scripts/run-windows.cmd"; do
  n=$(basename "$f")
  check "windows/$n: opt-out guard (GPN_FAST_SYNC==0)"  grep -q 'if "%GPN_FAST_SYNC%"=="0"' "$f"
  check "windows/$n: existing-DB guard"                 grep -q 'existing chain DB' "$f"
  check "windows/$n: pruning marker check"              grep -q '\.gpn-pruning-archive' "$f"
  check "windows/$n: signed metadata verify"            grep -q 'gpn_snapshot.exe" verify --meta' "$f"
  check "windows/$n: archive verify (hash+size)"        grep -q 'verify --meta "%GPN_FS_TMP%\\snapshot.json" --snapshot' "$f"
  check "windows/$n: 3x disk guard"                     grep -q 'GPN_SNAP_SIZE% \* 3' "$f"
  check "windows/$n: import --pruning archive"          grep -q 'import --snapshot "%GPN_FS_TMP%\\snap.tar.gz" --base-path "%GPN_FS_TMP%\\stage" --pruning archive' "$f"
  check "windows/$n: staging, not the real base path"   grep -q 'move "%GPN_FS_TMP%\\stage\\chains" "%GPN_BASE_PATH%\\chains"' "$f"
  check "windows/$n: fallback is never fatal"           grep -q 'falling back to a normal full peer sync' "$f"
  check "windows/$n: reports the bootstrapped block"    grep -q 'bootstrapped from the weekly snapshot at block' "$f"
  check "windows/$n: same guard order as Linux path" \
    test "$(guard_seq "$f")" = "1:2:3:4:5:6:7:8"
done

echo
if [ "$fail" -eq 0 ]; then
  printf '\033[32mALL %d FAST-SYNC CHECKS PASSED\033[0m\n' "$pass"
  exit 0
fi
printf '\033[31m%d passed, %d FAILED\033[0m\n' "$pass" "$fail"
exit 1
