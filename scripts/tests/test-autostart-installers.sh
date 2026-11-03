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

# ---- a stop must explain itself and survive a reboot ----------------------
assert_has "$ps1" "report-exit.ps1"            "launcher calls a generated exit reporter"
assert_has "$ps1" 'set "RC=%ERRORLEVEL%"'      "launcher captures the supervisor's exit code"
assert_has "$ps1" "if errorlevel 3"             "launcher stops the restart loop on 3 strikes"
assert_has "$ps1" "goto :run"                   "launcher retries after a reported failure"
assert_has "$ps1" '[System.Windows.Forms.MessageBox]::Show' "a hidden task surfaces the reason in a message box"
assert_has "$ps1" "last-exit.json"              "the reporter reads the node's exit log"
assert_has "$ps1" "crash loop"                  "the reporter names a crash loop"
assert_has "$ps1" "NOT restarting"              "the reporter says it stopped restarting"
assert_has "$ps1" "ts_unix"                     "the 3-strike window uses unix timestamps"
assert_has "$ps1" "winget install --id Microsoft.VCRedist.2015+.x64 -e" "0xC0000135 is mapped to the VC++ fix"
assert_has "$ps1" "-1073741515"                 "the signed loader-failure code is recognised"
assert_has "$ps1" "3221225781"                  "the unsigned loader-failure code is recognised"
assert_has "$ps1" "function Verify-NodeTask"    "installer can verify autostart"
assert_has "$ps1" "function Wait-NodeUp"        "verification waits for the node to answer"
assert_has "$ps1" "schtasks /Run"               "verification runs the task"
assert_has "$ps1" "system_health"               "verification checks the RPC"
assert_has "$ps1" "'^-{1,2}(verify|v)\$'"       "installer accepts -Verify"

# canonical values, not placeholders left behind
assert_has "$ps1" "http://gpn-testnet.duckdns.org:8092" "canonical AP default"
boots=$(grep -o "/p2p/12D3KooW" "$ps1" | wc -l)
if [ "${boots:-0}" -ge 3 ]; then ok "canonical 3 bootstrap peers"; else bad "expected 3 bootnode multiaddrs (found ${boots:-0})"; fi

# ---- ollama is a hard prerequisite ---------------------------------------
assert_has "$ps1" "http://127.0.0.1:11434/api/tags" "waits on ollama /api/tags"
assert_has "$ps1" "function Wait-Ollama"            "has an explicit ollama wait loop"
assert_has "$ps1" "ollama is missing -- installing it"   "installs ollama instead of exiting on a fresh machine"
assert_has "$ps1" "Test-MissingVCRuntime"                "detects the missing Visual C++ runtime (0xC0000135)"
assert_has "$ps1" "Microsoft.VCRedist.2015+.x64"         "installs the VC++ redistributable"
assert_has "$ps1" "vc_redist.x64.exe"                    "has a direct redist fallback"
assert_has "$ps1" "exit %KEYRC%"                         "launcher reports the key-generation exit code"
assert_has "$ps1" "0xC0000135 = missing Visual C++"      "launcher names the DLL-not-found cause"
assert_has "$ps1" "install-ollama.ps1"                   "delegates to the ollama installer"
assert_has "$ps1" "OllamaSetup.exe"                      "has a direct-install fallback (winget absent)"
assert_has "$ps1" "Programs\\Ollama\\ollama.exe"      "refreshes PATH for a per-user ollama install"
assert_has "$ps1" "no-?ollama-?install"                  "documents the -NoOllamaInstall opt-out"
assert_has "$ps1" "installing from the archive"          "does not Copy-Item a file onto itself (Root == BinDir)"
assert_has "$ps1" "srcFull -ieq \$dstFull"               "compares resolved install dirs before copying"
# After winget reports success the process PATH is still stale; without a refresh
# the helper downloads the 1.5 GB installer a second time.
assert_has "$repo/scripts/install-ollama.ps1" "function Update-PathNow"          "helper refreshes PATH itself"
assert_has "$repo/scripts/install-ollama.ps1" "Programs"                        "helper probes the per-user ollama path"
assert_has "$repo/scripts/install-ollama.ps1" "disable-interactivity"            "winget runs unattended"
assert_has "$repo/scripts/install-ollama.ps1" "no progress bar"                  "warns that the download is silent"

