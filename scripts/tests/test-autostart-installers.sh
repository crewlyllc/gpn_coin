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
# 2026-11-05 box 4: the launcher used to run
#   powershell -Command "& gpn_node.exe *>&1 | Tee-Object ..."
# and that wrapper exits 1 BEFORE the node starts whenever the launcher is
# spawned without a console (installer's Win32_Process.Create, scheduled task).
# The operator saw "exited 1 without writing a reason" while the node never ran.
assert_lacks "$ps1" "Tee-Object -FilePath"   "launcher does not wrap the node in a PowerShell pipeline"
assert_has "$ps1" '>> "__LOG__" 2>&1'        "launcher appends the node's own output to the log"
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
# 2026-11-04 box 1: Start-Process -WindowStyle Hidden inherits the installer's
# console; closing that window killed a 35-minute-old node with 0xC000013A.
assert_has "$ps1" "function Start-NodeDetached"                  "has a console-less launcher"
assert_has "$ps1" "Invoke-CimMethod -ClassName Win32_Process -MethodName Create" \
  "starts the node detached through Win32_Process.Create (no inherited console)"
assert_has "$ps1" "CommandLine      ="                          "CIM path passes a CommandLine"
assert_has "$ps1" "Win32_Process.Create returned"                 "reports a failed CIM launch"
assert_has "$ps1" "CIM/WMI is unavailable"                        "CIM failure falls back instead of aborting"
assert_has "$ps1" "Start-Process -FilePath \$Command"            "Start-Process fallback is still present"
assert_has "$ps1" "start node detached: \$NodeCmd"                "both direct-start sites use the detached launcher"
assert_has "$ps1" "-1073741510"                                  "the signed console-close code is recognised"
assert_has "$ps1" "3221225786"                                   "the unsigned console-close code is recognised"
assert_has "$ps1" "console-closed"                               "console close has its own exit kind"
assert_has "$ps1" "start it detached (the installer does this) or run it as a scheduled task" \
  "console close names the real fix"
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
# 2026-11-03 box 3: the VC++ check ran before $nodeExe was assigned, so it
# tested an empty path (Test-Path error) and silently did nothing.
assert_has "$ps1" "IsNullOrWhiteSpace(\$Exe)"                 "guards an empty exe path before Test-Path"
# 2026-11-03 box 3: re-running the installer as a normal user hit a GPN-Node task
# created earlier by Administrator -> schtasks rc=1 "Access is denied" with no
# explanation. It must name the owner and the fix.
assert_has "$ps1" "left it alone and"                      "explains when GPN-Node belongs to another account"
# 2026-11-04 box 1: task owned by 'Jeremy', install as 'Jeremy Guo'. Deleting it
# needs admin, so the installer must fall back to a Startup shortcut, not abort.
assert_has "$ps1" "registered a Startup shortcut instead"   "foreign-owned task falls back to Startup"
# 2026-11-04: the install must END with a running node even when Task Scheduler is
# unavailable, and must not claim success it did not achieve.
assert_has "$ps1" "no scheduled task -- starting the node directly" "starts the node when only a Startup shortcut exists"
assert_has "$ps1" "starting the node directly"                     "starts directly when schtasks /Run is denied"
assert_has "$ps1" "NodeAutostart"                                  "tracks which autostart mechanism actually worked"
assert_lacks "$ps1" 'Say "scheduled task \$NodeTask registered'  "never claims a task succeeded before checking"
# /RL HIGHEST needs elevation; from a normal shell it fails rc=1 even with no
# existing task. The node needs no admin rights, so the installer must retry
# without it rather than abort.
assert_has "$ps1" "retrying at normal privilege"         "falls back when /RL HIGHEST is refused"
# 2026-11-04: a standard Windows account gets "Access is denied" from
# schtasks /Create at all. Reuse an existing launcher task, and fall back to a
# Startup shortcut when the Task Scheduler refuses.
assert_has "$ps1" "reusing it (not recreating)"            "reuses an existing GPN-Node task"
assert_has "$ps1" "using a Startup shortcut instead"       "falls back to a Startup shortcut when denied"
# 2026-11-04 tried baking the absolute key path at install time; 2026-11-05 box 4
# proved that wrong: installed by one account, run by another -> the baked path
# pointed into the wrong profile and the launcher printed "refusing to start a
# node with a shared/default identity". The path is resolved at RUNTIME now, with
# the install-time path kept only as a fallback for machines that already have a
# key there.
assert_has "$ps1" "__KEYFILE__"                            "launcher template still carries the install-time fallback"
assert_has "$ps1" "Replace('__KEYFILE__'"                  "install substitutes the fallback path"
assert_has "$ps1" 'set "GPN_NODE_KEY_FILE=%USERPROFILE%'    "launcher resolves the key path from %USERPROFILE% at runtime"
assert_has "$ps1" "GetFolderPath('Startup')"               "writes the shortcut to the user Startup folder"
# The update task must not require elevation either, and the deploy path must
# never fail the release build when a task cannot be registered.
assert_has "$ps1" "<RunLevel>LeastPrivilege</RunLevel>"    "update task runs at least privilege"
assert_lacks "$ps1" "<RunLevel>HighestAvailable</RunLevel>" "update task never requests elevation"
assert_has "$ps1" "the release is unaffected"              "deploy mode warns instead of failing the build"
assert_has "$ps1" "-RunHighest"                          "elevated task creation is opt-in, not the default"
assert_lacks "$ps1" "'/Create', '/TN', \$NodeTask, '/TR', "cmd /c \$NodeCmd", '/SC', 'ONLOGON', '/RL'" \
  "the default create does NOT request elevation"
