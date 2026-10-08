#!/usr/bin/env bash
# Stage the red-team runtime upgrade (forkless set_code) WITHOUT applying it.
#
# Hard rules honoured:
#  - never modify genesis / the canonical genesis WASM;
#  - the canonical genesis WASM is moved aside only for the build and RESTORED
#    afterwards (verified by sha).
#
# Produces an artifact under artifacts/project7/redteam-2026-10-08/ with
# spec_version 16 (> on-chain 15). Does NOT submit set_code.
set -euo pipefail

CANON="gpn-node/runtime/gpn_runtime.compact.compressed.wasm"
CANON_SHA_EXPECT="76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97"
WBUILD="gpn-node/target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm"
OUTDIR="artifacts/project7/redteam-2026-10-08"
BACKUP="$(mktemp -d)/canonical.wasm"

restore() {
  if [ ! -f "$CANON" ]; then
    cp -p "$BACKUP" "$CANON"
    echo "[stage] restored canonical genesis WASM"
  fi
}
trap restore EXIT

echo "[stage] verifying canonical genesis WASM sha..."
echo "$CANON_SHA_EXPECT  $CANON" | sha256sum -c -

echo "[stage] moving canonical genesis WASM aside..."
mv "$CANON" "$BACKUP"

echo "[stage] building runtime wasm (spec_version 16)..."
( cd gpn-node && cargo build -p gpn-runtime --release )

mkdir -p "$OUTDIR"
cp -p "$WBUILD" "$OUTDIR/gpn_runtime.compact.compressed.v16.wasm"
sha256sum "$OUTDIR/gpn_runtime.compact.compressed.v16.wasm" | tee "$OUTDIR/SHA256SUMS"

echo "[stage] done -> $OUTDIR/gpn_runtime.compact.compressed.v16.wasm"