# Windows PowerShell 5.1 reads .ps1 as ANSI unless there is a BOM: any non-ASCII
# byte (em dash, ellipsis, box drawing) can break string parsing with errors like
# "The string is missing the terminator". Keep every shipped script pure ASCII.
while IFS= read -r f; do
  if LC_ALL=C grep -q $'[^\x01-\x7F]' "$f"; then
    fail "$f contains non-ASCII bytes (breaks Windows PowerShell 5.1 parsing)"
  else
    ok "$(basename "$f") is ASCII-only"
  fi
done < <(find "$repo" -name '*.ps1' -not -path '*/target/*' -not -path '*/.git/*')
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
assert_has "$tmp/launcher.cmd" "__REPORT__"      "launcher leaves a slot for the reporter path"
assert_has "$tmp/launcher.cmd" "Tee-Object"      "launcher still tees to one log"

# The reporter is a separate generated script; extract and check it too.
awk "/^\\\$ReportTemplate = @'/{f=1;next} f && /^'@\$/{exit} f" "$ps1" > "$tmp/report-exit.ps1"
assert_file "$tmp/report-exit.ps1" "exit reporter template extracted"
assert_has "$tmp/report-exit.ps1" "param([int]\$ExitCode" "reporter takes the exit code"
assert_has "$tmp/report-exit.ps1" "MessageBox"           "reporter shows a message box"
assert_has "$tmp/report-exit.ps1" "Add-Content -Path \$Log" "reporter writes the same text to node.log"
assert_has "$tmp/report-exit.ps1" "ConvertFrom-Json"     "reporter parses the NDJSON exit log"
assert_has "$tmp/report-exit.ps1" ".Count -ge 3"        "reporter applies the 3-strike rule"

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

echo "== Windows installer: auto-update (GPN-Update) =="
# The Windows equivalent of the Linux gpn-update.timer. What is guarded here:
# a box that never reboots never updated, because the node only checked the
# signed manifest when the supervisor started.
assert_has "$ps1" "\$UpdateTask = 'GPN-Update'"            "task name is GPN-Update"
assert_has "$ps1" "update-and-restart.ps1"                  "writes the update wrapper"
assert_has "$ps1" "every 6h"                                "registers a 6-hour cadence"
assert_has "$ps1" "<Interval>PT6H</Interval>"               "trigger repeats every 6 hours"
assert_has "$ps1" "<Duration>P1D</Duration>"                "repetition covers the whole day"
assert_has "$ps1" "<RandomDelay>PT30M</RandomDelay>"        "trigger has +/- 30 min jitter"
assert_has "$ps1" "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>" "update task cannot run twice"
assert_has "$ps1" "the CLI cannot express jitter"           "explains why the task is registered from XML"
assert_has "$ps1" "'/XML'"                                  "registers through schtasks /XML"
assert_has "$ps1" "'/F'"                                    "re-registration replaces the task (idempotent)"
assert_has "$ps1" "-NoProfile -ExecutionPolicy Bypass -File" "the task action runs the wrapper without a profile"
assert_has "$ps1" ".PARAMETER UpdateTaskOnly"                "deploy step can register only the update task"
assert_has "$ps1" "update.lock"                              "the wrapper takes a lock (no concurrent updaters)"
assert_has "$ps1" "@(\$NodeTask, \$OllamaTask, \$UpdateTask)" "uninstall also removes GPN-Update"
assert_has "$ps1" "removed the auto-update wrapper"           "uninstall removes update-and-restart.ps1"
assert_has "$ps1" "-Note"                                     "update failures surface through report-exit.ps1"

