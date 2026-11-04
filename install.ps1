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
    2. creates C:\gpn\start-node.cmd (all env in one place, one log file)
    3. makes sure ollama is installed AND serving (it is a hard prerequisite)
    4. registers the scheduled task  GPN-Node   (/SC ONLOGON /RL HIGHEST,
       MultipleInstances=IgnoreNew)  -- the node starts when you log in
    5. makes ollama autostart as well (task GPN-Ollama, or reuses the existing
       ollama autostart entry instead of adding a duplicate)
    6. starts the node now
    7. registers the scheduled task  GPN-Update  -- the Windows equivalent of
       the Linux gpn-update.timer: every 6 hours (+/- 30 min jitter) it checks
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
  '/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/23.241.109.255/tcp/30333/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz'
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
# Win32_Process.Create runs the command through the WMI service, which has no
# console to inherit. Start-Process is kept only as a fallback for machines
# where CIM/WMI is unavailable or blocked by policy; it is exactly as fragile as
# before, so the fallback says so out loud.
function Start-NodeDetached {
  param([string]$Command = '', [string]$WorkDir = '')
  if (-not $Command) { $Command = $NodeCmd }
  if (-not $WorkDir) { $WorkDir = $InstallDir }
  try {
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
      CommandLine      = "cmd /c `"$Command`""
      CurrentDirectory = $WorkDir
    } -ErrorAction Stop
    if ($r -and [int]$r.ReturnValue -eq 0) {
      Say "node started detached (Win32_Process.Create, pid $($r.ProcessId))"
      return 'cim'
    }
    Warn "Win32_Process.Create returned $($r.ReturnValue) -- falling back to Start-Process"
  } catch {
    Warn "CIM/WMI is unavailable ($($_.Exception.Message)) -- falling back to Start-Process"
  }
  # Last resort. This inherits a console, so a later window close can still take
  # the node down; the supervisor's console handler and the exit record are the
  # safety net for exactly this case.
  Start-Process -FilePath $Command -WorkingDirectory $WorkDir -WindowStyle Hidden
  Say 'node started with Start-Process (fallback: it may inherit a console)'
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
  Write-Host "Start the node by hand any time:  `"$NodeCmd`""
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
REM ============================================================================
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
REM --- local dashboard (loopback only): http://127.0.0.1:8099 ----------------
set "GPN_GUI=1"
set "GPN_OLLAMA=http://127.0.0.1:11434"
set "GPN_BIN=__INSTALL_DIR__\bin"

REM --- per-machine node identity (a shared key makes boxes collide) ----------
set "GPN_NODE_KEY="
set "GPN_NODE_KEY_FILE=__KEYFILE__"
set "KEYRC=1"
set "WRC=1"
REM What the launcher resolved -- printed so a mismatch is visible in the trace
REM instead of only as "could not create a node key".
echo [gpn] node identity: key=%GPN_NODE_KEY_FILE% userprofile=%USERPROFILE% cwd=%CD%
if not exist "%GPN_NODE_KEY_FILE%" (
  echo [gpn] generating this machine's node identity: %GPN_NODE_KEY_FILE%
  "%GPN_BIN%\gpn-node.exe" key generate-node-key --file "%GPN_NODE_KEY_FILE%" > "%TEMP%\gpn-keygen.log" 2>&1
  set "KEYRC=%ERRORLEVEL%"
  type "%TEMP%\gpn-keygen.log"
)
if not exist "%GPN_NODE_KEY_FILE%" (
  echo [gpn] ERROR: could not create a node key at %GPN_NODE_KEY_FILE% (exit %KEYRC%)
  if "%KEYRC%"=="-1073741515" echo [gpn] ERROR: 0xC0000135 = missing Visual C++ runtime -- run: winget install --id Microsoft.VCRedist.2015+.x64 -e
  echo [gpn] ERROR: refusing to start a node with a shared/default identity.
  if "%KEYRC%"=="" set "KEYRC=1"
  powershell -NoProfile -ExecutionPolicy Bypass -File "__REPORT__" -ExitCode %KEYRC% -Log "__LOG__"
  exit /b 1
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
powershell -NoProfile -ExecutionPolicy Bypass -Command "& '%GPN_BIN%\gpn_node.exe' *>&1 | Tee-Object -FilePath '__LOG__' -Append; exit $LASTEXITCODE"
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

function Write-Launcher {
  $text = $LauncherTemplate
  $text = $text.Replace('__INSTALL_DIR__', $InstallDir)
  $text = $text.Replace('__KEYFILE__', (Join-Path $env:USERPROFILE '.gpn\node-key'))
  $text = $text.Replace('__AP_URL__', $ApUrl)
  $text = $text.Replace('__BOOTNODES__', $Boots)
  $text = $text.Replace('__LOG__', $NodeLog)
  $text = $text.Replace('__REPORT__', $ReportScript)
  Invoke-Step "write $NodeCmd" {
    Set-Content -Path $NodeCmd -Value $text -Encoding ascii
  }
  Say "launcher written: $NodeCmd"
  return $text
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
if ($same.Count -ge 3) {
  $loopText = "$text`nThe SAME failure happened $($same.Count) times in 10 minutes - NOT restarting. Fix the reason above, then start GPN-Node again (schtasks /Run /TN GPN-Node)."
  if ($Log) { Add-Content -Path $Log -Value $loopText }
  try {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show($loopText, 'GPN node: crash loop', 'OK', 'Error') | Out-Null
  } catch { }
  exit 3
}

try {
  Add-Type -AssemblyName System.Windows.Forms
  [System.Windows.Forms.MessageBox]::Show($text, 'GPN node stopped', 'OK', 'Error') | Out-Null
} catch { }
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
# One GPN auto-update cycle, run by the GPN-Update scheduled task every 6 hours
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
  try {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $ReportScript -ExitCode $Code -Log $LogPath -Note $note
  } catch {
    Write-Log ('could not show the failure dialog: ' + $_.Exception.Message)
  }
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

  $updater = Join-Path $BinDir 'gpn_update.exe'
  if (-not (Test-Path $updater)) {
    Show-UpdateFailure 1 'update check' ('gpn_update.exe is missing from ' + $BinDir)
    exit 1
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
  & schtasks /End /TN $NodeTask 2>&1 | Out-Null
  $endRc = $LASTEXITCODE
  if ($endRc -ne 0) { Write-Log ('schtasks /End exited ' + $endRc + ' - the node may not have been running') }

  if (Wait-NodeExit 60) {
    Write-Log 'GPN-Node stopped'
  } else {
    Write-Log 'the node did not exit within 60 s - stopping the remaining gpn* processes'
    Get-Process -ErrorAction SilentlyContinue | Where-Object {
      $_.ProcessName -like 'gpn*' -and $_.ProcessName -ne 'gpn_update' -and $_.Id -ne $PID
    } | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
    if (Wait-NodeExit 10) { Write-Log 'GPN-Node stopped (forced)' }
    else { Write-Log 'WARNING: gpn processes are still running; the apply step may fail' }
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

  if ($apply -eq 0) {
    Write-Log 'update cycle complete'
    exit 0
  }
  Show-UpdateFailure $apply 'gpn_update --apply' 'the new release was not installed; the node is back on the previous build'
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
# 6-hour repetition. Writing the XML directly works on stock Windows 10/11,
# needs no module, and (with /F) replaces the task wholesale, so re-running the
# installer cannot leave a second task or a duplicate trigger behind.
$UpdateTaskXml = @'
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>GPN auto-update: checks the signed release manifest every 6 hours and, when a newer release exists, stops GPN-Node, applies the update and starts the node again.</Description>
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
        <Interval>PT6H</Interval>
        <Duration>P1D</Duration>
        <StopAtDurationEnd>false</StopAtDurationEnd>
        <RandomDelay>PT30M</RandomDelay>
      </Repetition>
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
    Write-Host "[whatif] register scheduled task $UpdateTask (every 6h, random delay 30m) -> $UpdateScript" -ForegroundColor Cyan
    return $true
  }

  $xmlPath = Join-Path $env:TEMP 'gpn-update-task.xml'
  Set-Content -Path $xmlPath -Value $xmlText -Encoding Unicode
  $rc = Invoke-Schtasks @('/Create', '/TN', $UpdateTask, '/XML', $xmlPath, '/F')
  Remove-Item -Force $xmlPath -ErrorAction SilentlyContinue
  if ($rc -ne 0) {
    Warn "could not register $UpdateTask (rc=$rc) -- this box will NOT auto-update; re-run install.ps1 as the logged-in user (not elevated, and not as another account)"
    return $false
  }
  Say "scheduled task $UpdateTask registered (every 6h +/- 30 min, MultipleInstances=IgnoreNew)"
  return $true
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
function Register-NodeTask {
  # Default: create the task at NORMAL privilege. The node binds only high ports
  # (30333/9944/8093/8094/8099), so it needs no admin rights -- and asking for
  # them (/RL HIGHEST) makes the create fail with rc=1 "Access is denied" from a
  # non-elevated shell, which is what broke the stock install on box 3. Elevated
  # installs used to also produce a task owned by Administrator that never fires
  # for the user who actually logs in. Opt in with -RunHighest if you want it.
  $createArgs = @('/Create', '/TN', $NodeTask, '/TR', "cmd /c $NodeCmd", '/SC', 'ONLOGON', '/F')
  if ($RunHighest) { $createArgs += @('/RL', 'HIGHEST') }
  Invoke-Step "schtasks /Create /TN $NodeTask (/SC ONLOGON$(if ($RunHighest) { ' /RL HIGHEST' }))" {
    # Reuse an existing task that already runs our launcher. A standard (non-admin)
    # Windows account gets "Access is denied" from /Create even when the task is
    # its own (measured 2026-11-04: `schtasks /Create /TN gpntest ... /F` fails on a
    # fresh box), so recreating what already works is both unnecessary and fatal.
    $existing = (& cmd /c "schtasks /Query /TN $NodeTask /FO LIST /V 2>nul") -join "`n"
    if ($existing -match 'Task To Run:\s*(.+?)\r?\n' -and $Matches[1] -match 'start-node\.cmd') {
      $ownerX = ''
      if ($existing -match 'Run As User:\s*(\S+)') { $ownerX = $Matches[1] }
      if (-not $ownerX -or ($ownerX -match [regex]::Escape($env:USERNAME))) {
        Say "$NodeTask already exists and runs $NodeCmd -- reusing it (not recreating)"
        $script:NodeAutostart = 'task'
        return
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
      $plain = @('/Create', '/TN', $NodeTask, '/TR', "cmd /c $NodeCmd", '/SC', 'ONLOGON', '/F')
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
          $sc.TargetPath = $NodeCmd
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
        $sc.TargetPath = $NodeCmd
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
if ($Uninstall) { Invoke-Uninstall; exit 0 }

# Deploy-step mode: only the auto-update task. The release workflow runs this on
# every deploy so a box installed before GPN-Update existed heals itself; it
# touches no binaries, no ollama and not the running node.
if ($UpdateTaskOnly) {
  Say "registering only the auto-update task in $InstallDir"
  Write-UpdateWrapper
  $ok = Register-UpdateTask
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
foreach ($needed in @('gpn_node.exe', 'gpn-node.exe')) {
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
[void](Register-UpdateTask)

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
Write-Host "  updates:        every 6h (+/- 30 min), log: $UpdateLog"
Write-Host "  stop:           schtasks /End /TN $NodeTask"
Write-Host ''
Write-Host 'Remove the autostart in one command (data + wallet are kept):'
Write-Host '  powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall'
try { Stop-Transcript | Out-Null } catch { }
