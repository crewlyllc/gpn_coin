#!/usr/bin/env bash
# Build the **spec 55** runtime WASM — item 251: fee-free entry becomes repeatable
# once per `FreeEntryCooldown` (5 760 blocks) per GPU fingerprint
# (`FreeEntryGpuLastEntry`) instead of a permanent `FreeEntryGpus` mark, with
# `FreeEntryGpuRateLimited` as the named refusal inside the cooldown.
# Without this, item 251 exists only in the binary's embedded runtime and cannot
# be observed on the live chain.
#
# STAGED artifact. Never touches the committed canonical genesis WASM
# (`runtime/gpn_runtime.compact.compressed.wasm`, sha256 76f37b23…16fa97):
# it is moved aside for the duration of the build and restored byte-identical
# (hash-checked) on every exit path.
#
# Features reproduce the live testnet runtime's behaviour (same set spec 53 used):
#   testnet-fast-timelock — live governance timelock is 10 blocks
#   testnet-no-stake      — live testnet is stake-free (MinAttestationStake = 0)
#   testnet-zero-ed       — no-op alias (ED = 0 unconditional since 2026-11-05)
# `testnet-lenient-identity` is deliberately NOT enabled: `RequireGpuIdentity`
# stays at its live default (true).
#
# The artifact is NOT applied by this script; no `set_code` is run here.
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

DEST="$REPO_ROOT/../wasm/gpn_runtime_spec55_free_entry_cooldown.compact.compressed.wasm"
cp -p "$OUT" "$DEST"
echo
echo "artifact: $DEST"
ls -l "$DEST"
sha256sum "$DEST"
# compact.compressed: 8-byte magic header then a zstd frame — decompress before
# looking for the version section (scanning compressed bytes finds nothing).
python3 - "$DEST" <<'PY'
import hashlib, struct, sys
b = open(sys.argv[1], 'rb').read()
print("size:                 %d bytes" % len(b))
print("blake2_256 code hash: 0x" + hashlib.blake2b(b, digest_size=32).hexdigest())
print("magic:                %s (expect 52bc537646db8e05)" % b[:8].hex())
try:
    import zstandard as zstd
    raw = zstd.ZstdDecompressor().decompress(b[8:])
except ImportError:
    import subprocess
    raw = subprocess.run(["zstd", "-d", "-c"], input=b[8:], capture_output=True).stdout
print("decompressed:         %d bytes" % len(raw))
needle = b'\x0cgpn\x0cgpn'
at = raw.find(needle)
if at < 0:
    print("spec_version:          NOT FOUND")
    sys.exit(1)
off = at + len(needle)
authoring, spec, impl = struct.unpack_from('<III', raw, off)
print("authoring_version:    %d" % authoring)
print("spec_version:         %d" % spec)
print("impl_version:         %d" % impl)
PY
echo "SPEC55 BUILD OK"