assert_has "$ps1" "schtasks /Delete /TN \$NodeTask /F"    "gives the exact admin command to clear it"
python3 - "$ps1" <<'CHECK'
import sys, re
s = open(sys.argv[1]).read()
def pos(pat):
    m = re.search(pat, s)
    return m.start() if m else -1
defs = pos(r"\$nodeExe = Join-Path \$BinDir 'gpn-node.exe'")
use  = pos(r"Test-MissingVCRuntime \$nodeExe")
print("ok   exe path is defined before the VC++ check" if 0 <= defs < use else
      "FAIL exe path is defined AFTER the VC++ check (empty Test-Path)")
sys.exit(0 if 0 <= defs < use else 1)
CHECK
assert_has "$ps1" "Microsoft.VCRedist.2015+.x64"         "installs the VC++ redistributable"
assert_has "$ps1" "vc_redist.x64.exe"                    "has a direct redist fallback"
# 2026-11-06: the launcher no longer decides "is the key there?" with cmd's
# `if not exist` -- that probe reported a present, readable, unlocked key as
# MISSING on box 2 and produced three wrong "identity" diagnoses. The binary
# answers instead, because it can open the file and read the real OS error.
assert_has   "$ps1" "--ensure-node-key"                   "launcher asks the binary whether the key is usable"
assert_has   "$ps1" 'if not "%KEYRC%"=="0"'               "launcher stops when the binary cannot supply a key"
assert_lacks "$ps1" 'if not exist "%GPN_NODE_KEY_FILE%" (' "launcher does not probe the key with if-not-exist"
assert_lacks "$ps1" "refusing to start a node with a shared/default identity" "the misleading identity message is gone"
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
assert_lacks "$ps1" "Install it, then run this installer again:[^\n]*\n[^\n]*winget install[^\n]*\n[^\n]*Nothing was changed" \
  "no longer dead-ends the user on a missing ollama"
assert_has "$ps1" "winget install --id Ollama.Ollama -e" "prints the install command when ollama is missing"
assert_has "$ps1" "REFUSE TO START"                 "explains why it refuses to continue without ollama"

# ---- the message the operator must see -----------------------------------
assert_has "$ps1" "Autostart: scheduled task"                  "prints the autostart confirmation (task)"
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
assert_lacks "$tmp/launcher.cmd" "Tee-Object"    "launcher execs the node directly (no PowerShell wrapper)"
assert_has "$tmp/launcher.cmd" '>> "__LOG__" 2>&1' "node output goes straight to node.log"

