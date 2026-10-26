#!/usr/bin/env bash
# Stage + tar the Linux release bundle (layout=bundle).
# Used by .github/workflows/release-linux-selfhosted.yml and manually:
#
#   CT_NODE=... CT_AGENT=... scripts/ci/package-linux-bundle.sh /tmp/gpn-linux.tar.gz
#
# Layout (unchanged from the original release-unix.yml packaging):
#   gpn-node                the chain node/miner
#   gpn_*                   the agent binaries at the bundle root
#   scripts/install-ollama.sh, scripts/install-ollama-amd.sh
#   gpn-node.example.toml, README-FIRST-RUN.txt
#
# Differences from the old glob: only bins that gpn-agent/Cargo.toml actually
# declares are copied (the old `gpn_*` glob also swept in rustc *.d dep-info
# files and stale binaries left in target/release from removed bin targets).
set -euo pipefail

OUT=${1:-gpn-linux.tar.gz}
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CT_NODE=${CT_NODE:-$REPO_ROOT/gpn-node/target}
CT_AGENT=${CT_AGENT:-$REPO_ROOT/gpn-agent/target}
CANON_SHA=${CANON_SHA:-76f37b23a33e8ba3ada91849e69b66836590673f14ea009ef6d914eb0d16fa97}
cd "$REPO_ROOT"

# 1. canonical genesis runtime WASM must be byte-identical (the runtime build
#    script embeds it; a build that regenerates it changes the chain).
got=$(sha256sum gpn-node/runtime/gpn_runtime.compact.compressed.wasm | cut -d' ' -f1)
[ "$got" = "$CANON_SHA" ] || { echo "ERROR: canonical runtime WASM is $got, expected $CANON_SHA" >&2; exit 1; }

# 2. the exact set of agent bins to ship, from the manifest (not a glob)
bins=$(cargo metadata --no-deps --format-version 1 --manifest-path gpn-agent/Cargo.toml \
  | python3 -c 'import json,sys
d=json.load(sys.stdin)
print("\n".join(sorted({t["name"] for p in d["packages"] for t in p["targets"]
                       if "bin" in t["kind"] and t["name"].startswith("gpn_")})))')

stage=$(mktemp -d "${TMPDIR:-/tmp}/gpn-linux-stage.XXXXXX")
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/scripts"
n=0
for b in $bins; do
  [ -f "$CT_AGENT/release/$b" ] || { echo "ERROR: missing binary $CT_AGENT/release/$b" >&2; exit 1; }
  cp "$CT_AGENT/release/$b" "$stage/"
  n=$((n+1))
done
[ "$n" -ge 50 ] || { echo "ERROR: only $n agent binaries staged" >&2; exit 1; }
cp "$CT_NODE/release/gpn-node" "$stage/"
cp scripts/install-ollama.sh scripts/install-ollama-amd.sh "$stage/scripts/"
cp gpn-node.example.toml README-FIRST-RUN.txt "$stage/"
chmod +x "$stage"/gpn_* "$stage/scripts/"*.sh

for f in gpn-node gpn_node gpn_llm_provider gpn_finetune_provider gpn_attest gpn_wallet \
         scripts/install-ollama.sh scripts/install-ollama-amd.sh \
         gpn-node.example.toml README-FIRST-RUN.txt; do
  [ -s "$stage/$f" ] || { echo "ERROR: bundle is missing required file: $f" >&2; exit 1; }
done

rm -f "$OUT"
tar -czf "$OUT" -C "$stage" .
echo "package contents ($n agent bins):"
find "$stage" -type f | sed "s|$stage/|  |" | sort
echo "asset: $OUT size=$(stat -c%s "$OUT") sha256=$(sha256sum "$OUT" | cut -d' ' -f1)"
