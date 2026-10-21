#!/usr/bin/env bash
# Set the Worker's AP_TOKEN secret (the access point's bearer token).
#
# The `/api/*` proxy runs at Cloudflare's edge, so from the AP's point of view
# it is a non-loopback caller. The AP requires `Authorization: Bearer <token>`
# for those (see `GPN_AP_TOKEN` in gpn_scheduler.rs). The token must NOT go in
# wrangler.jsonc — this repo is mirrored publicly.
#
#   ./scripts/set-ap-token.sh                 # reads ~/.gpn/accesspoint.env
#   AP_TOKEN=... ./scripts/set-ap-token.sh    # or pass it explicitly
#
# Requires a logged-in wrangler (`npx wrangler login`). Setting a secret does
# not redeploy; the running Worker picks it up immediately.
set -euo pipefail
cd "$(dirname "$0")/.."

token="${AP_TOKEN:-}"
if [ -z "$token" ] && [ -f "$HOME/.gpn/accesspoint.env" ]; then
  token=$(grep -E '^GPN_AP_TOKEN=' "$HOME/.gpn/accesspoint.env" | tail -1 | cut -d= -f2- || true)
fi
if [ -z "$token" ]; then
  echo "No token found. Set AP_TOKEN=... or add GPN_AP_TOKEN=... to ~/.gpn/accesspoint.env" >&2
  exit 1
fi

printf '%s' "$token" | npx wrangler secret put AP_TOKEN --name gpn-coin
echo "AP_TOKEN set. Verify with:"
echo "  curl -s -X POST https://gpncoin.ai/api/v1/chat/completions \\"
echo "    -H 'content-type: application/json' \\"
echo "    -d '{\"model\":\"llama3.2:1b\",\"messages\":[{\"role\":\"user\",\"content\":\"say hi\"}]}'"
