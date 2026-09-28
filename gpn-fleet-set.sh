#!/usr/bin/env bash
# gpn-fleet-set.sh — set the fleet directive and publish it (writes the repo file + pushes).
#
# Usage:  GPN_GH_TOKEN=<pat> ./gpn-fleet-set.sh <network>
#   e.g.  GPN_GH_TOKEN=ghp_... ./gpn-fleet-set.sh mainnet
#
# The directive lives at:
#   https://raw.githubusercontent.com/jeremyzguo/gpn_coin/main/fleet-directive.json
# and every gpn_fleet-supervised node polls it and follows `target_network`.
set -euo pipefail

NET="${1:?usage: gpn-fleet-set.sh <network>}"
REPO="$(cd "$(dirname "$0")" && pwd)"
TOKEN="${GPN_GH_TOKEN:?set GPN_GH_TOKEN to a GitHub PAT with repo write}"

"$REPO/gpn-agent/target/release/gpn_fleet_set" "$NET" "$REPO/fleet-directive.json"

cd "$REPO"
git add fleet-directive.json
git commit -q -m "fleet: set target_network=$NET"
git push "https://jeremyzguo:${TOKEN}@github.com/jeremyzguo/gpn_coin.git" main
echo "✅ fleet directive published: target_network=$NET"
