#!/usr/bin/env bash
# Build the **spec 53** runtime WASM — item 231: `register_provider`
# (compute-marketplace `call_index 0`) becomes `Pays::No`, so no registration
# path charges a fee. Without this, "joining costs nothing" is false for every
# caller that uses the plain path, and no runtime on chain carries the fix
# (spec 52 predates it).
#
# STAGED artifact. Never touches the committed canonical genesis WASM
# (`runtime/gpn_runtime.compact.compressed.wasm`, sha256 76f37b23…16fa97):
# it is moved aside for the duration of the build and restored byte-identical
# (hash-checked) on every exit path.
#
# Features are chosen to REPRODUCE the live testnet runtime's behaviour:
#   testnet-fast-timelock  — live governance timelock is 10 blocks; without this
#                            feature the upgrade would silently restore
#                            17 280 blocks (3 days) on testnet.
#   testnet-no-stake       — live testnet is stake-free (MinAttestationStake = 0).
#   testnet-zero-ed        — no-op alias; ED = 0 is unconditional since 2026-11-05.
# `testnet-lenient-identity` is deliberately NOT enabled: `RequireGpuIdentity`
# stays at its live default (true).
#
# The artifact is NOT applied by this script; no `set_code` is run here.
# Enactment is governance-only / owner-gated (docs/upgrades-governance.md — the
# testnet sudo key is Jeremy's offline key, so this machine cannot enact it).
set -euo pipefail
cd "$(dirname "$0")/.."

REPO_ROOT=$PWD
CANON=runtime/gpn_runtime.compact.compressed.wasm
WANT_SHA=76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97
FEATURES=testnet-zero-ed,testnet-no-stake,testnet-fast-timelock
STAMP=$(date +%Y%m%d-%H%M%S)

test -f "$CANON" || { echo "canonical wasm missing"; exit 1; }
BEFORE=$(sha256sum "$CANON" | cut -d' ' -f1)
[ "$BEFORE" = "$WANT_SHA" ] || {
  echo "!! canonical wasm hash is $BEFORE, expected $WANT_SHA — refusing to build"; exit 1; }

grep -n "spec_version" runtime/src/lib.rs | head -2
CANON_BAK=/tmp/gpn_canonical_$STAMP.wasm
cp -p "$CANON" "$CANON_BAK"

restore() {
  [ -f "$CANON" ] || cp -p "$CANON_BAK" "$CANON"
  AFTER=$(sha256sum "$CANON" | cut -d' ' -f1)
  echo "canonical sha256 after: $AFTER"
  [ "$AFTER" = "$WANT_SHA" ] || { echo "!! CANONICAL WASM CHANGED !!"; exit 1; }
}
trap restore EXIT

mv "$CANON" /tmp/gpn_canonical_inbuild.wasm
touch runtime/build.rs   # force build.rs to re-run now that the canonical file is absent

CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.ci-cache/gpn-node-target} \
  cargo build -p gpn-runtime --release --features "$FEATURES"

OUT=${CARGO_TARGET_DIR:-$HOME/.ci-cache/gpn-node-target}/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm
test -f "$OUT" || { echo "no wasm artifact at $OUT"; exit 1; }

DEST="$REPO_ROOT/../wasm/gpn_runtime_spec53_free_registration.compact.compressed.wasm"
cp -p "$OUT" "$DEST"
echo
echo "artifact: $DEST"
ls -l "$DEST"
sha256sum "$DEST"
python3 - "$DEST" <<'PY'
import hashlib, struct, sys
b = open(sys.argv[1], 'rb').read()
print("size:                 %d bytes" % len(b))
print("blake2_256 code hash: 0x" + hashlib.blake2b(b, digest_size=32).hexdigest())
needle = b'\x0cgpn\x0cgpn'
at = b.find(needle)
if at < 0:
    print("spec_version:          NOT FOUND")
    sys.exit(1)
off = at + len(needle)
authoring, spec, impl = struct.unpack_from('<III', b, off)
print("authoring_version:    %d" % authoring)
print("spec_version:         %d" % spec)
print("impl_version:         %d" % impl)
PY
echo "SPEC53 BUILD OK"