# The reporter is a separate generated script; extract and check it too.
awk "/^\\\$ReportTemplate = @'/{f=1;next} f && /^'@\$/{exit} f" "$ps1" > "$tmp/report-exit.ps1"
assert_file "$tmp/report-exit.ps1" "exit reporter template extracted"
assert_has "$tmp/report-exit.ps1" "param([int]\$ExitCode" "reporter takes the exit code"
assert_has "$tmp/report-exit.ps1" "MessageBox"           "reporter shows a message box"
assert_has "$tmp/report-exit.ps1" "Add-Content -Path \$Log" "reporter writes the same text to node.log"
assert_has "$tmp/report-exit.ps1" "ConvertFrom-Json"     "reporter parses the NDJSON exit log"
assert_has "$tmp/report-exit.ps1" ".Count -ge 3"        "reporter applies the 3-strike rule"
# A record written by a dying supervisor may not carry the supervisor's own exit
# code; a recent record is still the reason.
assert_has "$tmp/report-exit.ps1" "-le 600"             "reporter accepts a recent record whose code differs"
# Never leave the dialog at "unknown": show what the log last said.
assert_has "$tmp/report-exit.ps1" "Get-Content -Path \$Log -Tail 5" "no-record path prints the last 5 log lines"
assert_has "$tmp/report-exit.ps1" "last log lines:"      "the log tail is labelled"
assert_has "$tmp/report-exit.ps1" "Substring(0, 200)"    "log lines are truncated, not dumped"
assert_has "$tmp/report-exit.ps1" "console-closed"       "reporter maps a console close itself"

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
assert_has "$ps1" "every hour"                             "registers the hourly review cadence (#161)"
assert_has "$ps1" "<Interval>PT1H</Interval>"              "trigger repeats every hour"
assert_has "$ps1" "<Duration>P1D</Duration>"                "repetition covers the whole day"
assert_has "$ps1" "<RandomDelay>PT10M</RandomDelay>"       "trigger has +/- 10 min jitter"
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

# ---- the update hook must cover every unit that runs installed code ---------
# 2026-11-12: it named only the supervisor, so on a host whose provider stack is
# a set of sibling units every update left them executing the deleted previous
# image. A file on disk and the code a process runs are two different facts.
echo "== Linux: update hook covers the whole stack =="
hookhome="$tmp/hookhome"; hookunits="$hookhome/.config/systemd/user"
mkdir -p "$hookunits" "$hookhome/.gpn/fleet" "$hookhome/.local/bin"
for b in gpn_node gpn_update gpn_scheduler gpn_provider_status; do
  printf '#!/bin/sh\n' > "$hookhome/.local/bin/$b"; chmod +x "$hookhome/.local/bin/$b"
done
# the supervisor runs a launcher script under the prefix; two siblings run
# shipped binaries directly; one unit runs something unrelated; one is an alias.
printf '[Service]\nExecStart=%s/.gpn/fleet/run-fleet.sh\n' "$hookhome" > "$hookunits/gpn-fleet.service"
printf '[Service]\nExecStart=%s/.local/bin/gpn_scheduler --listen 0.0.0.0:8092\n' "$hookhome" > "$hookunits/gpn-accesspoint.service"
ln -sf "$hookunits/gpn-accesspoint.service" "$hookunits/gpn-scheduler.service"
printf '[Service]\nExecStart=%s/.local/bin/gpn_provider_status\n' "$hookhome" > "$hookunits/gpn-provider-heartbeat.service"
# the oracle shape: shell-wrapped, home-relative binary path (%h specifier)
printf "[Service]\nExecStart=/bin/sh -c 'exec %%h/.local/bin/gpn_operate_oracle ws://127.0.0.1:9944 %%h/x.toml 1800 \"\$SIGNER\"'\n" > "$hookunits/gpn-oracle.service"
printf '[Service]\nExecStart=/usr/bin/ollama serve\n' > "$hookunits/gpn-ollama.service"
# an INACTIVE standalone node: `restart` here would start a second supervisor
printf '[Service]\nExecStart=%s/.local/bin/gpn_node --alice --force-authoring\n' "$hookhome" > "$hookunits/gpn-node.service"
HOME="$hookhome" PATH="$stub:$PATH" GPN_TEST_LOG="$GPN_TEST_LOG" \
  bash "$helper" install-updater --prefix "$hookhome/.gpn" --bin-dir "$hookhome/.local/bin" \
  >>"$tmp/hook.log" 2>&1 && ok "install-updater exits 0" || bad "install-updater failed: $(cat "$tmp/hook.log")"
upd="$hookunits/gpn-update.service"
assert_file "$upd" "gpn-update.service written by install-updater"
assert_has "$upd" "GPN_UPDATE_RESTART_CMD=systemctl --user try-restart" "hook uses try-restart, never restart"
for u in gpn-fleet.service gpn-accesspoint.service gpn-provider-heartbeat.service gpn-oracle.service; do
  grep -Fq "$u" "$upd" && ok "hook names $u" \
    || bad "hook does not name $u (an update would leave it on the old image)"
