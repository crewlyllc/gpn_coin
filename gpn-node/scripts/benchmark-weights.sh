#!/usr/bin/env bash
# Regenerate the measured `weights.rs` for every GPN pallet using
# `frame-omni-bencher`. Run from the `gpn-node` workspace root.
#
# Prerequisites:
#   1. A frame-omni-bencher binary built from the *same* polkadot-sdk rev as the
#      workspace (see gpn-node/Cargo.toml). Point FRAME_OMNI_BENCHER at it, e.g.
#      FRAME_OMNI_BENCHER=~/.cargo/git/checkouts/polkadot-sdk-*/<rev>/target/release/frame-omni-bencher
#   2. A release runtime wasm built with benchmarks:
#        cargo build -p gpn-runtime --release --features runtime-benchmarks
#
# Usage: FRAME_OMNI_BENCHER=/path/to/frame-omni-bencher scripts/benchmark-weights.sh [pallet ...]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BENCHER="${FRAME_OMNI_BENCHER:?set FRAME_OMNI_BENCHER to the frame-omni-bencher binary}"
WASM="${RUNTIME_WASM:-target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm}"
TEMPLATE="scripts/frame-weight-template.hbs"
STEPS="${STEPS:-50}"
REPEAT="${REPEAT:-20}"

# pallet-crate-dir:pallets
PALLETS=(
  "pallet_gpu_pow:pallets/gpu-pow"
  "pallet_gpu_attestation:pallets/gpu-attestation"
  "pallet_gpn_liveness:pallets/gpn-liveness"
  "pallet_gpn_treasury:pallets/gpn-treasury"
  "pallet_compute_marketplace:pallets/compute-marketplace"
  "pallet_profitability_oracle:pallets/profitability-oracle"
  "pallet_gpn_token:pallets/gpn-token"
  "pallet_shielded_pool:pallets/shielded-pool"
)

want() { [ "$#" -eq 0 ] && return 0; for a in "$@"; do [ "$a" = "${1}" ] && return 0; done; return 1; }

for entry in "${PALLETS[@]}"; do
  pallet="${entry%%:*}"
  dir="${entry##*:}"
  if [ "$#" -gt 0 ]; then
    match=0
    for a in "$@"; do [ "$a" = "$pallet" ] && match=1; done
    [ "$match" = 1 ] || continue
  fi
  echo ">>> benchmarking ${pallet} -> ${dir}/src/weights.rs"
  "$BENCHER" v1 benchmark pallet \
    --runtime "$WASM" \
    --genesis-builder none \
    --pallet "$pallet" \
    --extrinsic "*" \
    --steps "$STEPS" \
    --repeat "$REPEAT" \
    --template "$TEMPLATE" \
    --output "${dir}/src/weights.rs"
done
echo "done."
