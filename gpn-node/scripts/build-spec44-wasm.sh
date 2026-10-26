#!/usr/bin/env bash
# Build the **spec 44** runtime WASM (GFLOPS floor recalibration, queue #97) as a
# STAGED artifact — patterned on build-retarget-clamp-wasm.sh.
#
# Never touches the canonical genesis WASM (`gpn_runtime.compact.compressed.wasm`,
# sha256 76f37b23…16fa97): it is moved aside for the duration of the build and
# restored byte-identical (hash-checked) on every exit path.
#
# The working tree keeps its committed `spec_version` (the spec-43 artifact is
# reproducible from HEAD); the artifact built here is **spec 44** because it
# contains the spec-43 code PLUS the recalibrated GFLOPS floor/tier constants.
# The bump is done with an in-place edit that is reverted by the same trap.
#
# The artifact is NOT applied by this script. On testnet the application path is
# `gpn_upgrade_runtime` (Sudo.sudo(System.set_code)); the staged plan is
# shared/artifacts/project7/staged/gflops-floor-v1.md.
set -euo pipefail
cd "$(dirname "$0")/.."

REPO_ROOT=$PWD
CANON=runtime/gpn_runtime.compact.compressed.wasm
WANT_SHA=76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97
NEW_SPEC=44
STAMP=$(date +%Y%m%d-%H%M%S)
LIBBAK=/tmp/gpn_runtime_lib_$STAMP.rs

test -f "$CANON" || { echo "canonical wasm missing"; exit 1; }
BEFORE=$(sha256sum "$CANON" | cut -d' ' -f1)
[ "$BEFORE" = "$WANT_SHA" ] || {
  echo "!! canonical wasm hash is $BEFORE, expected $WANT_SHA — refusing to build"; exit 1; }

CUR_SPEC=$(grep -oP 'spec_version:\s*\K[0-9]+' runtime/src/lib.rs | head -1)
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
touch runtime/build.rs   # force build.rs to re-run now that the file is absent

cargo build -p gpn-runtime --release --offline

OUT=target/release/wbuild/gpn-runtime/gpn_runtime.compact.compressed.wasm
test -f "$OUT" || { echo "no wasm artifact at $OUT"; exit 1; }

DEST="$REPO_ROOT/../wasm/gpn_runtime_spec44_gflops_floor.compact.compressed.wasm"
cp -p "$OUT" "$DEST"
echo
echo "artifact: $DEST"
ls -l "$DEST"
sha256sum "$DEST"
python3 - "$DEST" <<'PY'
import hashlib, sys
b = open(sys.argv[1], 'rb').read()
print("size:                 %d bytes" % len(b))
print("blake2_256 code hash: 0x" + hashlib.blake2b(b, digest_size=32).hexdigest())
PY
