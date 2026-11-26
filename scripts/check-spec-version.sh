#!/usr/bin/env bash
# Guard against the drift that produced spec 43/44/45 on chain while the source
# tree still said 42: a runtime change without a spec_version bump cannot be
# deployed (set_code rejects a downgrade) and the mismatch is invisible until
# someone tries.
#
# Rules:
#   source spec_version  ==  network.json spec_version   ALWAYS. The registry
#   floor is what classify() compares a peer's spec against, so a floor one spec
#   behind the binary prints "healthy" for an outdated node (row 279: floor 54
#   shipped while every released binary built spec 55).
#   Raising the floor declares older nodes incompatible - that is a deliberate
#   edit made in the same commit as the runtime bump, not a side effect of it.
#   A staged, unreleased bump is not an exception: update both files together.
set -euo pipefail
repo="${GPN_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
src=$(grep -m1 -oE 'spec_version: [0-9]+' "$repo/gpn-node/runtime/src/lib.rs" | grep -oE '[0-9]+')
net=$(python3 -c "import json;print(json.load(open('$repo/network.json'))['spec_version'])")
echo "source spec_version=$src   network.json spec_version=$net"
if [ "$src" -lt "$net" ]; then
  echo "::error::source spec_version ($src) is BEHIND the registered network spec ($net) — bump it in the same commit as the runtime change" >&2
  exit 1
fi
if [ "$src" -ne "$net" ]; then
  echo "::error::network.json spec_version ($net) does not match the runtime's spec_version ($src) - the floor declares which nodes are OUTDATED, so it must name the spec this tree builds; update network.json in the same commit (raising it is a deliberate compatibility call, see gpn-agent/src/network_registry.rs)" >&2
  exit 1
fi
echo "OK"