done
assert_lacks "$upd" "gpn-ollama.service"  "hook leaves units outside bin dir/prefix alone"
assert_lacks "$upd" "gpn-scheduler.service" "hook skips the gpn-scheduler alias (same unit twice)"
assert_lacks "$upd" "restart gpn-update" "hook never restarts itself"
grep -Fq " ; " "$upd" && bad "hook still chains two commands with ;" || ok "hook is a single systemctl call"
# a stopped unit must not be started by the hook (try-restart only restarts ACTIVE units)
grep -Fq "systemctl --user restart " "$upd" && bad "hook contains a plain restart" \
  || ok "hook contains no plain restart"
# the duplicate-node guard: fleet and node share one chain DB, and try-restart
# was empirically able to start a disabled+inactive gpn-node.service on
# 2026-11-12, which took the RocksDB lock and killed the real node.
if grep -Fq "gpn-node.service" "$upd"; then
  bad "hook names gpn-node.service while gpn-fleet.service exists (shared chain DB)"
else
  ok "hook never touches the duplicate gpn-node.service when the fleet exists"
fi
# fault injection: siblings whose ExecStart is elsewhere drop out of the hook.
printf '[Service]\nExecStart=/usr/bin/true\n' > "$hookunits/gpn-accesspoint.service"
printf '[Service]\nExecStart=/usr/bin/true\n' > "$hookunits/gpn-fleet.service"
HOME="$hookhome" PATH="$stub:$PATH" GPN_TEST_LOG="$GPN_TEST_LOG" \
  bash "$helper" install-updater --prefix "$hookhome/.gpn" --bin-dir "$hookhome/.local/bin" \
  >>"$tmp/hook.log" 2>&1
if grep -Fq "gpn-accesspoint.service" "$upd" || grep -Fq "gpn-fleet.service" "$upd"; then
  bad "hook names a unit that does not run installed code"
else
  ok "hook only names units that run bin-dir/prefix code"
fi
grep -Fq "gpn-provider-heartbeat.service" "$upd" && ok "hook keeps the sibling that still qualifies" \
  || bad "hook dropped a qualifying sibling"
# a plain single-node host has no fleet unit, so gpn-node.service IS the stack
rm -f "$hookunits/gpn-fleet.service"
printf '[Service]\nExecStart=%s/.local/bin/gpn_node --chain testnet\n' "$hookhome" > "$hookunits/gpn-node.service"
HOME="$hookhome" PATH="$stub:$PATH" GPN_TEST_LOG="$GPN_TEST_LOG" \
  bash "$helper" install-updater --prefix "$hookhome/.gpn" --bin-dir "$hookhome/.local/bin" \
  >>"$tmp/hook.log" 2>&1
grep -Fq "try-restart gpn-node.service" "$upd" && ok "single-node host: the node unit is restarted" \
  || bad "single-node host: the node unit was left on the old image"

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
# ---- access point / scheduler unit ----------------------------------------
# 2026-11-08 (W52): the access point's binary IS `gpn_scheduler`, and its unit is
# `gpn-accesspoint.service` — which is why a name-based sweep kept reporting
# "gpn-scheduler has no service, the scheduler runs as a one-off process". It never
# did. The alias makes the documented name resolve to the SAME unit; a second unit
# with its own ExecStart would bind :8092 twice and crash-loop. Guard both halves.
echo "== Linux: install-accesspoint.sh unit =="
ap="$repo/install-accesspoint.sh"
assert_file "$ap" "install-accesspoint.sh is present"
assert_has "$ap" "Alias=gpn-scheduler.service" "AP unit aliases the documented name gpn-scheduler"
assert_has "$ap" "Restart=always" "AP unit is supervised (Restart=always)"
assert_has "$ap" "WantedBy=default.target" "AP unit is enabled for login/boot"
assert_lacks "$ap" "ExecStart=\$BIN_DIR/gpn_scheduler" "no second unit starts gpn_scheduler directly"

# ---- syntax ---------------------------------------------------------------
echo "== syntax =="
for f in "$repo/install.sh" "$repo/uninstall.sh" "$helper" "$ap"; do
  if bash -n "$f" 2>"$tmp/syn.err"; then ok "bash -n $(basename "$f")"; else
    bad "bash -n $(basename "$f"): $(cat "$tmp/syn.err")"; fi
done

echo
if [ "$fail" -ne 0 ]; then echo "FAILED"; exit 1; fi
echo "all autostart/installer assertions passed"
