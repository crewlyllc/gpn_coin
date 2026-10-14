#!/usr/bin/env bash
# Stage the sudo/verifier key-rotation runtime upgrade (forkless set_code)
# WITHOUT applying it.
#
# Incident: 2026-10-14 chain sudo key leaked via a public file-server (:8083).
# This build rotates the compile-time ChallengeVerifierAccount fallback to the
# NEW account and adds a root-only mutable ChallengeVerifierAccount storage +
# `set_challenge_verifier` extrinsic (future rotations need no WASM upgrade).
#
# Hard rules honoured:
#  - never modify genesis / the canonical genesis WASM;
#  - the canonical genesis WASM is moved aside only for the build and RESTORED
#    afterwards (verified by sha).
set -euo pipefail

CANON="gpn-node/runtime/gpn_runtime.compact.compressed.wasm"
CANON_SHA_EXPECT="76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97"
WBUILD="gpn-node/target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm"
OUTDIR="artifacts/project7/sudo-rotation-2026-10-14"
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

echo "[stage] building runtime wasm..."
( cd gpn-node && cargo build -p gpn-runtime --release )

mkdir -p "$OUTDIR"
cp -p "$WBUILD" "$OUTDIR/gpn_runtime_sudo_rotation.compact.compressed.wasm"
sha256sum "$OUTDIR/gpn_runtime_sudo_rotation.compact.compressed.wasm" | tee "$OUTDIR/SHA256SUMS"

echo "[stage] done -> $OUTDIR/gpn_runtime_sudo_rotation.compact.compressed.wasm"
