#!/usr/bin/env bash
# Build the **spec 45** runtime WASM — testnet governance timelock 3 days → 10 blocks.
#
# STAGED artifact: patterned on build-spec44-wasm.sh. Never rebuilds spec 43/44
# artifacts and never touches the canonical genesis WASM
# (`gpn_runtime.compact.compressed.wasm`, sha256 76f37b23…16fa97): it is moved
# aside for the duration of the build and restored byte-identical (hash-checked)
# on every exit path.
#
# The change itself is FEATURE-GATED (`testnet-fast-timelock`), not an
# unconditional edit: this script passes `--features testnet-fast-timelock`, so
# the artifact carries the 10-block timelock while a plain
# `cargo build -p gpn-runtime --release` (mainnet) keeps `3 * DAYS`.
# `runtime/tests/testnet_timelock_spec45.rs` pins the feature-off value to
# 17 280 blocks so a leak into mainnet fails CI.
#
# The working tree keeps its committed `spec_version` (spec-43 artifacts are
# reproducible from HEAD); the artifact built here is **spec 45** because it
# contains the spec-43/44 code PLUS the feature-gated timelock. The bump is done
# with an in-place edit that is reverted by the same trap.
#
# The artifact is NOT applied by this script. On testnet the application path is
# `gpn_upgrade_runtime` (Sudo.sudo(System.set_code)); the staged plan is
# shared/artifacts/project7/staged/testnet-timelock-v1.md.
set -euo pipefail
cd "$(dirname "$0")/.."

REPO_ROOT=$PWD
CANON=runtime/gpn_runtime.compact.compressed.wasm
WANT_SHA=76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97
NEW_SPEC=45
FEATURE=testnet-fast-timelock
STAMP=$(date +%Y%m%d-%H%M%S)
LIBBAK=/tmp/gpn_runtime_lib_$STAMP.rs

test -f "$CANON" || { echo "canonical wasm missing"; exit 1; }
BEFORE=$(sha256sum "$CANON" | cut -d' ' -f1)
[ "$BEFORE" = "$WANT_SHA" ] || {
  echo "!! canonical wasm hash is $BEFORE, expected $WANT_SHA — refusing to build"; exit 1; }

CUR_SPEC=$(grep -oP 'spec_version:\s*\K[0-9]+' runtime/src/lib.rs | head -1)
[ "$NEW_SPEC" -gt "$CUR_SPEC" ] || { echo "spec $NEW_SPEC is not after $CUR_SPEC"; exit 1; }
cp -p runtime/src/lib.rs "$LIBBAK"
CANON_BAK=/tmp/gpn_canonical_$STAMP.wasm
cp -p "$CANON" "$CANON_BAK"

restore() {
  cp -p "$LIBBAK" runtime/src/lib.rs
  [ -f "$CANON" ] || cp -p "$CANON_BAK" "$CANON"
  AFTER=$(sha256sum "$CANON" | cut -d' ' -f1)
  NOW_SPEC=$(grep -oP 'spec_version:\s*\K[0-9]+' runtime/src/lib.rs | head -1)
  echo "canonical sha256 after:  $AFTER"
  echo "spec_version restored to: $NOW_SPEC"
  [ "$AFTER" = "$WANT_SHA" ] || { echo "!! CANONICAL WASM CHANGED !!"; exit 1; }
  [ "$NOW_SPEC" = "$CUR_SPEC" ] || { echo "!! spec_version NOT restored !!"; exit 1; }
}
trap restore EXIT

mv "$CANON" /tmp/gpn_canonical_inbuild.wasm
sed -i "s/spec_version: $CUR_SPEC,/spec_version: $NEW_SPEC,/" runtime/src/lib.rs
grep -n "spec_version" runtime/src/lib.rs | head -2
touch runtime/build.rs   # force build.rs to re-run now that the canonical file is absent

# `--features $FEATURE` is what flips `GovernanceTimelockBlocks` to 10; the
# wasm-builder forwards it into the nested wasm32 build (it enables every
# feature enabled for the native build except `default`/`std`).
cargo build -p gpn-runtime --release --offline --features "$FEATURE"

OUT=target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm
test -f "$OUT" || { echo "no wasm artifact at $OUT"; exit 1; }

DEST="$REPO_ROOT/../wasm/gpn_runtime_spec45_testnet_timelock.compact.compressed.wasm"
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
# runtime_version blob: spec_name, impl_name (SCALE strings), then
# authoring_version, spec_version, impl_version (u32 LE each).
needle = b'\x0cgpn\x0cgpn'   # compact(3) + "gpn", twice
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
