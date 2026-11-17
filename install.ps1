#Requires -Version 5.1
<#
.SYNOPSIS
  GPN one-command installer for Windows: makes a downloaded node survive a reboot.

.DESCRIPTION
  The release zip ships binaries. Extracting it and running gpn_node.exe starts
  one node -- until you close the window or reboot. Nothing restarts it, and
  `gpn_node` REFUSES TO START without ollama (exit 69, node=down(no ollama)),
  so ollama has to come back too.

  This script:
    1. copies the binaries to C:\gpn\bin (the extracted folder can be deleted)
    2. creates C:\gpn\start-node.cmd (all env in one place, one log file) and
       C:\gpn\hidden-launch.vbs (wscript host: the node runs with NO console
       window, so there is no window whose close could kill it)
    3. makes sure ollama is installed AND serving (it is a hard prerequisite)
    4. registers the scheduled task  GPN-Node   (/SC ONLOGON, runs
       wscript.exe hidden-launch.vbs, MultipleInstances=IgnoreNew) -- the node
       starts hidden when you log in
    5. makes ollama autostart as well (task GPN-Ollama, or reuses the existing
       ollama autostart entry instead of adding a duplicate)
    6. starts the node now
    7. registers the scheduled task  GPN-Update  -- the Windows equivalent of
       the Linux gpn-update.timer: every hour (+/- 10 min jitter) it checks
#       (TEMPORARY hourly cadence for the review window, 2026-11-12; reverts
#       with DEFAULT_CHECK_HOURS in gpn-agent/src/update.rs)
       the signed release manifest and, when a newer release exists, STOPS the
       node, applies the update and STARTS it again. Without it a Windows box
       only checked for updates when the supervisor started, so a machine that
       never reboots never updates (that is how box 1 sat on an old build).

  Running it twice is safe: tasks are recreated by name (/F), never duplicated.

.PARAMETER Uninstall
  Remove the autostart: stop + delete GPN-Node and GPN-Update, delete
  GPN-Ollama only if this script created it, and remove the generated
  update-and-restart.ps1. Chain data, update.log and the wallet are NEVER
  touched.

.PARAMETER UpdateTaskOnly
  Register only GPN-Update (and write update-and-restart.ps1). Used by the
  release deploy step so a box installed before this existed heals itself
  without re-copying binaries, re-installing ollama or restarting the node.

.PARAMETER WhatIf
  Print every action that would be taken, change nothing. Used by CI to test
  the install logic on Linux (where there is no Task Scheduler).

.PARAMETER NoStart
  Register the autostart but do not launch the node now.

.PARAMETER NoOllamaInstall
  Fail instead of installing ollama when it is missing. By default the
  installer installs ollama (winget, else the official installer), starts it,
  and pulls the baseline models -- a fresh machine has no ollama, and
  gpn_node exits 69 without it.

.PARAMETER InstallDir
  Install root (default C:\gpn).

.PARAMETER Ref
  Referral code of whoever referred you (4-32 chars of [A-Za-z0-9_-]).
  Recorded ONCE in $GPN_PREFIX\referral (default %USERPROFILE%\.gpn\referral)
  and never rewritten -- the same file, same rule, as install.sh --ref and
  `gpn_node --ref CODE`. Data collection for the testnet-participation
  allocation only: no rewards, no chain writes. A malformed code is a
  WARNING, never an install failure.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File install.ps1

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File install.ps1 --ref team-alpha

.EXAMPLE
  install.cmd --remove-autostart
#>
param(
)
<#  Parameters are parsed by hand below (not via param()): a script param block
    rejects unknown `--flags` with a binding error before any code runs, and we
    accept both PowerShell style (-Uninstall) and documented bash style
    (--remove-autostart).
#>

# Native tools (schtasks) write to stderr on a missing task; that must not be
# turned into a terminating error by $ErrorActionPreference='Stop'. Every
# mutation checks its exit code explicitly instead.
$ErrorActionPreference = 'Continue'

$Uninstall  = $false
$WhatIf     = $false
$NoStart    = $false
$NoOllamaInstall = $false
$RunHighest = $false
$Verify     = $false
$UpdateTaskOnly = $false
# Fast sync: bootstrap from the weekly signed snapshot, as install.sh does.
# OFF only via --no-fast-sync / GPN_FAST_SYNC=0.
$FastSync = $true
if ($env:GPN_FAST_SYNC -eq '0') { $FastSync = $false }
$InstallDir = 'C:\gpn'
# Referral code (--ref CODE / --ref=CODE / GPN_REF=CODE). Optional; a bad value
# is a warning, never an install failure -- see Set-Referral below.
$RefCode    = if ($env:GPN_REF) { [string]$env:GPN_REF } else { '' }

for ($i = 0; $i -lt $args.Count; $i++) {
  $a = [string]$args[$i]
  switch -Regex ($a) {
    '^-{1,2}(uninstall|remove-autostart)$' { $Uninstall = $true }
    '^-{1,2}what ?if$'                     { $WhatIf    = $true }
    '^-{1,2}no-?start$'                    { $NoStart   = $true }
    '^-{1,2}no-?ollama-?install$'          { $NoOllamaInstall = $true }
    '^-{1,2}run-?highest$'               { $RunHighest = $true }
    '^-{1,2}(verify|v)$'                   { $Verify = $true }
    '^-{1,2}no-?fast-?sync$'               { $FastSync = $false }
    '^-{1,2}fast-?sync$'                   { $FastSync = $true }
    '^-{1,2}update-?task-?only$'           { $UpdateTaskOnly = $true }
    '^-{1,2}install-?dir[=:](.+)$'         { $InstallDir = $Matches[1] }
    '^-{1,2}install-?dir$' {
      if ($i + 1 -lt $args.Count) { $i++; $InstallDir = [string]$args[$i] }
    }
    '^-{1,2}ref[=:](.+)$'                  { $RefCode = $Matches[1] }
    '^-{1,2}ref$' {
      if ($i + 1 -lt $args.Count) { $i++; $RefCode = [string]$args[$i] } else { $RefCode = '' }
    }
    '^-{1,2}(help|\?)$' { Get-Help $MyInvocation.MyCommand.Path -Detailed; exit 0 }
    default { Write-Host " !  ignoring unknown argument: $a" -ForegroundColor Yellow }
  }
}

$Root      = Split-Path -Parent $MyInvocation.MyCommand.Path
$BinDir    = Join-Path $InstallDir 'bin'
$DataDir   = Join-Path $InstallDir 'data'
$NodeCmd   = Join-Path $InstallDir 'start-node.cmd'
$HiddenLaunch = Join-Path $InstallDir 'hidden-launch.vbs'
$NodeLog   = Join-Path $InstallDir 'node.log'
$ReportScript = Join-Path $InstallDir 'report-exit.ps1'
$NodeTask  = 'GPN-Node'
$OllamaTask = 'GPN-Ollama'
$UpdateTask = 'GPN-Update'
$UpdateScript = Join-Path $InstallDir 'update-and-restart.ps1'
$UpdateLog = Join-Path $InstallDir 'update.log'
$UpdateLock = Join-Path $InstallDir 'update.lock'
$Dashboard = 'http://127.0.0.1:8099'
$InstallLog = Join-Path $InstallDir 'install-log.txt'

# Self-logging: "it installed and then the window closed" is undiagnosable.
# Every run writes the full transcript here, so the last line is always the
# reason, whether the run succeeded, refused, or crashed.
if (-not $WhatIf) {
  try {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Start-Transcript -Path $InstallLog -Append -Force | Out-Null
    Say "logging this run to $InstallLog"
  } catch { }
}

# Canonical defaults -- identical to install.sh. An operator-supplied env var
# wins; nothing here is a secret (never put a key in this file).
$ApUrl  = if ($env:GPN_AP_URL) { $env:GPN_AP_URL } else { 'http://gpn-testnet.duckdns.org:8092' }
$Boots  = if ($env:GPN_BOOTNODES) { $env:GPN_BOOTNODES } else {
  '/dns4/gpn-testnet.duckdns.org/tcp/30333/ws/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/142.129.54.88/tcp/30333/ws/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo'
}

# -- Same-LAN seed -----------------------------------------------------------
# A node that shares a subnet with the seed should dial it directly. Going out
# to the public address and back through the router ("hairpin NAT") completes
# the TCP handshake but libp2p can reject the connection as a self-dial, which
# is exactly what happened on 2026-11-06: TcpTestSucceeded = True, peer never
# appeared, and two boxes forked away mining alone.
#
# The LAN address is computed HERE, per machine, and only written into THIS
# machine's launcher -- it must never be published in the bundle: 192.168.1.x
# on somebody else's home network is a printer, not a seed.
function Add-SameSubnetBootnode {
    param([string]$Boots)
    try {
        $locals = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop |
                    Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' })
    } catch { return $Boots }
    if ($locals.Count -eq 0) { return $Boots }

    $entries = $Boots -split ',' | Where-Object { $_.Trim() }
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($e in $entries) {
        $out.Add($e.Trim())
        # Only IPv4 literals can be compared to a local subnet. A DNS name may
        # well resolve to a LAN address, but guessing that here would be worse
        # than leaving it alone.
        $m = [regex]::Match($e, '^/ip4/(\d+\.\d+\.\d+\.\d+)/tcp/(\d+)/p2p/(\S+)$')
        if (-not $m.Success) { continue }
        $seedIp, $port, $peer = $m.Groups[1].Value, $m.Groups[2].Value, $m.Groups[3].Value
        $seedPrefix = ($seedIp -split '\.')[0..2] -join '.'
        foreach ($l in $locals) {
            $localPrefix = ($l.IPAddress -split '\.')[0..2] -join '.'
            if ($localPrefix -ne $seedPrefix) { continue }
            $candidate = "/ip4/$($l.IPAddress)/tcp/$port/p2p/$peer"
            if (-not ($out -contains $candidate)) {
                $out.Insert(0, $candidate)   # first = preferred
                Warn "seed $seedIp shares this subnet -- dialling it at $($l.IPAddress) instead of through the router"
            }
            break
        }
    }
    return ($out -join ',')
}

function Say  { param([string]$m) Write-Host "==> $m" -ForegroundColor Green }
function Warn { param([string]$m) Write-Host " !  $m" -ForegroundColor Yellow }

# All schtasks calls go through here: stderr is swallowed, rc returned.
# schtasks messages are the only explanation we get for a failure, so keep them.
# Discarding this output is why "rc=1" was undiagnosable on box 3 and on the
# fresh Windows box (2026-11-03/04): "Access is denied" and "cannot find the
# file specified" are the same number otherwise.
$script:LastSchtasksOutput = ''
function Invoke-Schtasks {
  param([string[]]$Args1)
  $script:LastSchtasksOutput = ((& schtasks @Args1 2>&1) | Out-String).Trim()
  return $LASTEXITCODE
}

# One place for "mutate the machine": honours -WhatIf everywhere.
function Invoke-Step {
  param([string]$Description, [scriptblock]$Action)
  if ($WhatIf) { Write-Host "[whatif] $Description" -ForegroundColor Cyan; return }
  & $Action
}

# ------------------------------------------------------------ detached start --
# Start the node with NO console of its own.
#
# 2026-11-04, box 1: `Start-Process -WindowStyle Hidden` still gives the child a
# console, and it is the SAME console as the installer's. When that window
# closed, Windows sent CTRL_CLOSE_EVENT to every attached process: the node and
# both children died together with 0xC000013A (=
# -1073741510) after mining for 35 minutes. Hidden is not detached.
#
# 2026-11-16: Win32_Process.Create has no console to inherit, but it still put a
# console window on the desktop when this ran from the user's interactive
# session, which is what the owner saw. Both paths now go through
# hidden-launch.vbs (wscript + window style 0), so the node has a hidden console
# and no visible window, whichever way it is started.
function Start-NodeDetached {
  param([string]$Command = '', [string]$WorkDir = '')
  if (-not $WorkDir) { $WorkDir = $InstallDir }
  $hidden = Join-Path $WorkDir 'hidden-launch.vbs'
  if (-not (Test-Path -LiteralPath $hidden)) { $hidden = $HiddenLaunch }
  try {
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
      CommandLine      = "wscript.exe `"$hidden`""
      CurrentDirectory = $WorkDir
    } -ErrorAction Stop
    if ($r -and [int]$r.ReturnValue -eq 0) {
      Say "node started detached, hidden (wscript, pid $($r.ProcessId))"
      return 'cim'
    }
    Warn "Win32_Process.Create returned $($r.ReturnValue) -- falling back to Start-Process"
  } catch {
    Warn "CIM/WMI is unavailable ($($_.Exception.Message)) -- falling back to Start-Process"
  }
  # Last resort: still the hidden launcher, so the node gets no visible window;
  # wscript itself is a GUI host and has none either.
  Start-Process -FilePath 'wscript.exe' -ArgumentList "`"$hidden`"" -WorkingDirectory $WorkDir -WindowStyle Hidden
  Say 'node started via wscript hidden launcher (fallback path)'
  return 'start-process'
}

function Test-Ollama {
  try {
    Invoke-RestMethod 'http://127.0.0.1:11434/api/tags' -TimeoutSec 3 | Out-Null
    return $true
  } catch { return $false }
}

function Wait-Ollama {
  param([int]$Seconds = 60)
  $deadline = (Get-Date).AddSeconds($Seconds)
  while ((Get-Date) -lt $deadline) {
    if (Test-Ollama) { return $true }
    Start-Sleep -Seconds 2
  }
  return $false
}

# ---------------------------------------------------------------- uninstall --
function Invoke-Uninstall {
  Say "removing GPN autostart (chain data and wallet are NOT touched)"

  foreach ($tn in @($NodeTask, $OllamaTask, $UpdateTask)) {
    $rc = Invoke-Schtasks @('/Query', '/TN', $tn)
    if ($rc -ne 0) { Say "task $tn not present"; continue }

    Invoke-Step "schtasks /End /TN $tn" { [void](Invoke-Schtasks @('/End', '/TN', $tn)) }
    Invoke-Step "schtasks /Delete /TN $tn /F" {
      $rc = Invoke-Schtasks @('/Delete', '/TN', $tn, '/F')
      if ($rc -ne 0) { Warn "could not delete task $tn (rc=$rc)" }
    }
    Say "removed task $tn"
  }

  # The auto-update wrapper is ours; the log is not (it explains the last cycle
  # to whoever is debugging the box, so it stays).
  Invoke-Step "remove $UpdateScript" {
    if (Test-Path $UpdateScript) { Remove-Item -Force $UpdateScript }
    if (Test-Path $UpdateLock) { Remove-Item -Force $UpdateLock }
  }
  Say 'removed the auto-update wrapper (update.log is kept for diagnosis)'

  # Ollama's OWN autostart (installed by Ollama itself) is deliberately left
  # alone: deleting it would change a component we did not install.
  Say "left in place: ollama's own autostart entry (if any), $DataDir, and the wallet"
  Write-Host ''
  Write-Host "Your node will no longer start automatically."
  Write-Host "Your data and wallet are untouched:"
  Write-Host "  chain DB + keystore: $(Join-Path $DataDir 'keystore.json')  (this is your money -- back it up)"
  Write-Host "  binaries:            $BinDir"
  Write-Host "Start the node by hand any time:  `"$NodeCmd`"  (or double-click start-gpn.cmd for the startup summary)"
  return
}

# ------------------------------------------------------- ollama autostart ----
function Find-ExistingOllamaAutostart {
  foreach ($k in @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Run',
                   'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run')) {
    try {
      $p = Get-ItemProperty -Path $k -ErrorAction Stop
      if ($p.PSObject.Properties.Name -match '^(?i)ollama') { return "Run key $k" }
    } catch { }
  }
  $startup = [Environment]::GetFolderPath('Startup')
  if ($startup -and (Test-Path (Join-Path $startup 'Ollama.lnk'))) { return 'Startup folder shortcut' }
  try {
    if (Get-ScheduledTask -TaskName 'Ollama' -ErrorAction Stop) { return 'scheduled task Ollama' }
  } catch { }
  return $null
}

function Register-OllamaAutostart {
  $existing = Find-ExistingOllamaAutostart
  if ($existing) {
    # Do not stack a second autostart on top of the one the Ollama installer
    # already created -- two `ollama serve` at logon fight over :11434.
    Say "ollama already starts automatically ($existing) -- not adding a duplicate entry"
    return 'existing'
  }
  $state = Invoke-Step "schtasks /Create /TN $OllamaTask /TR `"cmd /c ollama serve`" /SC ONLOGON /F" {
    $rc = Invoke-Schtasks @('/Create', '/TN', $OllamaTask, '/TR', 'cmd /c ollama serve', '/SC', 'ONLOGON', '/F')
    if ($rc -ne 0) { return 'failed' }
    return 'created'
  }
  if ($state -eq 'created') {
    Say "ollama autostart registered (task $OllamaTask)"
  } elseif ($state -eq 'failed') {
    Warn "$OllamaTask could not be registered -- ollama may not be running after a reboot, and the node will refuse to start"
  }
  return $(if ($state) { $state } else { 'whatif' })
}

