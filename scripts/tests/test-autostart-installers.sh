#!/usr/bin/env bash
# test-autostart-installers.sh — offline gate for "a downloaded install survives
# a reboot" (2026-11-01).
#
# The defect this guards against: the release zip/tarball ships only binaries.
# Extracting it and running gpn_node starts a node with NO autostart entry, so
# after a reboot nothing runs. Ollama has the same gap, and gpn_node refuses to
# start without it. Two Windows boxes were verified down after a reboot.
#
# What is asserted here (all on Linux, no Windows box needed):
#   Windows  install.ps1
#     * registers GPN-Node with /SC ONLOGON /RL HIGHEST and
#       MultipleInstances = IgnoreNew
#     * makes ollama autostart too (GPN-Ollama) without duplicating an
#       autostart entry that already exists
#     * generates the launcher with the canonical env (BASE_PATH, AP_URL,
#       3 bootnodes, GUI=1, NODE_KEY_FILE) and Tee-Object -> node.log
#     * waits for ollama /api/tags and stops with an install command when
#       ollama is missing
#     * -Uninstall / --remove-autostart removes its own tasks only
#     * the generated launcher passes the same identity rules as the release
#       launcher (no dev URI, no pinned key/account, per-machine key file)
#     * -WhatIf machinery exists; executed for real when pwsh is present
#   Linux    scripts/gpn-autostart.sh
#     * writes gpn-node.service + gpn-ollama.service, enables them, and
#       enables linger (units alone only start at LOGIN, not at boot)
#     * idempotent: install twice -> still exactly two unit files, one marker
#     * remove deletes only what it created and keeps chain data / keystore
#   bundle   install.ps1 / install.sh / uninstall helpers are packaged by the
#            CI bundlers, and README-FIRST-RUN.txt names the files actually
#            shipped (a README that lies is a defect of its own).
#
# Usage: bash scripts/tests/test-autostart-installers.sh
set -uo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail=0
ok()  { printf '  ok   %s\n' "$*"; }
bad() { printf '  FAIL %s\n' "$*"; fail=1; }
skip(){ printf '  skip %s\n' "$*"; }

# assert_has FILE NEEDLE LABEL
assert_has() {
  if grep -Fq -- "$2" "$1"; then ok "$3"; else bad "$3 (missing: $2)"; fi
}
assert_lacks() {
  if grep -Fq -- "$2" "$1"; then bad "$3 (unexpected: $2)"; else ok "$3"; fi
}
assert_file() {
  if [ -s "$1" ]; then ok "$2"; else bad "$2 (missing/empty: $1)"; fi
}

ps1="$repo/install.ps1"
cmd="$repo/install.cmd"
unps="$repo/uninstall.ps1"

echo "== Windows installer: install.ps1 =="

# ---- task registration ----------------------------------------------------
assert_has "$ps1" "'/SC'"                    "registers with /SC"
assert_has "$ps1" "'ONLOGON'"                "starts at logon (ONLOGON)"
assert_has "$ps1" "'/RL'"                    "registers with /RL"
assert_has "$ps1" "'HIGHEST'"                "requests HIGHEST run level"
assert_has "$ps1" "MultipleInstances = 'IgnoreNew'" "sets MultipleInstances=IgnoreNew"
assert_has "$ps1" "IgnoreNew"                "single-instance policy present"
assert_has "$ps1" "\$NodeTask  = 'GPN-Node'" "task name is GPN-Node"
assert_has "$ps1" "'/Run'"                   "starts the task after registering"
assert_has "$ps1" "'/End'"                   "stops the task on uninstall"
assert_has "$ps1" "'/Delete'"                "deletes the task on uninstall"

