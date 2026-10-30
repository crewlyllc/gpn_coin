#!/usr/bin/env bash
# test-launcher-identity.sh — offline gate for the Windows launcher's identities
# (queue #96).
#
# Extracts the `$script = @'…'@` here-string that release-windows.yml publishes
# as C:\gpn\start-node.cmd and asserts the rules that the workflow enforces at
# build time, so a regression is caught on Linux CI without a Windows box:
#
#   * a node identity FILE, never a literal key           (item #90)
#   * a per-machine WALLET, never a dev URI / pinned account (item #96)
#   * an actual `--ensure-wallet` call before the node starts
#   * negative control: the pre-#96 launcher MUST be rejected
#
# Usage: bash scripts/tests/test-launcher-identity.sh
set -uo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
wf="$repo/.github/workflows/release-windows.yml"
cmdfile="$repo/scripts/run-windows.cmd"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail=0
ok()   { printf '  ok   %s\n' "$*"; }
bad()  { printf '  FAIL %s\n' "$*"; fail=1; }

extract_launcher() { # $1 = workflow file, $2 = output file
  awk '/\$script = @'"'"'$/{f=1;next} f && /^ *'"'"'@$/{exit} f{gsub(/^ {10}/,"");print}' "$1" > "$2"
}

# The rules, in one place. Returns 0 when the text is safe to publish.
# Mirrors Assert-LauncherSafe in .github/workflows/release-windows.yml.
check_launcher() { # $1 = file, $2 = label
  local f="$1" label="$2" problems=()
  grep -q 'GPN_NODE_KEY_FILE=' "$f" || problems+=("no GPN_NODE_KEY_FILE (no per-machine node identity)")
  grep -Eq '(^|[^0-9a-fA-F])[0-9a-fA-F]{64}([^0-9a-fA-F]|$)' "$f" && problems+=("literal 64-hex key")
  grep -Eq 'GPN_NODE_KEY=[0-9a-fA-F]' "$f" && problems+=("assigns GPN_NODE_KEY")
  # money identity
  grep -Eqi '//(alice|bob|charlie|dave|eve|ferdie)' "$f" && problems+=("dev URI (//Bob etc.) — a shared, public key")
  grep -Eqi 'GPN_AUTHOR' "$f" && problems+=("sets GPN_AUTHOR")
  grep -Eq 'GPN_SIGNER[[:space:]]*=[[:space:]]*[^"[:space:]]' "$f" && problems+=("pins GPN_SIGNER")
  grep -Eq '5[1-9A-HJ-NP-Za-km-z]{47}' "$f" && problems+=("hard-coded SS58 account")
  grep -q -- '--ensure-wallet' "$f" || problems+=("does not call gpn_node --ensure-wallet")
  grep -q 'GPN_SIGNER_FILE=' "$f" || problems+=("no GPN_SIGNER_FILE")
  # A node with no access point receives no AI work at all (2026-10-30: 4 peers,
  # 2 providers). The launcher must name one.
  grep -q 'GPN_AP_URL=' "$f" || problems+=("no GPN_AP_URL - the node would never poll for AI work (miner only)")

  if [ "${#problems[@]}" -eq 0 ]; then
    ok "$label: per-machine node key + wallet, no dev URI, no pinned account"
    return 0
  fi
  bad "$label: ${problems[*]}"
  return 1
}

echo "== extracted launcher (release-windows.yml) =="
extract_launcher "$wf" "$tmp/start-node.cmd"
if [ ! -s "$tmp/start-node.cmd" ]; then
  bad "could not extract the launcher here-string from $wf"
else
  # The published file has __CHAIN__ substituted at deploy time; do the same so
  # the check runs on bytes as close to the shipped ones as possible.
  sed -i 's/__CHAIN__/testnet/g' "$tmp/start-node.cmd"
  check_launcher "$tmp/start-node.cmd" "generated start-node.cmd"

  # Specific regressions worth naming (they were the reported defect).
  grep -q 'gpn_node.exe" --ensure-wallet' "$tmp/start-node.cmd" \
    && ok "wallet is provisioned before the node starts (supervisor binary)" \
    || bad "no 'gpn_node.exe --ensure-wallet' line"
  grep -q 'set "GPN_SIGNER_FILE=%GPN_BASE_PATH%\\keystore.json"' "$tmp/start-node.cmd" \
    && ok "keystore pinned to <base>\\keystore.json" \
    || bad "GPN_SIGNER_FILE is not <base>\\keystore.json"
  grep -q 'if errorlevel 1' "$tmp/start-node.cmd" \
    && ok "provisioning failure stops the launcher" \
    || bad "no errorlevel guard after --ensure-wallet"
  # AI serving out of the box: the shipped launcher must point at the canonical
  # access point, and the same value must appear in scripts/run-windows.cmd.
  grep -q 'set "GPN_AP_URL=http://gpn-testnet.duckdns.org:8092"' "$tmp/start-node.cmd" \
    && ok "generated start-node.cmd exports the canonical GPN_AP_URL" \
    || bad "start-node.cmd does not set GPN_AP_URL to the canonical access point"
fi

echo "== scripts/run-windows.cmd =="
check_launcher "$cmdfile" "scripts/run-windows.cmd" || true
grep -q 'set "GPN_AP_URL=http://gpn-testnet.duckdns.org:8092"' "$cmdfile" \
  && ok "run-windows.cmd exports the canonical GPN_AP_URL" \
  || bad "run-windows.cmd does not set GPN_AP_URL to the canonical access point"
grep -Eqi '//(alice|bob)' "$cmdfile" && bad "run-windows.cmd still mentions a dev URI" || ok "no dev URI"

echo "== negative control (pre-#96 template must be REJECTED) =="
cat > "$tmp/legacy.cmd" <<'EOF'
@echo off
set "GPN_SIGNER=//Bob"
set "GPN_AUTHOR=bob"
set "GPN_ACCOUNT=5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty"
set "GPN_NODE_KEY_FILE=%USERPROFILE%\.gpn\node-key"
EOF
if ( check_launcher "$tmp/legacy.cmd" "legacy template" ) >/dev/null 2>&1; then
  bad "self-check is BROKEN: the pre-#96 launcher (//Bob + GPN_AUTHOR + pinned account) PASSED"
else
  ok "pre-#96 launcher correctly rejected"
fi

echo
if [ "$fail" -ne 0 ]; then
  echo "RESULT: FAIL"
  exit 1
fi
echo "RESULT: PASS"