# Extract the generated wrapper and assert its CONTROL FLOW by line order:
# stop before apply, always start after, and never start on "nothing to do".
awk "/^\\\$UpdateWrapperTemplate = @'/{f=1;next} f && /^'@\$/{exit} f" "$ps1" > "$tmp/update-and-restart.ps1"
assert_file "$tmp/update-and-restart.ps1" "update wrapper template extracted"
uw="$tmp/update-and-restart.ps1"
assert_has "$uw" "gpn_update.exe"                  "wrapper runs gpn_update"
assert_has "$uw" "--check"                         "wrapper checks before doing anything"
assert_has "$uw" "--apply"                         "wrapper applies the update"
assert_has "$uw" "schtasks /End /TN \$NodeTask"    "wrapper stops the node (Windows locks a running .exe)"
assert_has "$uw" "schtasks /Run /TN \$NodeTask"    "wrapper starts the node again"
assert_has "$uw" "Wait-NodeExit"                   "wrapper waits for the process to exit"
assert_has "$uw" "[System.IO.FileShare]::None"     "wrapper takes an exclusive lock"
assert_has "$uw" "release manifest unreachable"    "exit 2 is logged, not treated as a failure"
assert_has "$uw" "node NOT restarted"              "exit 0/2 do not restart the node"
already=$(grep -n -e '-ExecutionPolicy Bypass -File \$ReportScript' "$uw" | head -1 | cut -d: -f1)
if [ -z "$already" ]; then bad "wrapper never reports through report-exit.ps1"; else ok "wrapper reports failures through report-exit.ps1"; fi

line_end=$(grep -n 'schtasks /End /TN \$NodeTask' "$uw" | head -1 | cut -d: -f1)
line_apply=$(grep -n '\$updater --apply' "$uw" | head -1 | cut -d: -f1)
line_run=$(grep -n 'schtasks /Run /TN \$NodeTask' "$uw" | head -1 | cut -d: -f1)
line_fail=$(grep -n 'Show-UpdateFailure \$apply' "$uw" | head -1 | cut -d: -f1)
[ -n "$line_end" ] && [ -n "$line_apply" ] && [ "$line_end" -lt "$line_apply" ] \
  && ok "stops the node BEFORE --apply" || bad "--apply is not preceded by schtasks /End"
[ -n "$line_apply" ] && [ -n "$line_run" ] && [ "$line_apply" -lt "$line_run" ] \
  && ok "restarts AFTER --apply" || bad "the restart does not follow --apply"
[ -n "$line_run" ] && [ -n "$line_fail" ] && [ "$line_run" -lt "$line_fail" ] \
  && ok "restarts before showing a failure dialog (a modal box cannot keep the node down)" \
  || bad "a failure dialog can be shown before the node is restarted"

# The "nothing to do" branch must return before any stop/start.
awk '/if \(\$check -eq 0\)/{f=1} f{print} f && /^  \}/{exit}' "$uw" > "$tmp/uptodate.txt"
if grep -q 'exit 0' "$tmp/uptodate.txt"; then ok "exit 0 returns 0"; else bad "exit 0 does not return 0"; fi
if grep -qE 'schtasks /(End|Run)|--apply' "$tmp/uptodate.txt"; then
  bad "exit 0 path touches the node"; else ok "exit 0 does not restart the node"; fi
## exit 2 must also leave the node alone
awk '/if \(\$check -eq 2\)/{f=1} f{print} f && /^  \}/{exit}' "$uw" > "$tmp/unreachable.txt"
if grep -qE 'schtasks /(End|Run)|--apply' "$tmp/unreachable.txt"; then
  bad "exit 2 path touches the node"; else ok "exit 2 does not restart the node"; fi

if LC_ALL=C grep -q $'[^\x01-\x7F]' "$uw"; then
  bad "generated update-and-restart.ps1 contains non-ASCII bytes"; else ok "update wrapper is ASCII-only"; fi