# ---- ollama autostart (a node without ollama refuses to start) ------------
assert_has "$ps1" "\$OllamaTask = 'GPN-Ollama'"       "own ollama autostart entry"
assert_has "$ps1" "function Find-ExistingOllamaAutostart" "detects an existing ollama autostart"
assert_has "$ps1" "not adding a duplicate entry"       "never duplicates an existing ollama entry"
assert_has "$ps1" "cmd /c ollama serve"                "ollama autostart runs 'ollama serve'"

# ---- launcher contract ----------------------------------------------------
assert_has "$ps1" "Tee-Object -FilePath"     "supervisor output is teed to a log"
assert_has "$ps1" 'set "GPN_BASE_PATH='    "launcher pins GPN_BASE_PATH"
assert_has "$ps1" 'set "GPN_AP_URL='       "launcher pins GPN_AP_URL"
assert_has "$ps1" 'set "GPN_BOOTNODES='    "launcher pins GPN_BOOTNODES"
assert_has "$ps1" 'set "GPN_GUI=1"'       "launcher enables the dashboard"
assert_has "$ps1" 'set "GPN_NODE_KEY_FILE=' "launcher uses a per-machine node key FILE"
assert_has "$ps1" "--ensure-wallet"          "launcher provides the money identity"
assert_has "$ps1" "GPN_SIGNER_FILE=%GPN_BASE_PATH%" "launcher points at the on-disk keystore"

# canonical values, not placeholders left behind
assert_has "$ps1" "http://gpn-testnet.duckdns.org:8092" "canonical AP default"
boots=$(grep -o "/p2p/12D3KooW" "$ps1" | wc -l)
if [ "${boots:-0}" -ge 3 ]; then ok "canonical 3 bootstrap peers"; else bad "expected 3 bootnode multiaddrs (found ${boots:-0})"; fi

# ---- ollama is a hard prerequisite ---------------------------------------
assert_has "$ps1" "http://127.0.0.1:11434/api/tags" "waits on ollama /api/tags"
assert_has "$ps1" "function Wait-Ollama"            "has an explicit ollama wait loop"
assert_has "$ps1" "ollama is missing -- installing it"   "installs ollama instead of exiting on a fresh machine"
assert_has "$ps1" "install-ollama.ps1"                   "delegates to the ollama installer"
assert_has "$ps1" "OllamaSetup.exe"                      "has a direct-install fallback (winget absent)"
assert_has "$ps1" "Programs\\Ollama\\ollama.exe"      "refreshes PATH for a per-user ollama install"
assert_has "$ps1" "no-?ollama-?install"                  "documents the -NoOllamaInstall opt-out"
assert_not_has "$ps1" "Install it, then run this installer again:[^\n]*\n[^\n]*winget install[^\n]*\n[^\n]*Nothing was changed" \
  "no longer dead-ends the user on a missing ollama"
assert_has "$ps1" "winget install --id Ollama.Ollama -e" "prints the install command when ollama is missing"
assert_has "$ps1" "REFUSE TO START"                 "explains why it refuses to continue without ollama"

# ---- the message the operator must see -----------------------------------
assert_has "$ps1" "Your node will start automatically when you log in." "prints the autostart confirmation"
assert_has "$ps1" "\$Dashboard = 'http://127.0.0.1:8099'" "prints the dashboard URL"
assert_has "$ps1" "payout address:" "prints the payout address"

# ---- uninstall -----------------------------------------------------------
assert_has "$ps1" "(uninstall|remove-autostart)" "accepts -Uninstall / --remove-autostart"
assert_has "$ps1" "left in place: ollama's own autostart" "leaves ollama's own entry alone"
assert_has "$ps1" "are NOT touched"                      "uninstall leaves data/wallet untouched"

# ---- WhatIf --------------------------------------------------------------
assert_has "$ps1" "function Invoke-Step"  "every mutation goes through one gate (WhatIf)"
assert_has "$ps1" "[whatif]"              "WhatIf prints planned actions"
assert_has "$ps1" "if (\$WhatIf)"         "WhatIf short-circuits mutations"