# ---------------------------------------------------------------- launcher ---
# Static template (single-quoted here-string, no interpolation) so CI can
# text-assert it the same way it asserts the workflow's launcher. Placeholders
# are replaced at write time.
$LauncherTemplate = @'
@echo off
REM ============================================================================
REM  GPN node launcher -- generated by install.ps1. Re-run install.ps1 to
REM  regenerate; edits here are overwritten.
REM
REM  A GPN node is ALWAYS both a miner and an AI provider. This starts the
REM  single-node supervisor gpn_node.exe, which brings up the chain node/miner,
REM  the provider heartbeat and the AI claim/infer loop together.
REM
REM  TWO MODES -- this is what makes "close the window" safe:
REM    start-node.cmd          user entry. Starts the node in the BACKGROUND
REM                            (via the GPN-Node task, or Win32_Process.Create
REM                            when there is no task), prints a one-time startup
REM                            summary and stays open. Closing that window does
REM                            NOT stop the node.
REM    start-node.cmd --task   the worker the hidden launcher (hidden-launch.vbs)
REM                            starts for the GPN-Node task. It runs the
REM                            supervisor in the foreground of a HIDDEN console:
REM                            wscript gives the task no console at all, so there
REM                            is no window to see and none to close.
REM ============================================================================
if /I "%~1"=="--task" goto :gpn_worker
if exist "%~dp0start-gpn.cmd" (
  call "%~dp0start-gpn.cmd"
  exit /b
)
:gpn_worker
setlocal
set "GPN_CHAIN=testnet"
REM --- data folder (chain DB + wallet live here) ------------------------------
set "GPN_BASE_PATH=__INSTALL_DIR__\data"
if not exist "%GPN_BASE_PATH%" mkdir "%GPN_BASE_PATH%" >nul 2>&1
REM --- money identity: created ON THIS MACHINE, never baked in ---------------
set "GPN_SIGNER="
set "GPN_ACCOUNT="
set "GPN_SIGNER_FILE=%GPN_BASE_PATH%\keystore.json"
REM --- access point this node polls for AI work ------------------------------
set "GPN_AP_URL=__AP_URL__"
REM --- canonical bootstrap peers --------------------------------------------
set "GPN_BOOTNODES=__BOOTNODES__"
REM --- reserved peers: always re-dialled, unlike a plain bootnode -----------
REM 2026-11-06: two boxes stayed disconnected for 40 min after the seed host
REM restarted. Bootnodes are dialled at start and then retried on a slow
REM backoff; a reserved peer keeps a slot and is re-dialled aggressively, so
REM "always reconnect to the seed" is enforced instead of hoped for.
set "GPN_RESERVED_PEERS=__BOOTNODES__"
REM --- local dashboard (loopback only): http://127.0.0.1:8099 ----------------
set "GPN_GUI=1"
REM --- where to look: the console only prints failures; the dashboard is live ---
echo [gpn] dashboard: http://127.0.0.1:8099   (status, models, peers, jobs -- keep this open)

set "GPN_OLLAMA=http://127.0.0.1:11434"
REM --- which binaries to run ------------------------------------------------
REM  The updater installs a complete version tree under data\update\versions
REM  and flips the `current` junction to select one. Run THAT supervisor when it
REM  is there: Windows locks a running .exe, so "copy the new build over bin" is
REM  exactly the step that cannot work while the node is up (box 3, 2026-11-14:
REM  0.3.46 staged and verified while 0.3.45 kept running for 8 h). Selecting a
REM  directory through the junction needs no locked-file write at all.
REM  bin\ stays as the fallback for a first install or a broken link.
set "GPN_BIN=__INSTALL_DIR__\data\update\current"
if not exist "%GPN_BIN%\gpn_node.exe" set "GPN_BIN=__INSTALL_DIR__\bin"

REM --- per-machine node identity (a shared key makes boxes collide) ----------
REM  NO cmd-side probing. `if not exist` reported a present, readable, unlocked
REM  key as MISSING on box 2 (2026-11-06) and the launcher then blamed the
REM  identity; the same file answered EXISTS minutes later. The binary opens the
REM  file, so it is the only thing that can answer honestly.
set "GPN_NODE_KEY="
if not defined USERPROFILE set "USERPROFILE=%HOMEDRIVE%%HOMEPATH%"
set "GPN_NODE_KEY_FILE=%USERPROFILE%\.gpn\node-key"
set "KEYRC=1"
"%GPN_BIN%\gpn_node.exe" --ensure-node-key --file "%GPN_NODE_KEY_FILE%"
set "KEYRC=%ERRORLEVEL%"
if not "%KEYRC%"=="0" (
  echo [gpn] ERROR: no usable node identity -- the line above says why.
  powershell -NoProfile -ExecutionPolicy Bypass -File "__REPORT__" -ExitCode %KEYRC% -Log "__LOG__"
  exit /b %KEYRC%
)

REM --- ollama is REQUIRED: gpn_node exits 69 without it ----------------------
powershell -NoProfile -Command "try { Invoke-RestMethod 'http://127.0.0.1:11434/api/tags' -TimeoutSec 3 | Out-Null } catch { Start-Process ollama -ArgumentList 'serve' -WindowStyle Hidden }"

REM --- money identity: create once, then NEVER replace -----------------------
"%GPN_BIN%\gpn_node.exe" --ensure-wallet
if errorlevel 1 (
  set "WRC=%ERRORLEVEL%"
  echo [gpn] ERROR: could not set up this machine's wallet - the node will NOT start.
  echo [gpn] ERROR: fix the keystore at %GPN_SIGNER_FILE%, or delete it on purpose to mine for a new account.
  powershell -NoProfile -ExecutionPolicy Bypass -File "__REPORT__" -ExitCode %WRC% -Log "__LOG__"
  exit /b %WRC%
)