# Negative control for the ordering checks: a wrapper that applies before it
# stops must be rejected, otherwise the assertions above prove nothing.
{ echo 'x=$updater --apply'; echo 'schtasks /End /TN $NodeTask'; } > "$tmp/bad-order.ps1"
if [ "$(grep -n 'schtasks /End /TN \$NodeTask' "$tmp/bad-order.ps1" | head -1 | cut -d: -f1)" -lt \
     "$(grep -n '\$updater --apply' "$tmp/bad-order.ps1" | head -1 | cut -d: -f1)" ]; then
  bad "negative control PASSED the stop-before-apply check - the check is broken"
else
  ok "negative control rejected as expected"
fi

echo "== Windows installer: wrapper + uninstall =="
assert_has "$cmd" "-ExecutionPolicy Bypass" "install.cmd bypasses the execution policy"
assert_has "$cmd" "install.ps1\" %*"        "install.cmd forwards all arguments"
assert_has "$unps" "-Uninstall"             "uninstall.ps1 delegates to -Uninstall"

echo "== Windows installer: -WhatIf executed (optional) =="
if command -v pwsh >/dev/null 2>&1; then
  # Real parser, not a text heuristic: a syntax error in install.ps1 would
  # otherwise only surface on the user's Windows box.
  for f in "$ps1" "$unps" "$tmp/report-exit.ps1"; do
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
case "$*" in
  *" is-enabled "*) echo enabled ;;
  *" is-active "*) echo active ;;
  *" stop "*) echo inactive > "$GPN_TEST_STATE" ;;
  *" start "*) echo active > "$GPN_TEST_STATE" ;;
esac
exit 0
EOF
cat > "$stub/loginctl" <<'EOF'
#!/bin/sh
echo "loginctl $*" >> "$GPN_TEST_LOG"
case "$*" in
  *show-user*) echo 'Linger=yes' ;;
esac
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
assert_file "$units/gpn-notify@.service" "failure notifier unit written"
assert_file "$home/.gpn/autostart" "autostart marker written"

assert_has "$units/gpn-node.service" "OnFailure=gpn-notify@%n.service" "node unit reports a fatal start"
assert_has "$units/gpn-ollama.service" "OnFailure=gpn-notify@%n.service" "ollama unit reports a fatal start"
assert_has "$units/gpn-notify@.service" "logger -t gpn-node" "notifier logs one clear line"
assert_has "$units/gpn-notify@.service" "notify-send" "notifier fires a desktop alert when one exists"
assert_has "$units/gpn-notify@.service" "XDG_SESSION_ID" "notifier is desktop-aware"
assert_has "$units/gpn-notify@.service" "|| true" "notifier never fails a headless box"

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
count=$(find "$units" -name 'gpn-node.service' -o -name 'gpn-ollama.service' | wc -l)
[ "$count" -eq 2 ] && ok "install twice -> still 2 service unit files" || bad "install twice -> $count service unit files"
count=$(find "$units" -name 'gpn-notify@.service' | wc -l)
[ "$count" -eq 1 ] && ok "one failure-notifier template" || bad "$count failure-notifier templates"

# ---- verify: enabled + linger + simulated reboot keeps the node up --------
: > "$GPN_TEST_LOG"
if HOME="$home" PATH="$stub:$PATH" GPN_TEST_LOG="$GPN_TEST_LOG" GPN_TEST_STATE="$tmp/state" \
   bash "$helper" verify --prefix "$home/.gpn" --bin-dir "$home/.local/bin" >"$tmp/verify.log" 2>&1; then
  ok "verify exits 0 when enabled + linger + reboot return the node"
else
  bad "verify failed: $(cat "$tmp/verify.log")"
fi
grep -q "PASSED" "$tmp/verify.log" && ok "verify says PASSED" || bad "verify did not report PASSED"
assert_has "$GPN_TEST_LOG" "--user is-enabled gpn-node.service" "verify observed the enabled state"
assert_has "$GPN_TEST_LOG" "--user stop gpn-node.service" "verify simulated a reboot (stop)"
assert_has "$GPN_TEST_LOG" "--user start gpn-node.service" "verify simulated a reboot (start)"
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