echo "== Windows installer: launcher identity rules =="
# Extract the single-quoted here-string that becomes C:\gpn\start-node.cmd.
awk "/^\\\$LauncherTemplate = @'/{f=1;next} f && /^'@\$/{exit} f" "$ps1" > "$tmp/launcher.cmd"
assert_file "$tmp/launcher.cmd" "launcher template extracted"

# Sets CLA_PROBLEMS (empty = safe). Deliberately does NOT call ok/bad so it
# can also be used as a negative control.
check_launcher() { # $1=file ; mirrors Assert-LauncherSafe
  local f="$1"
  CLA_PROBLEMS=()
  grep -q 'GPN_NODE_KEY_FILE=' "$f" || CLA_PROBLEMS+=("no GPN_NODE_KEY_FILE")
  grep -Eq '(^|[^0-9a-fA-F])[0-9a-fA-F]{64}([^0-9a-fA-F]|$)' "$f" && CLA_PROBLEMS+=("literal 64-hex key")
  grep -Eq 'GPN_NODE_KEY=[0-9a-fA-F]' "$f" && CLA_PROBLEMS+=("assigns GPN_NODE_KEY")
  grep -Eqi '//(alice|bob|charlie|dave|eve|ferdie)' "$f" && CLA_PROBLEMS+=("dev URI (//Bob etc.)")
  grep -Eqi 'GPN_AUTHOR' "$f" && CLA_PROBLEMS+=("sets GPN_AUTHOR")
  grep -Eq '(GPN_SIGNER|GPN_ACCOUNT)="?//?' "$f" && CLA_PROBLEMS+=("pins a signer/account")
  grep -Eq '\b5[1-9A-HJ-NP-Za-km-z]{47}\b' "$f" && CLA_PROBLEMS+=("hard-coded SS58 account")
  grep -q -- '--ensure-wallet' "$f" || CLA_PROBLEMS+=("no --ensure-wallet")
  grep -q 'GPN_SIGNER_FILE=' "$f" || CLA_PROBLEMS+=("no GPN_SIGNER_FILE")
  grep -q 'GPN_AP_URL=' "$f" || CLA_PROBLEMS+=("no GPN_AP_URL")
}
check_launcher "$tmp/launcher.cmd"
if [ ${#CLA_PROBLEMS[@]} -eq 0 ]; then ok "generated start-node.cmd is safe to publish"; else
  bad "generated start-node.cmd: ${CLA_PROBLEMS[*]}"; fi

# Negative control: the checker must reject a shared-identity launcher,
# otherwise the assertion above is worthless.
cat > "$tmp/legacy.cmd" <<'EOF'
set "GPN_SIGNER=//Bob"
set "GPN_AUTHOR=bob"
set "GPN_ACCOUNT=5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty"
set "GPN_NODE_KEY_FILE=%USERPROFILE%\.gpn\node-key"
EOF
check_launcher "$tmp/legacy.cmd"
if [ ${#CLA_PROBLEMS[@]} -eq 0 ]; then
  bad "negative control PASSED the launcher check — the check is broken"
else
  ok "negative control rejected as expected (${#CLA_PROBLEMS[@]} problems)"
fi

echo "== Windows installer: wrapper + uninstall =="
assert_has "$cmd" "-ExecutionPolicy Bypass" "install.cmd bypasses the execution policy"
assert_has "$cmd" "install.ps1\" %*"        "install.cmd forwards all arguments"
assert_has "$unps" "-Uninstall"             "uninstall.ps1 delegates to -Uninstall"

echo "== Windows installer: -WhatIf executed (optional) =="
if command -v pwsh >/dev/null 2>&1; then
  # Real parser, not a text heuristic: a syntax error in install.ps1 would
  # otherwise only surface on the user's Windows box.
  for f in "$ps1" "$unps"; do
    if pwsh -NoProfile -Command "\$e=\$null; [void][System.Management.Automation.Language.Parser]::ParseFile('$f',[ref]\$null,[ref]\$e); if (\$e.Count) { \$e | ForEach-Object { Write-Host \$_.Message }; exit 1 }" 2>&1; then
      ok "PowerShell parses $(basename "$f")"
    else
      bad "PowerShell could not parse $(basename "$f")"
    fi
  done
  mkdir -p "$tmp/wi" && cd "$tmp/wi" || exit 1
  # No binaries next to the script -> it must stop before mutating anything,
  # and -WhatIf must print the planned steps first.
  out=$(pwsh -NoProfile -File "$ps1" -WhatIf -InstallDir "$tmp/wi/inst" 2>&1 || true)
  cd "$repo" || exit 1
  if printf '%s' "$out" | grep -q '\[whatif\]'; then ok "-WhatIf prints planned actions"; else
    bad "-WhatIf printed no planned actions"; fi
  if [ -d "$tmp/wi/inst" ]; then bad "-WhatIf created $tmp/wi/inst (must not mutate)"; else
    ok "-WhatIf created nothing"; fi
else
  skip "pwsh not installed — -WhatIf not executed (text assertions above still cover it)"
fi

echo "== Linux: scripts/gpn-autostart.sh =="
helper="$repo/scripts/gpn-autostart.sh"
assert_file "$helper" "autostart helper present"

# Fake systemctl/loginctl: record the commands the script intends to run.
stub="$tmp/stub"; mkdir -p "$stub"
cat > "$stub/systemctl" <<'EOF'
#!/bin/sh
echo "systemctl $*" >> "$GPN_TEST_LOG"
exit 0
EOF
cat > "$stub/loginctl" <<'EOF'
#!/bin/sh
echo "loginctl $*" >> "$GPN_TEST_LOG"
exit 0
EOF
chmod +x "$stub/systemctl" "$stub/loginctl"

home="$tmp/home"; mkdir -p "$home/.gpn/node" "$home/.gpn/ollama/bin" "$home/.local/bin"
echo '{"secret":"keep me"}' > "$home/.gpn/node/keystore.json"
printf '#!/bin/sh\n' > "$home/.gpn/ollama/bin/ollama"; chmod +x "$home/.gpn/ollama/bin/ollama"
# install.sh symlinks the supervisor here before it calls the helper.
printf '#!/bin/sh\n' > "$home/.local/bin/gpn_node"; chmod +x "$home/.local/bin/gpn_node"
export GPN_TEST_LOG="$tmp/calls.log"; : > "$GPN_TEST_LOG"

run_helper() {
  HOME="$home" PATH="$stub:$PATH" GPN_TEST_LOG="$GPN_TEST_LOG" \
    bash "$helper" "$@" --prefix "$home/.gpn" --bin-dir "$home/.local/bin" >>"$tmp/out.log" 2>&1
}

units="$home/.config/systemd/user"

run_helper install && ok "install exits 0" || bad "install failed (see $tmp/out.log)"
assert_file "$units/gpn-node.service" "gpn-node.service written"
assert_file "$units/gpn-ollama.service" "gpn-ollama.service written"
assert_file "$home/.gpn/autostart" "autostart marker written"

assert_has "$units/gpn-node.service" "ExecStart=$home/.local/bin/gpn_node" "node unit runs gpn_node"
assert_has "$units/gpn-node.service" "Environment=GPN_GUI=1" "node unit enables the dashboard"
assert_has "$units/gpn-node.service" "Environment=GPN_AP_URL=http://gpn-testnet.duckdns.org:8092" "node unit pins the canonical AP"
assert_has "$units/gpn-node.service" "Environment=GPN_BASE_PATH=$home/.gpn/node" "node unit pins the base path"
assert_has "$units/gpn-node.service" "WantedBy=default.target" "node unit installs into default.target"
assert_has "$units/gpn-node.service" "Restart=always" "node unit restarts on failure"
assert_has "$units/gpn-ollama.service" "ExecStart=$home/.gpn/ollama/bin/ollama serve" "ollama unit runs ollama serve"
assert_has "$units/gpn-ollama.service" "WantedBy=default.target" "ollama unit installs into default.target"

assert_has "$GPN_TEST_LOG" "--user enable gpn-node.service" "enables the node unit"
assert_has "$GPN_TEST_LOG" "--user enable gpn-ollama.service" "enables the ollama unit"
assert_has "$GPN_TEST_LOG" "--user daemon-reload" "reloads the user manager"
assert_has "$GPN_TEST_LOG" "enable-linger" "enables linger (boot without login)"

# ---- idempotency: run it again, twice the work must not mean twice the units
: > "$GPN_TEST_LOG"
run_helper install || bad "second install failed"
count=$(find "$units" -name 'gpn-*.service' | wc -l)
[ "$count" -eq 2 ] && ok "install twice -> still 2 unit files" || bad "install twice -> $count unit files"
marker_units=$(grep -c '^units=' "$home/.gpn/autostart" || true)
[ "$marker_units" -eq 1 ] && ok "one marker, one entry" || bad "marker has $marker_units 'units=' lines"

# ---- no ollama installed: node unit only, and it says why ------------------
home2="$tmp/home2"; mkdir -p "$home2/.gpn" "$home2/.local/bin"
printf '#!/bin/sh\n' > "$home2/.local/bin/gpn_node"; chmod +x "$home2/.local/bin/gpn_node"
HOME="$home2" PATH="$stub:$PATH" GPN_TEST_LOG="$GPN_TEST_LOG" \
  bash "$helper" install --prefix "$home2/.gpn" --bin-dir "$home2/.local/bin" >>"$tmp/out2.log" 2>&1
if [ -f "$home2/.config/systemd/user/gpn-node.service" ]; then ok "node unit still installed without ollama"; else
  bad "node unit missing when ollama is absent"; fi
if [ -f "$home2/.config/systemd/user/gpn-ollama.service" ]; then bad "ollama unit written with no ollama binary"; else
  ok "no ollama unit when ollama is absent"; fi
grep -q "REFUSES TO START without ollama" "$tmp/out2.log" && ok "warns that the node will refuse to start" \
  || bad "no warning about missing ollama"

# ---- a unit pointing at a missing binary must never be written -------------
home3="$tmp/home3"; mkdir -p "$home3"
HOME="$home3" PATH="$stub:$PATH" GPN_TEST_LOG="$GPN_TEST_LOG" \
  bash "$helper" install --prefix "$home3/.gpn" --bin-dir "$home3/nowhere" >>"$tmp/out3.log" 2>&1
if [ -f "$home3/.config/systemd/user/gpn-node.service" ]; then
  bad "wrote a unit for a binary that does not exist"
else
  ok "refuses to write a unit for a missing binary"
fi

# ---- remove: only what it created, data untouched --------------------------
: > "$GPN_TEST_LOG"
run_helper remove && ok "remove exits 0" || bad "remove failed"
[ -f "$units/gpn-node.service" ] && bad "remove left gpn-node.service" || ok "node unit removed"
[ -f "$units/gpn-ollama.service" ] && bad "remove left gpn-ollama.service" || ok "ollama unit removed"
[ -f "$home/.gpn/autostart" ] && bad "remove left the marker" || ok "marker removed"
assert_has "$GPN_TEST_LOG" "--user disable --now gpn-node.service" "disables the node unit"
assert_has "$GPN_TEST_LOG" "disable-linger" "disables linger"
[ -f "$home/.gpn/node/keystore.json" ] && ok "keystore untouched by remove" || bad "remove deleted the keystore!"
[ -d "$home/.gpn/ollama" ] && ok "ollama install untouched by remove" || bad "remove deleted the ollama install"

# ---- install.sh integration ----------------------------------------------
echo "== Linux: install.sh integration =="
assert_has "$repo/install.sh" "--no-autostart"                  "install.sh documents --no-autostart"
assert_has "$repo/install.sh" "gpn-autostart.sh"                "install.sh uses the autostart helper"
assert_has "$repo/install.sh" 'if [ "$AUTOSTART" = 1 ]'         "install.sh honours the autostart flag"
assert_has "$repo/install.sh" "uninstall.sh --remove-autostart" "install.sh prints the undo command"
assert_has "$repo/uninstall.sh" "--remove-autostart"            "uninstall.sh exposes --remove-autostart"
assert_has "$repo/uninstall.sh" "--status"                      "uninstall.sh can report status"

if bash "$repo/install.sh" --help 2>&1 | grep -q -- "--no-autostart"; then
  ok "install.sh --help lists --no-autostart"
else
  bad "install.sh --help does not list --no-autostart"
fi

# --no-autostart must not reach the helper: the call sits behind AUTOSTART=1.
assert_has "$repo/install.sh" 'if [ "$AUTOSTART" = 1 ] && [ -f "$AUTOSTART_HELPER" ]' \
  "autostart helper is only called when AUTOSTART=1"
if grep -q 'elif \[ "$AUTOSTART" = 1 \]' "$repo/install.sh"; then
  ok "missing helper is reported instead of silently skipped"
else
  bad "no branch for a missing autostart helper"
fi

# ---- bundle packaging ----------------------------------------------------
echo "== bundle packaging =="
for f in install.sh uninstall.sh install.ps1 install.cmd uninstall.ps1 scripts/gpn-autostart.sh; do
  if grep -Fq "$f" "$repo/scripts/ci/package-linux-bundle.sh"; then ok "linux bundler ships $f"; else
    bad "linux bundler does not ship $f"; fi
  if grep -Fq "$f" "$repo/.github/workflows/release-unix.yml"; then ok "unix release ships $f"; else
    bad "unix release does not ship $f"; fi
done
for f in install.ps1 install.cmd uninstall.ps1 install.sh uninstall.sh; do
  if grep -Fq "'$f'" "$repo/.github/workflows/release-windows.yml"; then ok "windows release ships $f"; else
    bad "windows release does not ship $f"; fi
done

# ---- README tells the truth ---------------------------------------------
echo "== README-FIRST-RUN.txt =="
readme="$repo/README-FIRST-RUN.txt"
for f in install.ps1 install.cmd uninstall.ps1 install.sh uninstall.sh \
         scripts/gpn-autostart.sh scripts/install-ollama.ps1 scripts/install-ollama.sh \
         scripts/install-ollama-amd.sh gpn-node.example.toml; do
  grep -Fq "$f" "$readme" && ok "README names $f" || bad "README does not name $f"
  [ -e "$repo/$f" ] && ok "README file exists: $f" || bad "README names a file that does not exist: $f"
done
if grep -qi "extract" "$readme" && grep -qi "install.ps1" "$readme" && grep -qi "install.sh" "$readme"; then
  ok "README's first run is extract -> install"
else
  bad "README no longer describes extract -> install"
fi
assert_has "$readme" "http://127.0.0.1:8099" "README states the dashboard URL"
assert_has "$readme" "enable-linger" "README explains Linux boot autostart"
assert_has "$repo/docs/run-a-node.md" "install.ps1" "run-a-node docs point at the installer"
assert_has "$repo/docs/run-a-node.md" "enable-linger" "run-a-node docs explain linger"

# ---- syntax ---------------------------------------------------------------
echo "== syntax =="
for f in "$repo/install.sh" "$repo/uninstall.sh" "$helper"; do
  if bash -n "$f" 2>"$tmp/syn.err"; then ok "bash -n $(basename "$f")"; else
    bad "bash -n $(basename "$f"): $(cat "$tmp/syn.err")"; fi
done

echo
if [ "$fail" -ne 0 ]; then echo "FAILED"; exit 1; fi
echo "all autostart/installer assertions passed"
