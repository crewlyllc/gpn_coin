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

  Running it twice is safe: tasks are recreated by name (/F), never duplicated.

.PARAMETER Uninstall
  Remove the autostart: stop + delete GPN-Node, delete GPN-Ollama only if this
  script created it. Chain data and the wallet are NEVER touched.

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

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File install.ps1

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
$InstallDir = 'C:\gpn'

for ($i = 0; $i -lt $args.Count; $i++) {
  $a = [string]$args[$i]
  switch -Regex ($a) {
    '^-{1,2}(uninstall|remove-autostart)$' { $Uninstall = $true }
    '^-{1,2}what ?if$'                     { $WhatIf    = $true }
    '^-{1,2}no-?start$'                    { $NoStart   = $true }
    '^-{1,2}no-?ollama-?install$'          { $NoOllamaInstall = $true }
    '^-{1,2}install-?dir[=:](.+)$'         { $InstallDir = $Matches[1] }
    '^-{1,2}install-?dir$' {
      if ($i + 1 -lt $args.Count) { $i++; $InstallDir = [string]$args[$i] }
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
$NodeTask  = 'GPN-Node'
$OllamaTask = 'GPN-Ollama'
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
function Invoke-Schtasks {
  param([string[]]$Args1)
  & schtasks @Args1 2>&1 | Out-Null
  return $LASTEXITCODE
}

# One place for "mutate the machine": honours -WhatIf everywhere.
function Invoke-Step {
  param([string]$Description, [scriptblock]$Action)
  if ($WhatIf) { Write-Host "[whatif] $Description" -ForegroundColor Cyan; return }
  & $Action
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

  foreach ($tn in @($NodeTask, $OllamaTask)) {
    $rc = Invoke-Schtasks @('/Query', '/TN', $tn)
    if ($rc -ne 0) { Say "task $tn not present"; continue }

    Invoke-Step "schtasks /End /TN $tn" { [void](Invoke-Schtasks @('/End', '/TN', $tn)) }
    Invoke-Step "schtasks /Delete /TN $tn /F" {
      $rc = Invoke-Schtasks @('/Delete', '/TN', $tn, '/F')
      if ($rc -ne 0) { Warn "could not delete task $tn (rc=$rc)" }
    }
    Say "removed task $tn"
  }

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
set "GPN_NODE_KEY_FILE=%USERPROFILE%\.gpn\node-key"
if not exist "%USERPROFILE%\.gpn" mkdir "%USERPROFILE%\.gpn" >nul 2>&1
if not exist "%GPN_NODE_KEY_FILE%" (
  echo [gpn] generating this machine's node identity: %GPN_NODE_KEY_FILE%
  "%GPN_BIN%\gpn-node.exe" key generate-node-key --file "%GPN_NODE_KEY_FILE%"
  set "KEYRC=%ERRORLEVEL%"
)
if not exist "%GPN_NODE_KEY_FILE%" (
  echo [gpn] ERROR: could not create a node key at %GPN_NODE_KEY_FILE% (exit %KEYRC%)
  if "%KEYRC%"=="-1073741515" echo [gpn] ERROR: 0xC0000135 = missing Visual C++ runtime -- run: winget install --id Microsoft.VCRedist.2015+.x64 -e
  echo [gpn] ERROR: refusing to start a node with a shared/default identity.
  exit /b 1
)

REM --- ollama is REQUIRED: gpn_node exits 69 without it ----------------------
powershell -NoProfile -Command "try { Invoke-RestMethod 'http://127.0.0.1:11434/api/tags' -TimeoutSec 3 | Out-Null } catch { Start-Process ollama -ArgumentList 'serve' -WindowStyle Hidden }"

REM --- money identity: create once, then NEVER replace -----------------------
"%GPN_BIN%\gpn_node.exe" --ensure-wallet
if errorlevel 1 (
  echo [gpn] ERROR: could not set up this machine's wallet - the node will NOT start.
  echo [gpn] ERROR: fix the keystore at %GPN_SIGNER_FILE%, or delete it on purpose to mine for a new account.
  exit /b 1
)

REM --- run the supervisor, tee everything to one log -------------------------
powershell -NoProfile -ExecutionPolicy Bypass -Command "& '%GPN_BIN%\gpn_node.exe' *>&1 | Tee-Object -FilePath '__LOG__' -Append"
'@

function Write-Launcher {
  $text = $LauncherTemplate
  $text = $text.Replace('__INSTALL_DIR__', $InstallDir)
  $text = $text.Replace('__AP_URL__', $ApUrl)
  $text = $text.Replace('__BOOTNODES__', $Boots)
  $text = $text.Replace('__LOG__', $NodeLog)
  Invoke-Step "write $NodeCmd" {
    Set-Content -Path $NodeCmd -Value $text -Encoding ascii
  }
  Say "launcher written: $NodeCmd"
  return $text
}

# ------------------------------------------------------------------ tasks ----
function Register-NodeTask {
  Invoke-Step "schtasks /Create /TN $NodeTask /TR `"cmd /c $NodeCmd`" /SC ONLOGON /RL HIGHEST /F" {
    $rc = Invoke-Schtasks @('/Create', '/TN', $NodeTask, '/TR', "cmd /c $NodeCmd", '/SC', 'ONLOGON', '/RL', 'HIGHEST', '/F')
    if ($rc -ne 0) { throw "schtasks /Create $NodeTask failed (rc=$rc)" }
  }
  if ($WhatIf) { return }

  # ONE supervisor only: /Run on an already-running task would fork a second
  # node on the same base path and the same ports (2026-10-23, ~2h lost).
  $miSet = $false
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
  Say "scheduled task $NodeTask registered (/SC ONLOGON /RL HIGHEST)"
}

# ----------------------------------------------------------------- install ---
if ($Uninstall) { Invoke-Uninstall; exit 0 }

Say "GPN installer -- installing from $Root into $InstallDir"
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

# 1b. Windows runtime: the release binaries are Rust MSVC builds and need the
# Visual C++ runtime. On a machine without it every .exe dies at load with
# 0xC0000135 (exit -1073741515) and prints NOTHING -- indistinguishable from
# "the installer did nothing".
function Get-ExeExitCode {
  param([string]$Exe, [string[]]$ExeArgs = @('key', '--help'))
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

$gpnNode = Join-Path $BinDir 'gpn_node.exe'
$nodeExe = Join-Path $BinDir 'gpn-node.exe'

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

# 3. launcher
[void](Write-Launcher)

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

# 5. autostart: node + ollama, both ONLOGON
Register-OllamaAutostart | Out-Null
Register-NodeTask

if (-not $NoStart) {
  Invoke-Step "schtasks /Run /TN $NodeTask" {
    $rc = Invoke-Schtasks @('/Run', '/TN', $NodeTask)
    if ($rc -ne 0) { Warn "schtasks /Run $NodeTask failed (rc=$rc)" }
  }
}

# 6. tell the operator exactly what happened
Write-Host ''
Write-Host "Your node will start automatically when you log in. Dashboard: $Dashboard"
Write-Host "  payout address: $payout"
Write-Host "  log:            $NodeLog"
Write-Host "  stop:           schtasks /End /TN $NodeTask"
Write-Host ''
Write-Host 'Remove the autostart in one command (data + wallet are kept):'
Write-Host '  powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall'
try { Stop-Transcript | Out-Null } catch { }
