#!/usr/bin/env bash
# Build the spec-42 runtime WASM *without* touching the canonical genesis WASM.
# The runtime's build.rs embeds `runtime/gpn_runtime.compact.compressed.wasm`
# when it exists, so it is moved aside for the duration of the build and
# restored (hash-verified) no matter what happens.
set -euo pipefail
cd /home/jeremyg/.openclaw/workspace/projectmanager07/gpn-node

CANON=runtime/gpn_runtime.compact.compressed.wasm
BAK=/tmp/gpn_canonical_$(date +%s).wasm

test -f "$CANON" || { echo "canonical wasm missing"; exit 1; }
BEFORE=$(sha256sum "$CANON" | cut -d' ' -f1)
echo "canonical sha256 before: $BEFORE"
cp -p "$CANON" "$BAK"

restore() {
  if [ ! -f "$CANON" ]; then
    cp -p "$BAK" "$CANON"
    echo "restored canonical wasm from $BAK"
  fi
  AFTER=$(sha256sum "$CANON" | cut -d' ' -f1)
  echo "canonical sha256 after:  $AFTER"
  [ "$BEFORE" = "$AFTER" ] || { echo "!! CANONICAL WASM CHANGED !!"; exit 1; }
}
trap restore EXIT

mv "$CANON" /tmp/gpn_canonical_inbuild.wasm
# force the build script to re-run now that the file is absent
touch runtime/build.rs
cargo build -p gpn-runtime --release --offline

OUT=target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm
test -f "$OUT" || { echo "no wasm artifact at $OUT"; exit 1; }
ls -l "$OUT"
sha256sum "$OUT"
