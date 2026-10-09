#!/usr/bin/env bash
# Stage the spec-23 runtime upgrade (forkless set_code) WITHOUT applying it.
#
# Item #16 red-team finding #13: treasury set_roster guard-after-write.
#
# Hard rules honoured:
#  - never modify genesis / the canonical genesis WASM;
#  - the canonical genesis WASM is moved aside only for the build and RESTORED
#    afterwards (verified by sha).
set -euo pipefail

cd "$(dirname "$0")/../.."   # repo root
CANON="gpn-node/runtime/gpn_runtime.compact.compressed.wasm"
CANON_SHA_EXPECT="76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97"
WBUILD="gpn-node/target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm"
OUTDIR="shared/artifacts/project7/wasm"
OUT="$OUTDIR/gpn_runtime_spec23_redteam_treasury_roster.compact.compressed.wasm"
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

echo "[stage] building runtime wasm (spec_version 23)..."
( cd gpn-node && cargo build -p gpn-runtime --release )

mkdir -p "$OUTDIR"
cp -p "$WBUILD" "$OUT"

# Restore the canonical genesis WASM before verifying (the trap also restores
# on any early exit).
restore

echo "[stage] verifying restored canonical genesis WASM sha..."
echo "$CANON_SHA_EXPECT  $CANON" | sha256sum -c -

sha256sum "$OUT" | tee "$OUT.sha256"

echo "[stage] done -> $OUT"
