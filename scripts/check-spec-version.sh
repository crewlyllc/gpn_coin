#!/usr/bin/env bash
# Guard against the drift that produced spec 43/44/45 on chain while the source
# tree still said 42: a runtime change without a spec_version bump cannot be
# deployed (set_code rejects a downgrade) and the mismatch is invisible until
# someone tries.
#
# Rules:
#   source spec_version  >=  network.json spec_version   (source must never be behind)
#   source spec_version  ==  network.json spec_version   only while unreleased;
#   once shipped, bump the source, then update network.json after the upgrade.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
src=$(grep -m1 -oE 'spec_version: [0-9]+' "$repo/gpn-node/runtime/src/lib.rs" | grep -oE '[0-9]+')
net=$(python3 -c "import json;print(json.load(open('$repo/network.json'))['spec_version'])")
echo "source spec_version=$src   network.json spec_version=$net"
if [ "$src" -lt "$net" ]; then
  echo "::error::source spec_version ($src) is BEHIND the registered network spec ($net) — bump it in the same commit as the runtime change" >&2
  exit 1
fi
if [ "$src" -gt $((net + 1)) ]; then
  echo "::warning::source spec_version ($src) is more than one ahead of the registered network spec ($net) — expected at most one unreleased bump"
fi
echo "OK"