REM --- run the supervisor; on failure SHOW the reason, then retry ------------
REM  A scheduled task runs HIDDEN, so a message box is the only channel the
REM  operator actually sees. report-exit.ps1 reads ~/.gpn/last-exit.json, writes
REM  the same text to the log, and stops this loop after the SAME failure 3
REM  times in 10 minutes (an endless restart loop is its own silent failure).
:run
REM  Run the node DIRECTLY -- no PowerShell wrapper, no pipeline.
REM  The old wrapper (powershell, piping through a file-teeing cmdlet) exited 1
REM  BEFORE the node starts whenever the launcher is spawned without a console
REM  (installer's Win32_Process.Create, scheduled task), and the operator then
REM  sees only "the node exited 1 without writing a reason" -- the node never
REM  ran. Reported on box 4, 2026-11-05: identical command works from an
REM  interactive shell, fails when detached.
"%GPN_BIN%\gpn_node.exe" >> "__LOG__" 2>&1
set "RC=%ERRORLEVEL%"
if "%RC%"=="0" goto :done
powershell -NoProfile -ExecutionPolicy Bypass -File "__REPORT__" -ExitCode %RC% -Log "__LOG__"
if errorlevel 3 (
  echo [gpn] crash loop detected -- not restarting. The reason is in __LOG__ and in the message box.
  goto :done
)
echo [gpn] node stopped (exit %RC%); retrying in 10s...
ping -n 11 127.0.0.1 >nul 2>&1
goto :run
:done
'@

# ------------------------------------------------------- hidden launcher ---
# The GPN-Node task must run the node with NO console window. It used to run
# `cmd /c C:\gpn\start-node.cmd --task` directly: Task Scheduler owns that
# console, so the node's stdout appeared in a window the user could see, and
# closing that window killed the node (exit -1073741510 = STATUS_CONTROL_C_EXIT,
# 'console-closed').
#
# wscript.exe is a GUI-subsystem host: Task Scheduler gives it no console at all.
# WScript.Shell.Run(..., 0, True) then starts the worker with a HIDDEN console,
# so there is no window to close -- and no admin rights, stored password or S4U
# logon type is involved (a hidden task is still an ordinary ONLOGON task run
# with the interactive token).
#
# Wait = True is deliberate: wscript stays alive for as long as the node runs, so
#   * MultipleInstances=IgnoreNew still prevents a second /Run from forking a
#     second supervisor onto the same ports + chain DB, and
#   * `schtasks /End /TN GPN-Node` still has a task process to end.
# The node's stdout/stderr are unaffected: start-node.cmd still redirects them to
# node.log itself, so a hidden console loses nothing.
$HiddenLaunchTemplate = @'
' hidden-launch.vbs -- generated by install.ps1; do not hand-edit.
' Runs the GPN node worker with NO console window (window style 0 = hidden).
' Task Scheduler gives wscript.exe no console, so nothing of the node is visible
' and there is no window whose close could kill it.
Option Explicit
Dim fso, sh, q, here, worker, wmi, procs
Set fso = CreateObject("Scripting.FileSystemObject")
Set sh  = CreateObject("WScript.Shell")
q = Chr(34)
here = fso.GetParentFolderName(WScript.ScriptFullName)

' One supervisor only: an /End can leave a node behind, and a second /Run would
' then put two supervisors on the same ports and the same chain database.
Set wmi = GetObject("winmgmts:\\.\root\cimv2")
Set procs = wmi.ExecQuery("SELECT ProcessId FROM Win32_Process WHERE Name='gpn_node.exe'")
If procs.Count > 0 Then WScript.Quit 0

worker = "cmd /c " & q & q & here & "\start-node.cmd" & q & " --task" & q
' 0 = hidden, True = wait (keeps this task instance alive while the node runs).
sh.Run worker, 0, True
'@

function Write-HiddenLaunch {
  Invoke-Step "write $HiddenLaunch" {
    Set-Content -Path $HiddenLaunch -Value $HiddenLaunchTemplate -Encoding ascii
  }
  Say "hidden launcher written: $HiddenLaunch"
}

function Write-Launcher {
  $text = $LauncherTemplate
  $text = $text.Replace('__INSTALL_DIR__', $InstallDir)
  $text = $text.Replace('__KEYFILE__', (Join-Path $env:USERPROFILE '.gpn\node-key'))
  $text = $text.Replace('__AP_URL__', $ApUrl)
  $text = $text.Replace('__BOOTNODES__', (Add-SameSubnetBootnode $Boots))
  $text = $text.Replace('__LOG__', $NodeLog)
  $text = $text.Replace('__REPORT__', $ReportScript)
  Invoke-Step "write $NodeCmd" {
    Set-Content -Path $NodeCmd -Value $text -Encoding ascii
  }
  Say "launcher written: $NodeCmd"
  return $text
}

# ------------------------------------------------ start + startup summary --
# The user-facing half of the launcher. It does two things and then gets out of
# the way:
#   1. make sure the node is running in the BACKGROUND (GPN-Node task first,
#      Win32_Process.Create second, hidden Start-Process last), and
#   2. print a one-time startup summary -- running binary + version, paths,
#      chain, wallet, key presence, ollama, peers/head, the dashboard URL and
#      the log path -- then idle until the user closes the window.
#
# It NEVER polls, refreshes or re-prints: the live view is the dashboard. And it
# never ties the node's lifetime to this window: the node is a task child or a
# WMI child, never a child of this process.
#
# Everything here is best effort with short timeouts. A node that has not
# answered yet must not stop the summary from being printed (owner rule,
# 2026-11-15), and nothing here ever reads key material -- only whether the key
# file exists.
$StartGpnPs1Template = @'
# start-gpn.ps1 -- generated by install.ps1; do not hand-edit.
#
# Starts GPN in the BACKGROUND and prints a one-time startup summary. Closing
# this window does NOT stop the node: it is started through the GPN-Node
# scheduled task, or Win32_Process.Create (which has no console to inherit) --
# never as a child of this console. Nothing here polls or refreshes; the live
# status view is http://127.0.0.1:8099.
param([switch]$NoWait)

$ErrorActionPreference = 'SilentlyContinue'
$InstallDir = '__INSTALL_DIR__'
$NodeCmd    = Join-Path $InstallDir 'start-node.cmd'
$LogPath    = Join-Path $InstallDir 'node.log'
$DataDir    = Join-Path $InstallDir 'data'
$Dashboard  = 'http://127.0.0.1:8099'
$RpcUrl     = 'http://127.0.0.1:9944'
$TaskName   = 'GPN-Node'
$OutFile    = Join-Path $InstallDir 'startup-summary.txt'

function Get-NodeProcess {
  @(Get-Process -Name 'gpn_node','gpn-node' -ErrorAction SilentlyContinue) | Select-Object -First 1
}

function Invoke-Rpc([string]$Method) {
  $body = '{"jsonrpc":"2.0","id":1,"method":"' + $Method + '","params":[]}'
  try { return (Invoke-RestMethod -Uri $RpcUrl -Method Post -Body $body -ContentType 'application/json' -TimeoutSec 2) } catch { return $null }
}

Write-Host ''
Write-Host '  Starting GPN in the background...' -ForegroundColor Cyan

$wasRunning = [bool](Get-NodeProcess)
if (-not $wasRunning) {
  # Task first: a task-owned node has no console to lose and can still be
  # stopped with `schtasks /End` for an update.
  $rc = 1
  $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if ($task) {
    & schtasks /Run /TN $TaskName 2>&1 | Out-Null
    $rc = $LASTEXITCODE
  }
  # Every fallback goes through the hidden launcher too: Win32_Process.Create and
  # Start-Process both hand a console app a console window when the caller is the
  # user's interactive session, which is the whole problem this solves.
  $hidden = Join-Path $InstallDir 'hidden-launch.vbs'
  if ($rc -ne 0) {
    try {
      $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine      = "wscript.exe `"$hidden`""
        CurrentDirectory = $InstallDir
      } -ErrorAction Stop
      if ($r -and [int]$r.ReturnValue -eq 0) { $rc = 0 }
    } catch { }
  }
  if ($rc -ne 0) {
    Start-Process -FilePath 'wscript.exe' -ArgumentList "`"$hidden`"" -WorkingDirectory $InstallDir -WindowStyle Hidden
  }
}

# Bounded wait: give the node a chance to answer so peers/head/chain are real
# numbers, whether it was already starting or we just launched it. Deliberately
# short and never fatal -- the summary prints even when nothing answers yet.
if (-not (Invoke-Rpc 'system_health')) {
  $deadline = (Get-Date).AddSeconds(20)
  while ((Get-Date) -lt $deadline) {
    if (Invoke-Rpc 'system_health') { break }
    Start-Sleep -Seconds 1
  }
}

# ---- what is actually running ----------------------------------------------
$proc     = Get-NodeProcess
$procPath = if ($proc) { $proc.Path } else { 'not running' }

# Version of the RUNNING binary (from its process path, never from bin\).
$version = ''
if ($proc -and $proc.Path) {
  $vi = (Get-Item -LiteralPath $proc.Path -ErrorAction SilentlyContinue).VersionInfo
  if ($vi) { $version = [string]$vi.ProductVersion; if (-not $version) { $version = [string]$vi.FileVersion } }
}
if (-not $version) {
  $v = Invoke-Rpc 'system_version'
  if ($v -and $v.result) { $version = [string]$v.result }
}
if (-not $version -and $procPath -ne 'not running') {
  # ...\data\update\current\gpn_node.exe -> resolve `current` -> ...\versions\0.3.47
  $dir = Split-Path -Parent $procPath
  $t = @((Get-Item -LiteralPath $dir -Force -ErrorAction SilentlyContinue).Target) | Select-Object -First 1
  if ($t) { $version = Split-Path -Leaf ([string]$t) }
}
if (-not $version) { $version = 'unknown' }

$chain = ''
$c = Invoke-Rpc 'system_chain'
if ($c -and $c.result) { $chain = [string]$c.result }
if (-not $chain) { $chain = 'unknown' }

# Wallet account comes from the node's own status document. It is a public
# address; nothing secret is ever printed.
$account = ''
$s = $null
try {
  $s = Invoke-RestMethod -Uri "$Dashboard/api/status" -TimeoutSec 2
  if ($s.account) { $account = [string]$s.account }
} catch { }
if (-not $account) { $account = 'not reported yet' }
if ($chain -eq 'unknown' -and $s -and $s.chain) { $chain = [string]$s.chain }

$peers = 'n/a'
$h = Invoke-Rpc 'system_health'
if ($h -and $h.result -and $null -ne $h.result.peers) { $peers = [string]$h.result.peers }

$head = 'n/a'
$hd = Invoke-Rpc 'chain_getHeader'
if ($hd -and $hd.result -and $hd.result.number) {
  try { $head = [string][Convert]::ToInt64(($hd.result.number -replace '^0x',''), 16) } catch { }
}

$keyFile  = Join-Path $env:USERPROFILE '.gpn\node-key'
$keyState = if (Test-Path $keyFile) { 'present' } else { 'MISSING' }

$ollama = 'not reachable'
try { Invoke-RestMethod 'http://127.0.0.1:11434/api/tags' -TimeoutSec 2 | Out-Null; $ollama = 'reachable' } catch { }

$lines = @(
  '',
  '  ============================================================',
  '    GPN is running in the background',
  '  ============================================================',
  ('    binary version : {0}' -f $version),
  ('    binary path    : {0}' -f $procPath),
  ('    base path      : {0}' -f $DataDir),
  ('    chain          : {0}' -f $chain),
  ('    wallet account : {0}' -f $account),
  ('    node key       : {0} ({1})' -f $keyState, $keyFile),
  ('    ollama         : {0}' -f $ollama),
  ('    peers          : {0}' -f $peers),
  ('    chain head     : {0}' -f $head),
  '',
  ('    Live status    : {0}' -f $Dashboard),
  ('    Log file       : {0}' -f $LogPath),
  '',
  '  Closing this window does NOT stop the node.',
  '  ============================================================',
  ''
)
foreach ($l in $lines) { Write-Host $l }
try { Set-Content -Path $OutFile -Value ($lines -join [Environment]::NewLine) -Encoding UTF8 } catch { }

Write-Host '  Press any key to close this window (the node keeps running).' -ForegroundColor Cyan
if (-not $NoWait) {
  try { [void][Console]::ReadKey($true) } catch { }
}
'@

$StartGpnCmdTemplate = @'
@echo off
title GPN - startup
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start-gpn.ps1" %*
'@

function Write-StartGpn {
  $ps1 = Join-Path $InstallDir 'start-gpn.ps1'
  $cmd = Join-Path $InstallDir 'start-gpn.cmd'
  $psText = $StartGpnPs1Template.Replace('__INSTALL_DIR__', $InstallDir)
  Invoke-Step "write $ps1" { Set-Content -Path $ps1 -Value $psText -Encoding ascii }
  Invoke-Step "write $cmd" { Set-Content -Path $cmd -Value $StartGpnCmdTemplate -Encoding ascii }
  Say "startup summary written: $cmd"
}

# ---------------------------------------------------- exit reporting --------
# Generated next to start-node.cmd. It is the ONLY channel a hidden scheduled
# task has: a dialog the user sees, plus a line in node.log, plus the 3-strikes
# rule that stops an endless restart loop.
$ReportTemplate = @'
# report-exit.ps1 -- generated by install.ps1; explains WHY the node stopped.
param([int]$ExitCode = 1, [string]$Log = '', [string]$Note = '')
$ErrorActionPreference = 'SilentlyContinue'
$exitFile = Join-Path $env:USERPROFILE '.gpn\last-exit.json'

function Get-Reports {
  if (-not (Test-Path $exitFile)) { return @() }
  $out = @()
  foreach ($line in (Get-Content -Path $exitFile -Tail 32)) {
    if (-not $line.Trim()) { continue }
    try { $out += ($line | ConvertFrom-Json) } catch { }
  }
  return $out
}

$reports = Get-Reports
$last = $null
if ($reports.Count -gt 0) {
  $candidate = $reports[-1]
  if ([int]$candidate.code -eq $ExitCode) {
    $last = $candidate
  } elseif (([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - [int]$candidate.ts_unix) -le 600) {
    # 2026-11-04: the supervisor can die (or be killed) right after a child
    # exits, so its own exit code is 1 even though the child's was 0xC000013A.
    # A record written in the last 10 minutes is still the reason.
    $last = $candidate
  }
}

# Only used when there is no record at all: the last lines of node.log. A
# dialog that says only "unknown" is a dead end.
$extra = ''
if (-not $Note -and -not $last) {
  if ($Log -and (Test-Path $Log)) {
    try {
      $tailLines = @(Get-Content -Path $Log -Tail 5 -ErrorAction Stop)
      if ($tailLines.Count -gt 0) {
        $extra = "`nlast log lines:`n"
        foreach ($l in $tailLines) {
          if ($l.Length -gt 200) { $l = $l.Substring(0, 200) + '...' }
          $extra = $extra + "  " + $l + "`n"
        }
      }
    } catch { }
  }
}

if ($Note) {
  # The auto-update wrapper reports through this same message box. Its failures
  # are not node exits, so they get their own kind and never join the node's
  # 3-strikes count (-Note is only supplied by update-and-restart.ps1).
  $kind   = 'update'
  $detail = $Note
  $fix    = "see $Log"
} elseif ($last) {
  $kind   = [string]$last.kind
  $detail = [string]$last.detail
  $fix    = [string]$last.remediation
} else {
  # A loader failure (0xC0000135) never reaches the program: no entry exists.
  # Map the status we were given, and record a synthetic entry so 3-strikes
  # detection works for this case too.
  switch ("$ExitCode") {
    '-1073741515' {
      $kind='vc-runtime'
      $detail='missing Visual C++ runtime (0xC0000135) - the node cannot even load, so it printed nothing'
      $fix='winget install --id Microsoft.VCRedist.2015+.x64 -e'
    }
    '3221225781' {
      $kind='vc-runtime'
      $detail='missing Visual C++ runtime (0xC0000135) - the node cannot even load, so it printed nothing'
      $fix='winget install --id Microsoft.VCRedist.2015+.x64 -e'
    }
    '69' { $kind='no-ollama'; $detail='ollama is required and was not reachable'; $fix='gpn_node.exe --setup' }
    '78' { $kind='not-dialable'; $detail='the node could not prove it is inbound-reachable'; $fix='forward TCP 30333, or set GPN_REQUIRE_BACKBONE=0' }
    '3'  { $kind='corrupt-key'; $detail='the wallet keystore is unusable'; $fix='restore the keystore from backup, then run gpn_node.exe --ensure-wallet' }
    '-1073741510' {
      $kind='console-closed'
      $detail='the console this node was started from was closed'
      $fix='start it detached (the installer does this) or run it as a scheduled task'
    }
    '3221225786' {
      $kind='console-closed'
      $detail='the console this node was started from was closed'
      $fix='start it detached (the installer does this) or run it as a scheduled task'
    }
    default {
      $kind='unknown'
      $detail="the node exited $ExitCode without writing a reason"
      $fix='run: C:\gpn\bin\gpn_node.exe --diagnose'
    }
  }
  $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
  $entry = [ordered]@{
    ts=(Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    ts_unix=$now; code=$ExitCode; kind=$kind; detail=$detail
    remediation=$fix; version='launcher'; os='windows'; pid=0
  }
  try {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $exitFile) | Out-Null
    Add-Content -Path $exitFile -Value (($entry | ConvertTo-Json -Compress))
  } catch { }
  $reports = Get-Reports
}

$text = @"
GPN node stopped
  reason : $kind (exit $ExitCode)
  found  : $detail
  fix    : $fix
  log    : $Log
$extra
"@
if ($Note) {
  $text = @"
GPN update failed
  reason : $detail
  fix    : update by hand: C:\gpn\bin\gpn_update.exe --apply
  log    : $Log
"@
}
if ($Log) { Add-Content -Path $Log -Value $text }

# 3 strikes in 10 minutes -> stop restarting. A crash loop that restarts forever
# is its own silent failure.
$cutoff = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - 600
$same = @($reports | Where-Object { $_.kind -eq $kind -and [int]$_.ts_unix -ge $cutoff })
# A dialog is a courtesy, not a gate. On a box nobody is sitting at, a modal
# MessageBox never returns, so the process that called us (and any lock it holds)
# leaks for ever. Show it in a job and give up after the timeout: the answer is
# already in the log, which is what an operator reads anyway.
function Show-Box {
  param([string]$Body, [string]$Title)
  try {
    $job = Start-Job -ScriptBlock {
      param($t, $ti)
      Add-Type -AssemblyName System.Windows.Forms
      [System.Windows.Forms.MessageBox]::Show($t, $ti, 'OK', 'Error') | Out-Null
    } -ArgumentList $Body, $Title
    if (-not (Wait-Job $job -Timeout 120)) { Stop-Job $job -ErrorAction SilentlyContinue }
    Remove-Job $job -Force -ErrorAction SilentlyContinue
  } catch { }
}

if ($same.Count -ge 3) {
  $loopText = "$text`nThe SAME failure happened $($same.Count) times in 10 minutes - NOT restarting. Fix the reason above, then start GPN-Node again (schtasks /Run /TN GPN-Node)."
  if ($Log) { Add-Content -Path $Log -Value $loopText }
  Show-Box $loopText 'GPN node: crash loop'
  exit 3
}

Show-Box $text 'GPN node stopped'
exit 0
'@

function Write-ReportScript {
  Invoke-Step "write $ReportScript" {
    Set-Content -Path $ReportScript -Value $ReportTemplate -Encoding ascii
  }
  Say "exit reporter written: $ReportScript"
}

# ------------------------------------------------- auto-update (GPN-Update) ---
# Windows equivalent of the Linux gpn-update.timer. Why the wrapper exists at
# all: gpn_update --apply copies every binary over C:\gpn\bin, and Windows
# refuses to replace a running .exe. The updater reports that as an error; this
# wrapper prevents it by stopping GPN-Node first, and it always starts the node
# again afterwards so a failed update cannot leave a dark box.
#
# Static template (single-quoted here-string, no interpolation) so CI can
# text-assert the control flow the same way it asserts the launcher.
$UpdateWrapperTemplate = @'
# update-and-restart.ps1 -- generated by install.ps1; do not hand-edit.
#
# One GPN auto-update cycle, run by the GPN-Update scheduled task every hour
# (TEMPORARY review-window cadence; reverts with DEFAULT_CHECK_HOURS)
# (+/- 30 minutes of jitter, so a fleet does not stampede the release host).
# This mirrors the Linux gpn-update.timer, with one Windows-specific twist:
#
#   Windows locks a running .exe, so gpn_update --apply cannot replace the
#   binaries while the node runs. The order is therefore
#       check -> STOP GPN-Node -> apply -> ALWAYS start GPN-Node
#   A failed update still restarts: a box with nothing running is worse than a
#   box on the previous build.
#
# Exit codes are logged, never propagated: a normal "nothing to do" cycle must
# not show up as a red "last run result" in Task Scheduler.

$ErrorActionPreference = 'Continue'

$InstallDir   = '__INSTALL_DIR__'
$BinDir       = Join-Path $InstallDir 'bin'
$LogPath      = Join-Path $InstallDir 'update.log'
$LockPath     = Join-Path $InstallDir 'update.lock'
$ReportScript = Join-Path $InstallDir 'report-exit.ps1'
$NodeTask     = 'GPN-Node'
$Launcher     = Join-Path $InstallDir 'start-node.cmd'
$CurrentLink  = Join-Path $InstallDir 'data\update\current'
$RpcUrl       = if ($env:GPN_RPC_URL) { $env:GPN_RPC_URL } else { 'http://127.0.0.1:9944' }
# How long the new build gets to come up and answer before we roll it back.
# 360 s, not 120 (measured 2026-11-17 on box 1): after the swap the node started at
# 02:00:13 and only bound JSON-RPC at 02:02:18 -- 125 s of GPU init + state load -- so a
# 120 s window rolled back a build that was already up, and left `current` on the old
# version while the RUNNING process served the new one. A window shorter than a cold
# start is a gate that fails correct work.
$ConfirmSecs  = if ($env:GPN_UPDATE_CONFIRM_SECS) { [int]$env:GPN_UPDATE_CONFIRM_SECS } else { 360 }

function Write-Log {
  param([string]$Message)
  $line = '[' + (Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + '] ' + $Message
  try { Add-Content -Path $LogPath -Value $line -ErrorAction Stop } catch { }
  Write-Host $line
}

# A hidden scheduled task has no console: a message box is the only channel the
# operator actually sees. Restart the node BEFORE calling this -- a modal dialog
# must never be the reason the node stays down.
function Show-UpdateFailure {
  param([int]$Code, [string]$What, [string]$Detail)
  Write-Log ('FAILED: ' + $What + ' (exit ' + $Code + ') - ' + $Detail)
  if (-not (Test-Path $ReportScript)) { return }
  $note = 'auto-update: ' + $What + ' failed (exit ' + $Code + ') - ' + $Detail + '. See ' + $LogPath
  # DETACHED on purpose, never `&`: report-exit.ps1 ends in a modal MessageBox.
  # A synchronous call waits for a human to click it, so on an unattended box the
  # cycle never returns, `finally` never runs, and update.lock stays held for
  # ever -- every later cycle then logs "another GPN-Update run is still in
  # progress" and quietly stops checking (box 3, 2026-11-15: 23 hours of no
  # update checks, 0.3.48/0.3.49 never seen, current stuck on a directory).
  try {
    Start-Process -FilePath 'powershell' -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',
      ('"' + $ReportScript + '"'),'-ExitCode',[string]$Code,'-Log',('"' + $LogPath + '"'),'-Note',('"' + $note + '"')) -WindowStyle Hidden | Out-Null
  } catch {
    Write-Log ('could not show the failure dialog: ' + $_.Exception.Message)
  }
}

# The version `current` names, i.e. the one the launcher will run next.
function Get-ExpectedVersion {
  try {
    $item = Get-Item -LiteralPath $CurrentLink -Force -ErrorAction Stop
    $t = @($item.Target) | Select-Object -First 1
    if (-not $t) { return $null }
    return (Split-Path -Leaf $t)
  } catch { return $null }
}

# Any node process alive? "RPC not answering yet" is normal while the GPU miner
# initialises; "no node process at all" is a real failure and must not wait out the
# whole window. Returns $null when the question cannot be answered (never claim dead).
function Test-NodeAlive {
  try {
    $p = Get-Process -Name 'gpn_node','gpn-node' -ErrorAction SilentlyContinue | Select-Object -First 1
    return [bool]$p
  } catch { return $null }
}

# Ask the RUNNING node what version it is. This is the only evidence that an
# update actually took: everything before it describes files on disk.
# Returns a message on success, $null on failure (reason in $script:ConfirmReason).
function Confirm-Running {
  param([string]$Expect, [int]$Seconds = 120)
  $script:ConfirmReason = 'no answer'
  if (-not $Expect) { $script:ConfirmReason = 'no expected version to confirm'; return $null }
  $deadline = (Get-Date).AddSeconds($Seconds)
  $body = '{"jsonrpc":"2.0","id":1,"method":"system_version","params":[]}'
  $sawAlive = $false
  $t0 = Get-Date
  $lastNote = $t0
  while ((Get-Date) -lt $deadline) {
    # Liveness first: a build that is still starting is not a failure, but one whose
    # process is gone is -- and waiting 6 minutes to say so helps nobody.
    $alive = Test-NodeAlive
    if ($alive -eq $true) { $sawAlive = $true }
    elseif ($sawAlive -and $alive -eq $false) {
      $script:ConfirmReason = 'the node process exited before it answered'
      return $null
    }
    if (((Get-Date) - $lastNote).TotalSeconds -ge 30) {
      $lastNote = Get-Date
      Write-Log ('still starting: ' + $Expect + ' has not answered yet (' + [int]((Get-Date) - $t0).TotalSeconds + ' s of ' + $Seconds + ' s)')
    }
    try {
      $r = Invoke-RestMethod -Uri $RpcUrl -Method Post -Body $body -ContentType 'application/json' -TimeoutSec 3
      $got = ([string]$r.result).Trim()
      if ($got -eq $Expect.Trim()) { return ('{0} is running (rpc {1})' -f $got, $RpcUrl) }
      $script:ConfirmReason = ('the node reports ' + $got + ', expected ' + $Expect)
    } catch {
      $script:ConfirmReason = ('rpc ' + $RpcUrl + ': ' + $_.Exception.Message)
    }
    Start-Sleep -Seconds 2
  }
  $script:ConfirmReason = ('not confirmed within ' + $Seconds + ' s: ' + $script:ConfirmReason)
  return $null
}

# Make the NEXT start run the version tree the updater selects, instead of the
# locked bin\gpn_node.exe. Idempotent: a launcher that already points at the
# junction is left byte-identical, and only one backup is ever taken.
function Repair-Launcher {
  if (-not (Test-Path -LiteralPath $Launcher)) { return }
  $body = Get-Content -LiteralPath $Launcher -Raw
  $new = $body
  $notes = @()

  # 1. The selection line. Only a line that IS `set "GPN_BIN=<bin>"`: matching
  #    the text anywhere also hits the fallback line the repair itself writes,
  #    which turned a second run into a duplicated, nested pair (box 1).
  $binPattern = '(?m)^[ \t]*set "GPN_BIN=' + [regex]::Escape($BinDir) + '"[ \t]*\r?$'
  if ($new -match $binPattern) {
    $replacement = 'set "GPN_BIN=' + $CurrentLink + '"' + "`r`n" +
                   'if not exist "%GPN_BIN%\gpn_node.exe" set "GPN_BIN=' + $BinDir + '"'
    $new = [regex]::Replace($new, $binPattern, $replacement)
    $notes += ('runs ' + $CurrentLink + ' (falls back to ' + $BinDir + ')')
  }

  # 2. A hard-coded exec line. The release workflow's launcher used to start
  #    `"C:\gpn\bin\gpn_node.exe"` literally, so fixing GPN_BIN alone changed
  #    nothing: the supervisor (and every child it spawns) still came from bin
  #    (box 1, 2026-11-14).
  $literal = [regex]::Escape('"' + (Join-Path $BinDir 'gpn_node.exe') + '"')
  $execPattern = '(?m)^[ \t]*' + $literal
  if ($new -match $execPattern) {
    $new = [regex]::Replace($new, $execPattern, '"%GPN_BIN%\gpn_node.exe"')
    $notes += 'the supervisor is started through %GPN_BIN%, not a hard-coded bin path'
  }

  if ($new -eq $body) { return }
  $backup = Join-Path $InstallDir ('start-node.cmd.bak-' + (Get-Date -Format 'yyyyMMddHHmmss'))
  try {
    Copy-Item -LiteralPath $Launcher -Destination $backup -ErrorAction Stop
    Set-Content -LiteralPath $Launcher -Value $new -Encoding ascii -NoNewline -ErrorAction Stop
    Write-Log ('launcher repaired: ' + ($notes -join '; ') + '; backup ' + $backup)
  } catch {
    Write-Log ('could not repair the launcher: ' + $_.Exception.Message)
  }
}

# Flip `current` back one version: the updater's own verb when it has one, the
# junction itself when the installed updater predates it. The launcher runs
# `current`, so this is what makes a rollback actually take effect.
function Invoke-Rollback {
  # Not `$updater`: that is assigned later in the cycle, and the self-test path
  # reaches this function before it exists (box 3, 2026-11-14: "The expression
  # after '&' ... produced an object that was not valid").
  $updaterExe = Join-Path $BinDir 'gpn_update.exe'
  $lines = @()
  if (Test-Path -LiteralPath $updaterExe) {
    $lines = @(& $updaterExe --rollback 2>&1 | ForEach-Object { [string]$_ })
    if ($LASTEXITCODE -eq 0) { return @{ Ok = $true; Lines = $lines } }
  }

  $prevFile = Join-Path $InstallDir 'data\update\current.previous'
  if (-not (Test-Path -LiteralPath $prevFile)) { return @{ Ok = $false; Lines = $lines } }
  $prev = (Get-Content -LiteralPath $prevFile -Raw).Trim()
  if (-not (Test-Path -LiteralPath (Join-Path $prev 'gpn_node.exe'))) {
    return @{ Ok = $false; Lines = ($lines + 'rollback target ' + $prev + ' is not a version tree') }
  }
  $nowDir = $null
  try { $nowDir = @((Get-Item -LiteralPath $CurrentLink -Force).Target) | Select-Object -First 1 } catch { }

  # A junction is removed with rmdir: the target's contents are untouched.
  & cmd /c rmdir "$CurrentLink" 2>&1 | Out-Null
  & cmd /c mklink /J "$CurrentLink" "$prev" 2>&1 | Out-Null
  $ok = ($LASTEXITCODE -eq 0) -and (Test-Path -LiteralPath (Join-Path $CurrentLink 'gpn_node.exe'))
  if ($ok -and $nowDir) {
    Set-Content -LiteralPath $prevFile -Value ($nowDir + "`r`n") -Encoding ascii
  }
  return @{ Ok = $ok; Lines = ($lines + ('current -> ' + $prev)) }
}

# Optional ops self-test, never used by the scheduled task (which passes no
# arguments). It exists so the confirm/rollback half of the loop can be proven
# on a box that has nothing new to install:
#   update-and-restart.ps1 -SelfTest 0.3.46   -> confirm 0.3.46 is running
#   update-and-restart.ps1 -SelfTestRollback  -> flip `current` back one step
#     (no restart; the caller restarts and confirms, so a test can be undone)
if ($args.Count -ge 1 -and $args[0] -eq '-SelfTest') {
  $want = if ($args.Count -ge 2) { [string]$args[1] } else { Get-ExpectedVersion }
  $m = Confirm-Running -Expect $want -Seconds $ConfirmSecs
  if ($m) { Write-Log ('SELFTEST CONFIRMED: ' + $m); exit 0 }
  Write-Log ('SELFTEST NOT CONFIRMED: ' + $script:ConfirmReason)
  exit 4
}
if ($args.Count -ge 1 -and $args[0] -eq '-SelfTestRollback') {
  $rb = Invoke-Rollback
  foreach ($l in $rb.Lines) { if ($l.Trim()) { Write-Log ('selftest rollback: ' + $l.Trim()) } }
  Write-Log ('SELFTEST ROLLBACK ok=' + $rb.Ok + ' now=' + (Get-ExpectedVersion))
  if ($rb.Ok) { exit 0 } else { exit 1 }
}

# True once no GPN node process is left. gpn_update is excluded on purpose: this
# script is running it, and it is not the node.
function Wait-NodeExit {
  param([int]$Seconds)
  $deadline = (Get-Date).AddSeconds($Seconds)
  while ((Get-Date) -lt $deadline) {
    $left = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
      $_.ProcessName -like 'gpn*' -and $_.ProcessName -ne 'gpn_update' -and $_.Id -ne $PID
    })
    if ($left.Count -eq 0) { return $true }
    Start-Sleep -Milliseconds 500
  }
  return $false
}

# End the node AND its worker tree. `/End` alone only ends the task's own
# process (wscript.exe); the node is a grandchild
# (wscript -> cmd /c start-node.cmd -> gpn_node.exe -> children), so it survives.
# The restart that follows is then a no-op: hidden-launch.vbs refuses to start a
# second supervisor when it already sees gpn_node.exe. Result on box 1
# (2026-11-16 20:00): the "restart" booted nothing, the RPC still answered the
# old version, and the confirm gate rolled back while claiming to have retried.
function Stop-NodeTree {
  param([int]$GraceSeconds = 60)
  & schtasks /End /TN $NodeTask 2>&1 | Out-Null
  $endRc = $LASTEXITCODE
  if ($endRc -ne 0) { Write-Log ('schtasks /End exited ' + $endRc + ' - the node may not have been running') }
  if (Wait-NodeExit $GraceSeconds) {
    Write-Log 'GPN-Node stopped'
    return $true
  }
  Write-Log ('the node did not exit within ' + $GraceSeconds + ' s - ending the worker tree (taskkill /T /F)')
  foreach ($n in @('gpn_node.exe', 'gpn-node.exe')) {
    # [string] is load-bearing. On Windows PowerShell 5.1 a native command's
    # stderr reaches the pipeline as an ErrorRecord, which has no Trim(); the
    # second image name is normally already gone (it was a child of the first),
    # so taskkill answers "not found" on stderr and the unguarded $_.Trim() threw
    # INSIDE this function. Box 1, 2026-11-17: the cycle died here with no apply
    # and no restart, and the box was left with no node running at all.
    & taskkill /F /T /IM $n 2>&1 | ForEach-Object {
      $line = [string]$_
      if ($line.Trim()) { Write-Log ('taskkill ' + $n + ': ' + $line.Trim()) }
    }
  }
  Get-Process -ErrorAction SilentlyContinue | Where-Object {
    $_.ProcessName -like 'gpn*' -and $_.ProcessName -ne 'gpn_update' -and $_.Id -ne $PID
  } | ForEach-Object {
    try { Stop-Process -Id $_.Id -Force -ErrorAction Stop }
    catch { Write-Log ('could not stop ' + $_.ProcessName + ' (pid ' + $_.Id + '): ' + $_.Exception.Message + ' - it may be elevated') }
  }
  if (Wait-NodeExit 10) {
    Write-Log 'GPN-Node stopped (forced)'
    return $true
  }
  return $false
}

# One updater at a time: two instances would extract into the same versions
# directory and race on the `current` junction. The task also sets
# MultipleInstances=IgnoreNew, but a manual run can still overlap.
$lock = $null
try {
  $lock = [System.IO.File]::Open($LockPath, [System.IO.FileMode]::OpenOrCreate,
                                 [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
} catch {
  Write-Log 'another GPN-Update run is still in progress - exiting without changes'
  exit 0
}

try {
  Write-Log 'update check started'

  # Identity of this boot, for the updater's once-per-release attempt record.
  # Empty on failure, which makes the record fail open rather than block forever.
  try {
    $os = Get-CimInstance -ClassName Win32_OperatingSystem -ErrorAction Stop
    $env:GPN_BOOT_ID = 'win-' + $os.LastBootUpTime.ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
  } catch { $env:GPN_BOOT_ID = '' }

  # Heal the launcher first: a box whose launcher still execs bin\gpn_node.exe
  # would apply this update and then keep running the old build (box 3,
  # 2026-11-14). Idempotent, so doing it every cycle is free.
  Repair-Launcher

  # Run the updater from a private copy OUTSIDE the install dir. Windows locks a
  # running .exe, so an updater executed as C:\gpn\bin\gpn_update.exe can never
  # rename the new gpn_update.exe into place: apply failed on that last file,
  # `current` had already moved so the node ran the new build while the install
  # dir stayed MIXED, and the cycle reported FAILED every hour (box 3, 2026-11-15).
  # From a copy, every file in the target dir is replaceable.
  $updaterSrc = Join-Path $BinDir 'gpn_update.exe'
  if (-not (Test-Path -LiteralPath $updaterSrc)) {
    Show-UpdateFailure 1 'update check' ('gpn_update.exe is missing from ' + $BinDir)
    exit 1
  }
  $RunDir = Join-Path $InstallDir 'data\update\run'
  New-Item -ItemType Directory -Force -Path $RunDir | Out-Null
  $updater = Join-Path $RunDir 'gpn_update.exe'
  try {
    Copy-Item -LiteralPath $updaterSrc -Destination $updater -Force -ErrorAction Stop
  } catch {
    # A copy that cannot be refreshed is still usable if one is already there.
    if (-not (Test-Path -LiteralPath $updater)) {
      Show-UpdateFailure 1 'update check' ('could not stage the updater into ' + $RunDir + ': ' + $_.Exception.Message)
      exit 1
    }
    Write-Log ('could not refresh the updater copy (' + $_.Exception.Message + ') - using the existing one')
  }

  # Install into C:\gpn\bin and keep the version trees beside the node's own
  # state (the supervisor uses <base>\update too), not in a per-user folder.
  $env:GPN_UPDATE_DIR = Join-Path $InstallDir 'data\update'
  $env:GPN_UPDATE_TARGET_DIR = $BinDir

  $checkLines = & $updater --check 2>&1 | ForEach-Object { [string]$_ }
  $check = $LASTEXITCODE
  foreach ($l in $checkLines) { if ($l.Trim()) { Write-Log ('check: ' + $l.Trim()) } }

  if ($check -eq 0) {
    Write-Log 'up to date - nothing to do (node NOT restarted)'
    exit 0
  }
  if ($check -eq 2) {
    Write-Log 'release manifest unreachable - will retry on the next cycle (node NOT restarted)'
    exit 0
  }
  if ($check -ne 3) {
    Show-UpdateFailure $check 'update check' 'the signed manifest was rejected or could not be read'
    exit 1
  }

  Write-Log 'update available - stopping GPN-Node before applying (Windows locks a running .exe)'
  $stopped = Stop-NodeTree 60
  if (-not $stopped) {
    # Applying now would install the new tree, fail to start it (the old node
    # still owns the ports and the exe handles), and burn this boot's only
    # attempt on a rollback -- the loop this row exists to stop. Leave the
    # install untouched instead.
    Write-Log 'REFUSING to apply: gpn processes survived the stop, so the new build could not start.'
    Write-Log 'The install is unchanged and the current build keeps running. Cause: an elevated node (-RL HIGHEST) that this non-elevated task cannot kill; reboot, or run the update from an elevated shell.'
    & schtasks /Run /TN $NodeTask 2>&1 | Out-Null
    Show-UpdateFailure 1 'stop the node for the update' 'the node could not be stopped, so nothing was installed and this build keeps running'
    exit 1
  }

  $applyLines = & $updater --apply 2>&1 | ForEach-Object { [string]$_ }
  $apply = $LASTEXITCODE
  foreach ($l in $applyLines) { if ($l.Trim()) { Write-Log ('apply: ' + $l.Trim()) } }

  # ALWAYS start the node again -- even when the update failed. Restarting
  # before any dialog also means a modal message box can never keep it down.
  & schtasks /Run /TN $NodeTask 2>&1 | Out-Null
  $runRc = $LASTEXITCODE
  if ($runRc -eq 0) {
    Write-Log 'GPN-Node started'
  } else {
    Write-Log ('could not start GPN-Node (schtasks /Run exited ' + $runRc + ') - start it by hand: schtasks /Run /TN GPN-Node')
    Show-UpdateFailure $runRc 'start GPN-Node' 'the node was stopped for the update but could not be started again'
  }

  if ($apply -eq 3) {
    Write-Log 'update declined (policy, or already attempted on this boot) - node restarted on the current build'
    exit 0
  }
  if ($apply -ne 0) {
    Show-UpdateFailure $apply 'gpn_update --apply' 'the new release was not installed; the node is back on the previous build'
    exit 1
  }

  # "Applied" is a statement about files on disk. The update is DONE only when
  # the running node reports the installed version -- download, verify, stop,
  # switch, start, CONFIRM is the whole cycle.
  $expect = Get-ExpectedVersion
  $confirm = Confirm-Running -Expect $expect -Seconds $ConfirmSecs
  if ($confirm) {
    Write-Log ('CONFIRMED: ' + $confirm)
    Write-Log 'update cycle complete'
    exit 0
  }

  Write-Log ('NOT CONFIRMED: ' + $script:ConfirmReason)
  Write-Log 'rolling back to the previous version and restarting it - a box must not be left on a build that does not come up'
  $rb = Invoke-Rollback
  foreach ($l in $rb.Lines) { if ($l.Trim()) { Write-Log ('rollback: ' + $l.Trim()) } }
  if (-not $rb.Ok) {
    Write-Log 'rollback failed - restarting whatever is installed'
  }
  & schtasks /End /TN $NodeTask 2>&1 | Out-Null
  if (Wait-NodeExit 60) { Write-Log 'node stopped for the rollback' }
  else {
    Write-Log 'the node survived the stop for the rollback - ending the worker tree'
    if (-not (Stop-NodeTree 10)) { Write-Log 'WARNING: the node is still running; the rollback restart may not take effect' }
  }
  & schtasks /Run /TN $NodeTask 2>&1 | Out-Null
  $back = Get-ExpectedVersion
  $backMsg = Confirm-Running -Expect $back -Seconds $ConfirmSecs
  if ($backMsg) { Write-Log ('CONFIRMED after rollback: ' + $backMsg) }
  else { Write-Log ('WARNING: even the previous build did not confirm: ' + $script:ConfirmReason) }
  Show-UpdateFailure 1 'confirm the new version' $script:ConfirmReason
  exit 1
}
catch {
  # The header promises "a failed update still restarts: a box with nothing
  # running is worse than a box on the previous build". A terminating error is
  # the one path that used to break that promise (box 1, 2026-11-17: a crash in
  # Stop-NodeTree left the box with no node at all). Make it mechanical.
  Write-Log ('UNEXPECTED: ' + $_.Exception.Message)
  Write-Log ('at: ' + $_.InvocationInfo.PositionMessage)
  try { & schtasks /Run /TN $NodeTask 2>&1 | Out-Null } catch { }
  Write-Log 'GPN-Node started (recovery from an unexpected error)'
  exit 1
}
finally {
  if ($lock) { $lock.Close() }
}
'@

# The task XML is used instead of `schtasks /SC DAILY /RI 360` for one reason:
# the CLI cannot express jitter. The ScheduledTasks module's
# `New-ScheduledTaskTrigger -RandomDelay` only dithers the INITIAL StartBoundary;
# `<RandomDelay>` inside `<Repetition>` is what Task Scheduler applies to every
# repetition (hourly during the 2026-11-12 review window). Writing the XML directly works on stock Windows 10/11,
# needs no module, and (with /F) replaces the task wholesale, so re-running the
# installer cannot leave a second task or a duplicate trigger behind.
$UpdateTaskXml = @'
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>GPN auto-update: checks the signed release manifest every hour (temporary review cadence) and, when a newer release exists, stops GPN-Node, applies the update and starts the node again.</Description>
    <URI>\GPN-Update</URI>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>2020-01-01T00:00:00</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByDay>
        <DaysInterval>1</DaysInterval>
      </ScheduleByDay>
      <Repetition>
        <Interval>PT1H</Interval>
        <Duration>P1D</Duration>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <!-- Per-repetition jitter is NOT expressible in Task Scheduler XML.
           `<RandomDelay>` is only legal directly under a trigger, and the
           schema rejects it inside `<Repetition>`:
             (18,11):RandomDelay:  ->  "the task XML contains an unexpected node"
           That single element is why GPN-Update answered "Access is denied /
           task XML is malformed" and never registered on any box, so every
           box fell back to the logon-time PowerShell loop. The jitter that
           matters (spreading downloads) is done by the node's own spread seed,
           so this only needs to dither the daily activation. -->
      <RandomDelay>PT10M</RandomDelay>
    </CalendarTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>__USER__</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT2H</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>powershell.exe</Command>
      <Arguments>-NoProfile -ExecutionPolicy Bypass -File "__SCRIPT__"</Arguments>
    </Exec>
  </Actions>
</Task>
'@

function Write-UpdateWrapper {
  $text = $UpdateWrapperTemplate.Replace('__INSTALL_DIR__', $InstallDir)
  Invoke-Step "write $UpdateScript" {
    Set-Content -Path $UpdateScript -Value $text -Encoding ascii
  }
  Say "auto-update wrapper written: $UpdateScript"
}

function Register-UpdateTask {
  $user = ''
  try { $user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name } catch { }
  if (-not $user) { $user = "$env:USERDOMAIN\$env:USERNAME" }
  $xmlText = $UpdateTaskXml.Replace('__SCRIPT__', $UpdateScript).Replace(
    '__USER__', [System.Security.SecurityElement]::Escape($user))

  if ($WhatIf) {
    Write-Host "[whatif] register scheduled task $UpdateTask (every 1h, random delay 10m) -> $UpdateScript" -ForegroundColor Cyan
    return $true
  }

  # The scheduled task can only express one install per task NAME. A second
  # install on the same box must not silently repoint the first one's autostart.
  $foreignUpdate = Get-ForeignTaskDir $UpdateTask
  if ($foreignUpdate) {
    Warn "$UpdateTask belongs to the install at '$foreignUpdate' -- NOT touching it (this install is $InstallDir)"
    Warn 'This install will not auto-update until the other install is removed or its task is renamed.'
    return $false
  }

  $xmlPath = Join-Path $env:TEMP 'gpn-update-task.xml'
  Set-Content -Path $xmlPath -Value $xmlText -Encoding Unicode
  $rc = Invoke-Schtasks @('/Create', '/TN', $UpdateTask, '/XML', $xmlPath, '/F')
  Remove-Item -Force $xmlPath -ErrorAction SilentlyContinue
  if ($rc -ne 0) {
    Warn "could not register $UpdateTask (rc=$rc) -- this box will NOT auto-update; re-run install.ps1 as the logged-in user (not elevated, and not as another account)"
    return $false
  }
  Say "scheduled task $UpdateTask registered (every 1h +/- 10 min, MultipleInstances=IgnoreNew)"
  return $true
}

# -- Update fallback: a Startup shortcut when Task Scheduler refuses ---------
# 2026-11-08: on box 2 and box 4 (and box 1) `schtasks /Create` answers "Access
# is denied" for a standard account, so GPN-Update never registers and the box
# silently never updates -- every fix this week then needed a human to reinstall
# by hand. The node's own in-process updater covers the case where it is running
# at the check hour, but it cannot cover a machine that is switched on later.
# A per-user Startup shortcut needs no privileges and does the same job: run the
# existing wrapper, sleep ~1 h with jitter, repeat (temporary review cadence).
function Write-UpdateFallback {
  if ($WhatIf) {
    Write-Host "[whatif] write update loop + Startup shortcut (no Task Scheduler needed)" -ForegroundColor Cyan
    return $true
  }
  $loop = Join-Path $InstallDir 'update-loop.ps1'
  $body = @'
# update-loop.ps1 -- generated by install.ps1; do not hand-edit.
# Fallback for accounts that cannot register the GPN-Update scheduled task.
# Runs the same wrapper the task would run, then sleeps ~1 h (+/- 10 min jitter,
# so a fleet started together does not stampede the release host).
# TEMPORARY hourly cadence (2026-11-12): reverts with DEFAULT_CHECK_HOURS.
$wrapper = Join-Path $PSScriptRoot 'update-and-restart.ps1'
$log = Join-Path $PSScriptRoot 'update-loop.log'
function Note($m) { try { Add-Content -Path $log -Value ((Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + ' ' + $m) } catch { } }
# The Startup shortcut can start this at logon while one is already running
# (the box was fixed by hand / an older loop survived). One loop is the point:
# two would double the checks and the restarts.
try {
  $me = [System.Diagnostics.Process]::GetCurrentProcess().Id
  $other = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" -ErrorAction Stop |
    Where-Object { $_.CommandLine -like '*update-loop.ps1*' -and $_.ProcessId -ne $me })
  if ($other.Count -gt 0) {
    Note ('another update loop is already running (pid ' + $other[0].ProcessId + ') - exiting')
    exit 0
  }
} catch { }
Note 'update loop started (Startup shortcut; Task Scheduler unavailable)'
while ($true) {
  try {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $wrapper | Out-Null
    Note ('wrapper exit ' + $LASTEXITCODE)
  } catch { Note ('wrapper failed: ' + $_.Exception.Message) }
  $sleep = 1 * 3600 + (Get-Random -Minimum -600 -Maximum 601)
  Note ('next update check in ' + [int]($sleep / 60) + ' min')
  Start-Sleep -Seconds $sleep
}
'@
  Invoke-Step "write $loop" { Set-Content -Path $loop -Value $body -Encoding ascii }

  $lnk = Join-Path ([Environment]::GetFolderPath('Startup')) 'GPN Update.lnk'
  try {
    $ws = New-Object -ComObject WScript.Shell
    $sc = $ws.CreateShortcut($lnk)
    $sc.TargetPath = 'powershell.exe'
    $sc.Arguments = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File ""$loop"""
    $sc.WorkingDirectory = $InstallDir
    $sc.WindowStyle = 7
    $sc.Description = 'GPN auto-update (Startup shortcut: the scheduled task could not be registered)'
    $sc.Save()
    Say "auto-update via Startup shortcut: $lnk (runs at logon, then ~6-hourly)"
    return $true
  } catch {
    Warn ("could not write the update Startup shortcut: " + $_.Exception.Message)
    return $false
  }
}

function Test-NodeUp {
  try {
    Invoke-RestMethod 'http://127.0.0.1:8099/' -TimeoutSec 2 | Out-Null
    return $true
  } catch { }
  try {
    $body = '{"jsonrpc":"2.0","id":1,"method":"system_health","params":[]}'
    Invoke-RestMethod -Uri 'http://127.0.0.1:9944' -Method Post -Body $body `
      -ContentType 'application/json' -TimeoutSec 2 | Out-Null
    return $true
  } catch { }
  return $false
}

function Wait-NodeUp {
  param([int]$Seconds = 90)
  $deadline = (Get-Date).AddSeconds($Seconds)
  while ((Get-Date) -lt $deadline) {
    if (Test-NodeUp) { return $true }
    Start-Sleep -Seconds 2
  }
  return $false
}

# Verify the whole point of the task: a simulated reboot brings the node back
# with no user action. Run/up -> End/Ready -> Run/up again.
function Verify-NodeTask {
  Say 'verifying autostart: /Run -> up -> /End -> Ready -> /Run -> up'
  $ok = $true
  $rc = Invoke-Schtasks @('/Run', '/TN', $NodeTask)
  if ($rc -ne 0) { Warn "schtasks /Run failed (rc=$rc)"; $ok = $false }
  elseif (-not (Wait-NodeUp -Seconds 120)) { Warn 'the node did not come up after /Run'; $ok = $false }
  else { Say 'node is up after /Run' }

  $rc = Invoke-Schtasks @('/End', '/TN', $NodeTask)
  if ($rc -ne 0) { Warn "schtasks /End failed (rc=$rc)"; $ok = $false }
  Start-Sleep -Seconds 2
  $state = ''
  try { $state = (Get-ScheduledTask -TaskName $NodeTask -ErrorAction Stop).State.ToString() } catch { }
  if ($state -and $state -ne 'Ready') { Warn "task state after /End is '$state', expected Ready"; $ok = $false }
  elseif (-not $state) { Say 'task state check skipped (ScheduledTasks module unavailable)' }
  else { Say 'task state after /End: Ready' }

  $rc = Invoke-Schtasks @('/Run', '/TN', $NodeTask)
  if ($rc -ne 0) { Warn "second /Run failed (rc=$rc)"; $ok = $false }
  elseif (-not (Wait-NodeUp -Seconds 120)) { Warn 'the node did not come back after the second /Run'; $ok = $false }
  else { Say 'node is up again after the simulated reboot' }

  if ($ok) { Say 'autostart verification PASSED' }
  else { Warn 'autostart verification FAILED (see the lines above)' }
  return $ok
}

# ------------------------------------------------------------------ tasks ----
$script:NodeAutostart = 'none'   # 'task' | 'startup' | 'none'
# Does an existing scheduled task belong to a DIFFERENT install directory?
#
# 2026-11-16, box 1: installing into `C:\gpn-ci` re-pointed the live `GPN-Node`
# from `C:\gpn\hidden-launch.vbs` at `C:\gpn-ci\hidden-launch.vbs` (and
# `GPN-Update` likewise), because the reuse test only compared the run-as user
# (`Jeremy` vs `Jeremy Guo` did not match) and the fallback deleted and recreated
# the task. A second install on a box must never take over the first one's
# autostart: that silently stops the node the operator was watching.
#
# Returns the foreign install dir, or $null when the task is ours/absent.
function Get-ForeignTaskDir {
  param([string]$TaskName)
  $q = (& cmd /c "schtasks /Query /TN $TaskName /FO LIST /V 2>nul") -join "`n"
  if (-not $q -or $q -notmatch 'Task To Run:\s*(.+?)\r?\n') { return $null }
  $tr = $Matches[1]
  $path = $null
  if ($tr -match '([A-Za-z]:\\[^"'']*?hidden-launch\.vbs)') { $path = $Matches[1] }
  elseif ($tr -match '([A-Za-z]:\\[^"'']*?start-node\.cmd)') { $path = $Matches[1] }
  elseif ($tr -match '([A-Za-z]:\\[^"'']*?update-and-restart\.ps1)') { $path = $Matches[1] }
  if (-not $path) { return $null }
  $dir = (Split-Path -Parent $path).TrimEnd('\')
  if ($dir -eq $InstallDir.TrimEnd('\')) { return $null }
  return $dir
}

function Register-NodeTask {
  # Default: create the task at NORMAL privilege. The node binds only high ports
  # (30333/9944/8093/8094/8099), so it needs no admin rights -- and asking for
  # them (/RL HIGHEST) makes the create fail with rc=1 "Access is denied" from a
  # non-elevated shell, which is what broke the stock install on box 3. Elevated
  # installs used to also produce a task owned by Administrator that never fires
  # for the user who actually logs in. Opt in with -RunHighest if you want it.
  $createArgs = @('/Create', '/TN', $NodeTask, '/TR', "wscript.exe `"$HiddenLaunch`"", '/SC', 'ONLOGON', '/F')
  if ($RunHighest) { $createArgs += @('/RL', 'HIGHEST') }
  Invoke-Step "schtasks /Create /TN $NodeTask (/SC ONLOGON$(if ($RunHighest) { ' /RL HIGHEST' }))" {
    $foreign = Get-ForeignTaskDir $NodeTask
    if ($foreign) {
      Warn "$NodeTask belongs to the install at '$foreign' -- NOT touching it (this install is $InstallDir)"
      Warn "Register the autostart from that install, or uninstall it first. Nothing was changed."
      $script:NodeAutostart = 'none'
      return
    }
    # Reuse an existing task that already runs our launcher. A standard (non-admin)
    # Windows account gets "Access is denied" from /Create even when the task is
    # its own (measured 2026-11-04: `schtasks /Create /TN gpntest ... /F` fails on a
    # fresh box), so recreating what already works is both unnecessary and fatal.
    $existing = (& cmd /c "schtasks /Query /TN $NodeTask /FO LIST /V 2>nul") -join "`n"
    $existingTr = ''
    if ($existing -match 'Task To Run:\s*(.+?)\r?\n') { $existingTr = $Matches[1] }
    if ($existingTr -match 'hidden-launch\.vbs' -or $existingTr -match 'start-node\.cmd') {
      $ownerX = ''
      if ($existing -match 'Run As User:\s*(\S+)') { $ownerX = $Matches[1] }
      if (-not $ownerX -or ($ownerX -match [regex]::Escape($env:USERNAME))) {
        if ($existingTr -match 'hidden-launch\.vbs') {
          Say "$NodeTask already exists and runs the hidden launcher -- reusing it (not recreating)"
          $script:NodeAutostart = 'task'
          return
        }
        # An older task runs `cmd /c start-node.cmd --task` directly, which puts
        # a VISIBLE console on the user's desktop and dies when it is closed
        # (exit -1073741510). Retarget it at wscript.exe + hidden-launch.vbs.
        $retargeted = $false
        if (-not $WhatIf) {
          $tr = "wscript.exe `"$HiddenLaunch`""
          try {
            $a = New-ScheduledTaskAction -Execute 'wscript.exe' -Argument "`"$HiddenLaunch`""
            Set-ScheduledTask -TaskName $NodeTask -Action $a -ErrorAction Stop | Out-Null
            $retargeted = $true
          } catch { }
          if (-not $retargeted) {
            $rc2 = Invoke-Schtasks @('/Change', '/TN', $NodeTask, '/TR', $tr)
            $retargeted = ($rc2 -eq 0)
          }
          if (-not $retargeted) {
            $rc2 = Invoke-Schtasks @('/Create', '/TN', $NodeTask, '/TR', $tr, '/SC', 'ONLOGON', '/F')
            $retargeted = ($rc2 -eq 0)
          }
        }
        if ($retargeted -or $WhatIf) {
          Say "$NodeTask retargeted to the hidden launcher (no console window)"
          $script:NodeAutostart = 'task'
          return
        }
        Warn "could not retarget $NodeTask to the hidden launcher -- leaving it as-is"
      }
    }

    # A leftover task owned by ANOTHER account cannot be replaced by this one.
    # Try to clear it first (works when it is ours or we are elevated).
    $q = (& cmd /c "schtasks /Query /TN $NodeTask /FO LIST /V 2>nul") -join "`n"
    $owner = ''
    if ($q -match 'Run As User:\s*(\S+)') { $owner = $Matches[1] }
    if ($owner -and ($owner -notmatch [regex]::Escape($env:USERNAME))) {
      [void](Invoke-Schtasks @('/Delete', '/TN', $NodeTask, '/F'))
    }

    $rc = Invoke-Schtasks $createArgs
    if ($rc -ne 0 -and $RunHighest) {
      # Elevation was requested but is unavailable: the node does not need it.
      Warn "/RL HIGHEST was refused (no elevation) -- retrying at normal privilege"
      $plain = @('/Create', '/TN', $NodeTask, '/TR', "wscript.exe `"$HiddenLaunch`"", '/SC', 'ONLOGON', '/F')
      $rc = Invoke-Schtasks $plain
    }
    if ($rc -eq 0) { $script:NodeAutostart = 'task' }
    if ($rc -ne 0) {
      $q2 = (& cmd /c "schtasks /Query /TN $NodeTask /FO LIST /V 2>nul") -join "`n"
      $owner2 = ''
      if ($q2 -match 'Run As User:\s*(\S+)') { $owner2 = $Matches[1] }
      if ($owner2 -and ($owner2 -notmatch [regex]::Escape($env:USERNAME))) {
        # The task belongs to ANOTHER account (box 1, 2026-11-04: task owned by
        # 'Jeremy', install run as 'Jeremy Guo'). Deleting it needs elevation,
        # which we refuse to require -- so fall back to a per-user Startup
        # shortcut, which needs none and starts this node at logon regardless.
        $lnk = Join-Path ([Environment]::GetFolderPath('Startup')) 'GPN Node.lnk'
        try {
          $ws = New-Object -ComObject WScript.Shell
          $sc = $ws.CreateShortcut($lnk)
          $sc.TargetPath = 'powershell.exe'
          $sc.Arguments = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$(Join-Path $InstallDir 'start-gpn.ps1')`" -NoWait"
          $sc.WorkingDirectory = $InstallDir
          $sc.WindowStyle = 7
          $sc.Description = 'GPN node (Startup shortcut: the scheduled task belongs to another account)'
          $sc.Save()
          $script:NodeAutostart = 'startup'
          Warn ("$NodeTask belongs to '$owner2', not '$env:USERNAME' -- left it alone and " +
                "registered a Startup shortcut instead: $lnk")
          Warn ("If '$owner2' also logs in here, its task will start a SECOND node on the " +
                "same ports. Remove it once, in an ADMIN prompt: schtasks /Delete /TN $NodeTask /F")
          return
        } catch {
          throw ("$NodeTask belongs to '$owner2', not '$env:USERNAME', and the Startup " +
                 "shortcut could not be written: $($_.Exception.Message). " +
                 "In an ADMIN prompt run: schtasks /Delete /TN $NodeTask /F")
        }
      }
      $why = if ($script:LastSchtasksOutput) { ": $script:LastSchtasksOutput" } else { '' }
      # Standard accounts cannot create scheduled tasks at all ("Access is denied").
      # A Startup-folder shortcut achieves the same "start at logon" without any
      # privilege, so an install must not die here.
      $lnk = Join-Path ([Environment]::GetFolderPath('Startup')) 'GPN Node.lnk'
      try {
        $ws = New-Object -ComObject WScript.Shell
        $sc = $ws.CreateShortcut($lnk)
        $sc.TargetPath = 'powershell.exe'
        $sc.Arguments = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$(Join-Path $InstallDir 'start-gpn.ps1')`" -NoWait"
        $sc.WorkingDirectory = $InstallDir
        $sc.WindowStyle = 7
        $sc.Description = 'GPN node (started by the Startup folder because Task Scheduler was denied)'
        $sc.Save()
        $script:NodeAutostart = 'startup'
        Warn "could not create the scheduled task (rc=$rc$why) -- using a Startup shortcut instead: $lnk"
        return
      } catch {
        throw "schtasks /Create $NodeTask failed (rc=$rc)$why; the Startup shortcut also failed: $($_.Exception.Message)"
      }
    }
  }
  if ($WhatIf) { return }

  # ONE supervisor only: /Run on an already-running task would fork a second
  # node on the same base path and the same ports (2026-10-23, ~2h lost).
  $miSet = $false
  if ($script:NodeAutostart -ne 'task') { return }   # nothing registered -> nothing to tune
  try {
    $st = Get-ScheduledTask -TaskName $NodeTask -ErrorAction Stop
    $st.Settings.MultipleInstances = 'IgnoreNew'
    Set-ScheduledTask -TaskName $NodeTask -Settings $st.Settings -ErrorAction Stop | Out-Null
    $miSet = ((Get-ScheduledTask -TaskName $NodeTask).Settings.MultipleInstances -eq 'IgnoreNew')
  } catch { Warn "MultipleInstances via ScheduledTasks module failed: $($_.Exception.Message)" }

  if (-not $miSet) {
    # Fallback for hosts without the ScheduledTasks module: rewrite the XML.
    try {
      $xml = Join-Path $env:TEMP 'gpn-node-task.xml'
      $raw = (& cmd /c "schtasks /Query /TN $NodeTask /XML") -join "`r`n"
      $doc = New-Object System.Xml.XmlDocument
      $doc.LoadXml(($raw -replace '(?s)^\s*<\?xml[^>]*\?>', ''))
      $node = $doc.SelectSingleNode('/Task/Settings/MultipleInstancesPolicy')
      if (-not $node) { throw 'no MultipleInstancesPolicy in the task XML' }
      $node.InnerText = 'IgnoreNew'
      $doc.InsertBefore($doc.CreateXmlDeclaration('1.0','UTF-16',$null), $doc.DocumentElement) | Out-Null
      $doc.Save($xml)
      $rc = Invoke-Schtasks @('/Create', '/TN', $NodeTask, '/XML', $xml, '/F')
      if ($rc -ne 0) { throw "schtasks /Create (XML) failed (rc=$rc)" }
      $miSet = $true
    } catch { Warn "could not set MultipleInstances (a manual /Run could fork a second tree): $($_.Exception.Message)" }
  }
  if ($miSet) { Say "$NodeTask MultipleInstances = IgnoreNew" }
}

# -------------------------------------------------------------- referral ---
# `--ref CODE`: record the referring install's code, ONCE, in the same file the
# node reads ($GPN_PREFIX/referral, default %USERPROFILE%\.gpn\referral -- see
# gpn-agent/src/referral.rs). Mirrors install.sh --ref exactly: write-once (an
# existing code is never rewritten, so attribution cannot be changed after the
# fact), validation identical (4-32 chars of [A-Za-z0-9_-]), and a bad code is a
# WARNING that never fails the install.
function Test-ReferralCode {
  param([string]$Code)
  # Returns $null when valid, else the reason (same wording as install.sh).
  if ($null -eq $Code) { return 'empty referral code' }
  $c = $Code.Trim()
  if ($c.Length -eq 0) { return 'empty referral code' }
  if ($c.Length -lt 4)  { return 'referral code too short (need 4-32 chars)' }
  if ($c.Length -gt 32) { return 'referral code too long (need 4-32 chars)' }
  if ($c -notmatch '^[A-Za-z0-9_-]+$') {
    return "referral code may only contain letters, digits, '-' and '_'"
  }
  return $null
}

function Set-Referral {
  param([string]$Code)
  if ([string]::IsNullOrWhiteSpace($Code)) { return }
  $why = Test-ReferralCode $Code
  if ($why) { Warn "referral code ignored: $why"; return }
  # Same prefix resolution as the node (gpn-agent/src/referral.rs::prefix_dir):
  # GPN_PREFIX, else HOME/USERPROFILE + .gpn. NOT $InstallDir -- the node reads
  # the referral from its prefix, so writing it anywhere else loses it.
  $prefix = ''
  if ($env:GPN_PREFIX) { $prefix = $env:GPN_PREFIX }
  elseif ($env:HOME) { $prefix = Join-Path $env:HOME '.gpn' }
  elseif ($env:USERPROFILE) { $prefix = Join-Path $env:USERPROFILE '.gpn' }
  else { $prefix = Join-Path '.' '.gpn' }
  $file = Join-Path $prefix 'referral'
  if (Test-Path $file) {
    $existing = (Get-Content $file -TotalCount 1)
    Say "[referral] already set to '$existing' in $file -- keeping it (write-once)"
    return
  }
  if ($WhatIf) {
    Write-Host "[whatif] would record referral code '$($Code.Trim())' in $file" -ForegroundColor Cyan
    return
  }
  try {
    New-Item -ItemType Directory -Force -Path $prefix | Out-Null
    # CreateNew = O_EXCL create (same as run-windows-provider.ps1): it throws if
    # the file appeared since the check above, so a racing run cannot rewrite an
    # existing code -- the first value always wins.
    $fs = [System.IO.File]::Open($file, [System.IO.FileMode]::CreateNew,
                                 [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
      # UTF-8 without BOM, one line: the node reads this file verbatim.
      $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes("$($Code.Trim())`n")
      $fs.Write($bytes, 0, $bytes.Length)
      $fs.Flush()
    } finally { $fs.Dispose() }
    Say "referral code recorded: '$($Code.Trim())' -> $file (write-once)"
  } catch [System.IO.IOException] {
    $existing = (Get-Content $file -TotalCount 1)
    Say "[referral] already set to '$existing' in $file -- keeping it (write-once)"
  } catch {
    Warn "[referral] could not write $file ($($_.Exception.Message)) -- continuing without a referral"
  }
}

# ----------------------------------------------------------------- install ---

# -- Fast sync (weekly snapshot) ----------------------------------------------
# Mirrors install.sh. A fresh Windows install otherwise re-verifies every block
# from genesis including its Ethash seal (hours on a growing chain), and a node
# that crawls is also the node most likely to settle on another laggard
# (2026-11-15: a newcomer sat at #69,295 against a ~#307,300 tip). So the
# default is to import the published weekly snapshot, verified against a
# *signed* metadata document before a single byte is trusted. It is opt-OUT
# (--no-fast-sync / GPN_FAST_SYNC=0), and every failure -- no metadata, bad
# signature, 404, short disk, hash mismatch, import error -- falls back to the
# normal full peer sync. It is never an install failure.
$SnapshotUrl = if ($env:GPN_SNAPSHOT_URL) { $env:GPN_SNAPSHOT_URL } else {
  'https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/gpn-testnet-snapshot.tar.gz'
}
$SnapshotMetaUrl = if ($env:GPN_SNAPSHOT_META_URL) { $env:GPN_SNAPSHOT_META_URL } else {
  'https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json'
}

function Invoke-FastSyncFallback {
  param([string]$Reason)
  Warn "fast sync unavailable ($Reason) -- falling back to a normal full peer sync (slower, still correct)."
}

function Invoke-FastSync {
  if (-not $FastSync) {
    Say 'fast sync disabled (--no-fast-sync) -- the node will sync from peers.'
    return
  }
  # Never touch an existing chain DB: a DB made with a different pruning mode
  # fails to open ("Incompatible pruning modes"), so importing over it would
  # break a working install. Same rule as install.sh.
  $chains = Join-Path $DataDir 'chains'
  if ((Test-Path $chains) -and (Get-ChildItem -LiteralPath $chains -Force -ErrorAction SilentlyContinue)) {
    if (Test-Path (Join-Path $DataDir '.gpn-pruning-archive')) {
      Say "found an existing chain DB at $DataDir -- fast sync not needed (leaving it untouched)."
    } else {
      Warn "found an existing chain DB at $DataDir with no archive-pruning marker -- NOT touching it; fast sync skipped."
    }
    return
  }
  $snap = Join-Path $BinDir 'gpn_snapshot.exe'
  if (-not (Test-Path $snap)) { Invoke-FastSyncFallback 'gpn_snapshot.exe not installed'; return }

  $work = Join-Path $InstallDir ('.fast-sync-' + [System.Guid]::NewGuid().ToString('N').Substring(0, 8))
  try {
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    $meta  = Join-Path $work 'snapshot.json'
    $vout  = Join-Path $work 'verify.txt'
    $arch  = Join-Path $work 'snap.tar.gz'

    Say 'fetching signed snapshot metadata...'
    try {
      Invoke-WebRequest -Uri $SnapshotMetaUrl -OutFile $meta -UseBasicParsing -TimeoutSec 60 | Out-Null
    } catch {
      Invoke-FastSyncFallback "snapshot metadata not available at $SnapshotMetaUrl ($($_.Exception.Message))"
      return
    }
    # Signature first: an unsigned or edited document is never trusted, so the
    # size/hash it advertises is never used.
    $out = & $snap verify --meta $meta 2>&1 | Out-String
    $out | Set-Content -LiteralPath $vout -Encoding UTF8
    if ($LASTEXITCODE -ne 0) {
      Invoke-FastSyncFallback "snapshot metadata failed verification: $(($out -split "`n" | Where-Object { $_.Trim() } | Select-Object -Last 1))"
      return
    }
    $snapBlock = ($out -split "`n" | Where-Object { $_ -match '^block=' } | Select-Object -First 1) -replace '^block=',''
    $snapSize  = ($out -split "`n" | Where-Object { $_ -match '^size=' }  | Select-Object -First 1) -replace '^size=',''
    if (-not ($snapSize -as [int64])) { Invoke-FastSyncFallback 'signed metadata has no usable size'; return }
    $snapSize = [int64]$snapSize

    Say "weekly snapshot: block $(if ($snapBlock) { $snapBlock } else { '?' }), $([math]::Round($snapSize/1MB,0)) MiB -- downloading..."

    # Disk guard: the archive plus the unpacked DB needs room; require 3x.
    try {
      $drive = (Get-Item -LiteralPath $InstallDir).PSDrive
      if ($drive -and $drive.Free -gt 0 -and $drive.Free -lt ($snapSize * 3)) {
        Invoke-FastSyncFallback "only $([math]::Round($drive.Free/1MB,0)) MiB free, need ~$([math]::Round($snapSize*3/1MB,0)) MiB (3x snapshot)"
        return
      }
    } catch { }

    if ($WhatIf) {
      Write-Host "[whatif] would download $SnapshotUrl and import it into $DataDir" -ForegroundColor Cyan
      return
    }
    try {
      Invoke-WebRequest -Uri $SnapshotUrl -OutFile $arch -UseBasicParsing -TimeoutSec 3600 | Out-Null
    } catch {
      Invoke-FastSyncFallback "snapshot download failed ($($_.Exception.Message))"
      return
    }
    # Verify the *bytes on disk* against the signed size + sha256.
    $out = & $snap verify --meta $meta --snapshot $arch 2>&1 | Out-String
    $out | Set-Content -LiteralPath $vout -Encoding UTF8
    if ($LASTEXITCODE -ne 0) {
      Invoke-FastSyncFallback "snapshot failed verification: $(($out -split "`n" | Where-Object { $_.Trim() } | Select-Object -Last 1))"
      return
    }

    # Import into a STAGING dir, never over the real base path: a failure then
    # has nothing to undo except our own temp dir.
    $stage = Join-Path $work 'stage'
    Say 'importing snapshot -- the manifest decides the pruning mode -- a few minutes...'
    $out = & $snap import --snapshot $arch --base-path $stage 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $stage 'chains'))) {
      Invoke-FastSyncFallback "snapshot import failed: $(($out -split "`n" | Where-Object { $_.Trim() } | Select-Object -Last 1))"
      return
    }
    New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
    if (Test-Path $chains) { Invoke-FastSyncFallback "$chains appeared during import -- not overwriting it"; return }
    Move-Item -LiteralPath (Join-Path $stage 'chains') -Destination $chains
    $marker = Join-Path $stage '.gpn-pruning-archive'
    if (Test-Path $marker) { Copy-Item -LiteralPath $marker -Destination $DataDir -Force }
    Say "bootstrapped from the weekly snapshot at block $(if ($snapBlock) { $snapBlock } else { '?' }) -- skipping the full peer sync."
  } catch {
    Invoke-FastSyncFallback "unexpected error: $($_.Exception.Message)"
  } finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
  }
}
if ($Uninstall) { Invoke-Uninstall; exit 0 }

# Deploy-step mode: only the auto-update task. The release workflow runs this on
# every deploy so a box installed before GPN-Update existed heals itself; it
# touches no binaries, no ollama and not the running node.
if ($UpdateTaskOnly) {
  Say "registering only the auto-update task in $InstallDir"
  Write-UpdateWrapper
  # Keep the user-facing start path and its startup summary current on a box
  # that was deployed (not reinstalled): the launcher is what users double-click.
  Write-StartGpn
  $ok = Register-UpdateTask
  if (-not $ok) {
    # Task Scheduler refuses a standard account ("Access is denied", seen on
    # boxes 1/2/4) and cannot be asked for an interactive token from an SSH
    # session. A per-user Startup shortcut needs no privileges and runs the
    # same wrapper on the same cadence, so the box must not be left with no
    # updater at all -- that is how box 3 sat on 0.3.45 for eight hours.
    Say 'falling back to the Startup-shortcut update loop'
    $ok = Write-UpdateFallback
  }
  if ($WhatIf) { Write-Host '[whatif] would not start or stop the node' -ForegroundColor Cyan }
  try { Stop-Transcript | Out-Null } catch { }
  # This mode is best-effort BY DESIGN: it runs inside the release build, whose
  # real job is to produce artifacts. Failing it because a *scheduled task* could
  # not be registered turns a cosmetic gap into a red build that blocks the
  # release for everyone (2026-11-03). Warn, and let the build finish.
  if (-not $ok) {
    Warn "auto-update task not registered on this runner -- the release is unaffected"
  }
  exit 0
}

Say "GPN installer -- installing from $Root into $InstallDir"
# 0. referral: before anything can fail, so a referred operator who hits an
#    unrelated install error still keeps the attribution.
Set-Referral $RefCode
# `gpn_snapshot.exe` is not optional: the node supervisor uses it to repair a
# forked chain by itself (#157). An install without it produces a node that
# cannot heal and only says so in a log line, so refuse it here instead.
foreach ($needed in @('gpn_node.exe', 'gpn-node.exe', 'gpn_snapshot.exe')) {
  if (-not (Test-Path (Join-Path $Root $needed))) {
    $msg = "$needed is not next to install.ps1 ($Root). Extract the whole release zip first."
    # -WhatIf is for dry runs (and for CI on non-Windows hosts), so it reports
    # the problem instead of aborting before it has printed a plan.
    if ($WhatIf) { Warn $msg } else { throw $msg }
  }
}

# 1. binaries -> C:\gpn\bin
Invoke-Step "create $BinDir and copy the release binaries" {
  New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
  New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
  # Installing from the archive itself (C:\gpn\bin\install.ps1) means $Root and
  # $BinDir are the same directory: Copy-Item would then try to copy every file
  # onto itself and fail with "Cannot overwrite the item ... with itself".
  $srcFull = [System.IO.Path]::GetFullPath($Root).TrimEnd('\')
  $dstFull = [System.IO.Path]::GetFullPath($BinDir).TrimEnd('\')
  if ($srcFull -ieq $dstFull) {
    Say 'binaries are already in place (installing from the archive) -- nothing to copy'
  } else {
    Copy-Item (Join-Path $Root '*.exe') $BinDir -Force
  }
}
Say "binaries installed in $BinDir"

# 1a. version pointer: `data\update\current` is what the launcher runs and what
#     the updater flips to apply a release. An install that leaves it absent (or
#     dangling) makes every later update start the WRONG tree, and makes
#     `gpn_update` answer "what is installed" from a compiled-in constant.
#     Healed here, where a fresh install and a repair both pass. A version tree
#     that is present and complete wins; otherwise the launcher keeps using
#     bin\ (its documented fallback) and nothing is invented on disk.
function Repair-VersionPointer {
  $updateRoot = Join-Path $DataDir 'update'
  $versions   = Join-Path $updateRoot 'versions'
  $link       = Join-Path $updateRoot 'current'
  if (-not (Test-Path $versions)) { return }
  $trees = @(Get-ChildItem -LiteralPath $versions -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName 'gpn_node.exe') } |
    Sort-Object -Property @{ Expression = { try { [version]$_.Name } catch { [version]'0.0.0' } } })
  if ($trees.Count -eq 0) {
    Say 'no version tree yet -- the launcher runs bin\ until the first update installs one'
    return
  }
  $newest = $trees[-1].FullName
  $now = $null
  try { $now = @((Get-Item -LiteralPath $link -Force).Target) | Select-Object -First 1 } catch { }
  if ($now -and (Test-Path (Join-Path $link 'gpn_node.exe'))) { return }  # already good
  if ($WhatIf) {
    Write-Host "[whatif] point $link at $newest" -ForegroundColor Cyan
    return
  }
  if (Test-Path $link) { & cmd /c rmdir "$link" 2>&1 | Out-Null }
  & cmd /c mklink /J "$link" "$newest" 2>&1 | Out-Null
  if (Test-Path (Join-Path $link 'gpn_node.exe')) {
    Say "version pointer repaired: current -> $newest"
  } else {
    Warn "could not point $link at $newest -- the launcher will use bin\"
  }
}
Repair-VersionPointer

# 2. snapshot-first bootstrap, before the node is ever started against an empty
#    chain dir. Every failure is a logged fallback to peer sync (see
#    Invoke-FastSync); it never fails the install.
Invoke-FastSync

$gpnNode = Join-Path $BinDir 'gpn_node.exe'
$nodeExe = Join-Path $BinDir 'gpn-node.exe'

# 1b. Windows runtime: the release binaries are Rust MSVC builds and need the
# Visual C++ runtime. On a machine without it every .exe dies at load with
# 0xC0000135 (exit -1073741515) and prints NOTHING -- indistinguishable from
# "the installer did nothing".
function Get-ExeExitCode {
  param([string]$Exe, [string[]]$ExeArgs = @('key', '--help'))
  # An empty path used to reach Test-Path and throw: on box 3 (2026-11-03) this
  # happened because $nodeExe was assigned *after* the VC++ check, so the check
  # silently did nothing on every install.
  if ([string]::IsNullOrWhiteSpace($Exe)) { return 0 }
  if (-not (Test-Path $Exe)) { return 0 }
  try {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ($ExeArgs -join ' ')
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    if (-not $p.WaitForExit(15000)) { try { $p.Kill() } catch { }; return 0 }
    return [int]$p.ExitCode
  } catch { return 0 }
}

function Test-MissingVCRuntime {
  param([string]$Exe)
  $rc = Get-ExeExitCode -Exe $Exe
  # signed and unsigned spellings of STATUS_DLL_NOT_FOUND
  return ($rc -eq -1073741515 -or $rc -eq 3221225781)
}

if (Test-MissingVCRuntime $nodeExe) {
  Say 'the binaries cannot load: missing Visual C++ runtime (0xC0000135) -- installing it'
  if ($WhatIf) {
    Write-Host '[whatif] would install the Microsoft Visual C++ 2015-2022 redistributable (x64)' -ForegroundColor Cyan
  } else {
    Invoke-Step 'install VC++ redistributable' {
      $wg = Get-Command winget -ErrorAction SilentlyContinue
      if ($wg) {
        & winget install --id Microsoft.VCRedist.2015+.x64 -e --accept-source-agreements --accept-package-agreements --disable-interactivity 2>&1 | Out-Null
      }
      if (Test-MissingVCRuntime $nodeExe) {
        $vc = Join-Path $env:TEMP 'vc_redist.x64.exe'
        Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $vc -UseBasicParsing
        Start-Process -FilePath $vc -ArgumentList '/install', '/quiet', '/norestart' -Wait
      }
    }
    if (Test-MissingVCRuntime $nodeExe) {
      Write-Host ''
      Write-Host 'The Visual C++ runtime is still missing, so the node cannot start.' -ForegroundColor Red
      Write-Host 'Install it, then run this installer again:' -ForegroundColor Red
      Write-Host '    winget install --id Microsoft.VCRedist.2015+.x64 -e' -ForegroundColor Red
      Write-Host "Log: $InstallLog" -ForegroundColor Red
      try { Stop-Transcript | Out-Null } catch { }
      exit 1
    }
    Say 'Visual C++ runtime installed'
  }
}

# 2. ollama: hard prerequisite -- the node refuses to start without it.
Say 'checking ollama (REQUIRED: gpn_node exits 69 without it)'
if (Test-Ollama) {
  Say 'ollama is already serving on 127.0.0.1:11434'
} else {
  $ollama = Get-Command ollama -ErrorAction SilentlyContinue
  if (-not $ollama -and -not $NoOllamaInstall) {
    # Installing ollama is the installer's job -- a fresh machine has none, and
    # "exit 1 and tell the user to run winget" is indistinguishable from a
    # crash when the window closes. Do it here instead.
    Say 'ollama is missing -- installing it (this can take a few minutes)'
    $helper = Join-Path $Root 'scripts\install-ollama.ps1'
    if ($WhatIf) {
      Write-Host "[whatif] would install ollama (winget Ollama.Ollama, else OllamaSetup.exe /VERYSILENT)" -ForegroundColor Cyan
      Write-Host "[whatif] would start ollama serve and pull the baseline models" -ForegroundColor Cyan
    } elseif (Test-Path $helper) {
      Invoke-Step 'install ollama + baseline models' {
        & powershell -NoProfile -ExecutionPolicy Bypass -File $helper
      }
      if ($LASTEXITCODE -ne 0) { Warn "ollama helper exited $LASTEXITCODE -- falling back to a direct install" }
    } else {
      Invoke-Step 'install ollama (direct)' {
        $wg = Get-Command winget -ErrorAction SilentlyContinue
        if ($wg) {
          & winget install --id Ollama.Ollama -e --accept-source-agreements --accept-package-agreements --silent 2>&1 | Out-Null
        }
        if (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
          $setup = Join-Path $env:TEMP 'OllamaSetup.exe'
          Invoke-WebRequest -Uri 'https://ollama.com/download/OllamaSetup.exe' -OutFile $setup
          Start-Process -FilePath $setup -ArgumentList '/VERYSILENT' -Wait
        }
      }
    }
    if (-not $WhatIf) {
      # The installer updated PATH in the machine scope; this process's copy is stale.
      $env:Path = [System.Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
                   [System.Environment]::GetEnvironmentVariable('Path', 'User')
      if (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
        $candidate = Join-Path $env:LOCALAPPDATA 'Programs\Ollama\ollama.exe'
        if (Test-Path $candidate) { $env:Path = (Split-Path -Parent $candidate) + ';' + $env:Path }
      }
      $ollama = Get-Command ollama -ErrorAction SilentlyContinue
    }
  }

  if (-not $ollama) {
    Write-Host ''
    Write-Host 'OLLAMA IS MISSING -- the node will REFUSE TO START without it.' -ForegroundColor Red
    Write-Host 'Install it, then run this installer again:' -ForegroundColor Red
    Write-Host '    winget install --id Ollama.Ollama -e'
    Write-Host '  or: powershell -ExecutionPolicy Bypass -File ".\scripts\install-ollama.ps1"'
    Write-Host ''
    if ($WhatIf) {
      Warn 'continuing because -WhatIf never changes anything'
    } else {
      Write-Host "Nothing else was changed (node not started). Re-run without -NoOllamaInstall to let the installer handle it." -ForegroundColor Red
      Write-Host "Log: $InstallLog" -ForegroundColor Red
      try { Stop-Transcript | Out-Null } catch { }
      exit 1
    }
  } else {
    Say 'starting ollama serve (detached)'
    Invoke-Step 'Start-Process ollama serve' {
      Start-Process -FilePath 'ollama' -ArgumentList 'serve' -WindowStyle Hidden
    }
    if (-not $WhatIf -and -not (Wait-Ollama -Seconds 60)) {
      Write-Host ''
      Write-Host 'ollama did not answer on http://127.0.0.1:11434/api/tags within 60s.' -ForegroundColor Red
      Write-Host 'The node will refuse to start until it does. Check it with:' -ForegroundColor Red
      Write-Host '    ollama serve'
      Write-Host '    curl http://127.0.0.1:11434/api/tags'
      Write-Host ''
      Write-Host 'Nothing else was changed.' -ForegroundColor Red
      exit 1
    }
  }
  Say 'ollama is serving'
}

# 3. launcher + the exit reporter it calls on every failure, and the
#    auto-update wrapper (shipped next to them; the GPN-Update task runs it)
[void](Write-Launcher)
Write-ReportScript
Write-UpdateWrapper
Write-StartGpn
Write-HiddenLaunch

# 4. wallet (so the payout address can be printed now) + node identity
$payout = 'unknown'
if (-not $WhatIf) {
  try {
    $env:GPN_BASE_PATH = $DataDir
    $env:GPN_SIGNER_FILE = Join-Path $DataDir 'keystore.json'
    $out = & $gpnNode --ensure-wallet 2>&1 | Out-String
    $m = [regex]::Match($out, 'payout account:\s*(\S+)')
    if ($m.Success) { $payout = $m.Groups[1].Value }
  } catch { Warn "could not provision the wallet yet: $($_.Exception.Message)" }
}

$homeDir = if ($env:USERPROFILE) { $env:USERPROFILE } else { $env:HOME }
$keyFile = Join-Path $homeDir (Join-Path '.gpn' 'node-key')
if (-not (Test-Path $keyFile)) {
  Invoke-Step "generate this machine's node key ($keyFile)" {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $keyFile) | Out-Null
    & $nodeExe key generate-node-key --file $keyFile | Out-Null
  }
  if (-not $WhatIf -and -not (Test-Path $keyFile)) {
    Write-Host ''
    Write-Host "Could not create the node key at $keyFile -- refusing to start a node with a" -ForegroundColor Red
    Write-Host 'shared/default identity (two boxes with one key advertise the same peer id).' -ForegroundColor Red
    Write-Host "Log: $InstallLog" -ForegroundColor Red
    try { Stop-Transcript | Out-Null } catch { }
    exit 1
  }
}

# 5. autostart: node + ollama (ONLOGON) + the 6-hourly update check
Register-OllamaAutostart | Out-Null
Register-NodeTask
if (-not (Register-UpdateTask)) {
  # Task Scheduler refused (standard account): fall back to a Startup shortcut so
  # this machine still updates itself instead of waiting for a human.
  [void](Write-UpdateFallback)
}

if (-not $NoStart) {
  if ($script:NodeAutostart -eq 'task') {
    Invoke-Step "schtasks /Run /TN $NodeTask" {
      $rc = Invoke-Schtasks @('/Run', '/TN', $NodeTask)
      if ($rc -eq 0) {
        Say "node started ($NodeTask)"
      } else {
        # The task exists but cannot be started (typical when it belongs to
        # another account). Start it directly so the install still ends with a
        # running node -- that is the contract.
        Warn "schtasks /Run $NodeTask failed (rc=$rc) -- starting the node directly"
        Invoke-Step "start node detached: $NodeCmd" {
          Start-NodeDetached
        }
      }
    }
  } else {
    # No scheduled task (Startup shortcut only): start it now, or the operator
    # would have to log out and back in before anything runs.
    Say 'no scheduled task -- starting the node directly'
    Invoke-Step "start node detached: $NodeCmd" {
      Start-NodeDetached
    }
  }
}

# One console window with the startup summary. The node is already running in the
# background, so this is a view only: closing it stops nothing.
if (-not $NoStart -and -not $WhatIf) {
  Invoke-Step 'open the startup summary window' {
    try {
      Start-Process -FilePath (Join-Path $InstallDir 'start-gpn.cmd') -WorkingDirectory $InstallDir | Out-Null
    } catch {
      Warn "could not open the startup summary window: $($_.Exception.Message)"
    }
  }
}

if ($Verify) {
  if ($WhatIf) {
    Write-Host '[whatif] would verify: /Run -> node up -> /End -> state Ready -> /Run -> node up' -ForegroundColor Cyan
  } else {
    [void](Verify-NodeTask)
  }
}

# 6. tell the operator exactly what happened
Write-Host ''
switch ($script:NodeAutostart) {
  'task'    { Write-Host "Autostart: scheduled task $NodeTask (starts at logon)." }
  'startup' { Write-Host "Autostart: Startup shortcut (no Task Scheduler permission needed; starts at logon)." }
  default   { Write-Host "Autostart: NOT configured -- run $NodeCmd after each login." }
}
Write-Host "Dashboard: $Dashboard"
Write-Host "  payout address: $payout"
Write-Host "  log:            $NodeLog"
Write-Host "  updates:        every 1h (+/- 10 min), log: $UpdateLog"
Write-Host "  stop:           schtasks /End /TN $NodeTask"
Write-Host ''
Write-Host 'Remove the autostart in one command (data + wallet are kept):'
Write-Host '  powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall'
try { Stop-Transcript | Out-Null } catch { }
