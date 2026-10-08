#!/usr/bin/env bash
# Stage the minimum-GPU-capability runtime upgrade (queue item 19, spec 17)
# WITHOUT applying it.
#
# Adds the on-chain attestation *eligibility* floor `MinEligibilityGflops`
# (3,000 GFLOPS = 3 TFLOPS): a device below it cannot attest ⇒ cannot earn.
#
# Hard rules honoured:
#  - never modify genesis / the canonical genesis WASM;
#  - the canonical genesis WASM is moved aside only for the build and RESTORED
#    afterwards (verified by sha).
#
# Produces a staged artifact under shared/artifacts/project7/wasm/ with
# spec_version 17 (> on-chain 16). Does NOT submit set_code.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
CANON="$REPO/gpn-node/runtime/gpn_runtime.compact.compressed.wasm"
CANON_SHA_EXPECT="76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97"
WBUILD="$REPO/gpn-node/target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm"
OUTDIR="$REPO/shared/artifacts/project7/wasm"
OUT="$OUTDIR/gpn_runtime_spec17_min_gpu_tflops.compact.compressed.wasm"
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

echo "[stage] building runtime wasm (spec_version 17)..."
( cd "$REPO/gpn-node" && cargo build -p gpn-runtime --release )

mkdir -p "$OUTDIR"
cp -p "$WBUILD" "$OUT"
echo "[stage] staged artifact:"
sha256sum "$OUT"

echo "[stage] done -> $OUT"
