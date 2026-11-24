<#
.SYNOPSIS
  The stranger lifecycle: what a person with no token and no repo access gets
  when they follow https://gpncoin.ai/download on a clean Windows machine.

  1. download from the site snippet, size and sha256 equal releases.json
  2. gpn-node --version equals the manifest version
  3. the provider registers unattended within 10 min (named reason on failure)
  4. one AI job is served end to end through the access point
  5. the node self-updates to the next published version, without a human
  6. it is still registered and still serving afterwards

  Every step prints PASS/FAIL and, on failure, the specific cause taken from
  the node's own log or last-exit.json. Exit code is the number of the first
  failed step (0 = all passed).

.DESCRIPTION
  Row 256. The defects that cost the fifth machine two days (silent install
  fallback, rollback on a loader refusal, a rejection message naming the wrong
  thing, a permanent per-GPU entry mark with no re-entry path) were invisible
  to the unit suite and obvious to this sequence. This script IS the release
  gate for them.

  It uses ONLY the public surface: https://gpncoin.ai/releases.json, the
  snippet printed on https://gpncoin.ai/download, and the public access point.
  No token, no repo checkout, no pre-staged zip. If a step needs a human, a
  credential or a retry, the release is not ready.

.PARAMETER ApiBase
  Site base that publishes releases.json (default https://gpncoin.ai).

.PARAMETER ApUrl
  Access point the node serves through (default the public testnet AP).

.PARAMETER InstallDir
  Where the node installs (default C:\gpn).

.PARAMETER Step
  Run one step only: 1..6, or 0 for all (default). Later phases need an
  installed node: use -Step 3..6 to continue after an earlier partial run.

.PARAMETER UpdateWaitMinutes
  How long step 5 waits for the updater to observe and apply a newer release
  (default 75: the Windows task runs hourly and an apply takes minutes).

.PARAMETER JobModel
  Model to request in step 4. Empty = ask the AP for the first model the node
  itself advertises.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File stranger-lifecycle.ps1
  Runs the whole sequence on a clean machine. Exit 0 means publishable.

.NOTES
  ASCII only on purpose: Windows PowerShell 5.1 decodes scripts with the ANSI
  codepage, and one UTF-8 em-dash becomes three latin1 characters, which is
  how workflow 44728980382 lost its release.
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
  [string] $ApiBase            = 'https://gpncoin.ai',
  [string] $ApUrl              = 'http://gpn-testnet.duckdns.org:8092',
  [string] $InstallDir         = 'C:\gpn',
  [int]    $Step               = 0,
  [int]    $UpdateWaitMinutes  = 75,
  [string] $JobModel           = '',
  [string] $Report             = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'

$script:Results = New-Object System.Collections.ArrayList
$script:StartTime = Get-Date

function Say([string] $msg) {
  $line = '[{0:HH:mm:ss}] {1}' -f (Get-Date), $msg
  Write-Host $line
  if ($Report) { Add-Content -LiteralPath $Report -Value $line -Encoding UTF8 }
}

function Step-Result([int] $n, [string] $name, [bool] $ok, [string] $detail) {
  $state = if ($ok) { 'PASS' } else { 'FAIL' }
  Say ("STEP {0} {1}: {2} -- {3}" -f $n, $name, $state, $detail)
  [void]$script:Results.Add([pscustomobject]@{ Step = $n; Name = $name; Ok = $ok; Detail = $detail })
}

function Fail([int] $n, [string] $name, [string] $cause) {
  Step-Result $n $name $false $cause
  Say ("RESULT: FAILED at step {0} ({1}). Cause: {2}" -f $n, $name, $cause)
  Save-Report
  exit $n
}

function Save-Report {
  if (-not $Report) { return }
  $lines = @()
  $lines += ''
  $lines += '== summary =='
  foreach ($r in $script:Results) {
    $lines += ('{0}. {1}: {2} -- {3}' -f $r.Step, $r.Name, $(if ($r.Ok) { 'PASS' } else { 'FAIL' }), $r.Detail)
  }
  $lines += ('elapsed: {0:N0}s' -f ((Get-Date) - $script:StartTime).TotalSeconds)
  Add-Content -LiteralPath $Report -Value ($lines -join "`r`n") -Encoding UTF8
}

# Native commands under EAP=Stop are a trap: curl's progress meter goes to
# stderr, PowerShell turns the first stderr line into a terminating error, and
# the script dies with exit 1 and no report line (this happened twice on
# GPN-209 on 2026-11-24 before the cause was found). Run natives with
# ErrorActionPreference=Continue and read $LASTEXITCODE.
function Invoke-Native {
  param([string] $exe, [string[]] $argv)
  $old = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    $lines = & $exe @argv 2>&1
    $rc = $LASTEXITCODE
    foreach ($l in @($lines)) { if ($l) { Say ('  ' + ($l | Out-String).Trim()) } }
    return $rc
  } finally { $ErrorActionPreference = $old }
}

function Get-NodeLog {
  foreach ($p in @((Join-Path $InstallDir 'node.log'), (Join-Path $InstallDir 'data\node.log'))) {
    if (Test-Path -LiteralPath $p) { return $p }
  }
  return $null
}

function Tail-Log([string] $path, [int] $count) {
  if (-not $path -or -not (Test-Path -LiteralPath $path)) { return @() }
  return @(Get-Content -LiteralPath $path -Tail $count -ErrorAction SilentlyContinue)
}

function Find-NodeExe {
  foreach ($p in @((Join-Path $InstallDir 'bin\gpn-node.exe'),
                   (Join-Path $InstallDir 'bin\gpn_node.exe'))) {
    if (Test-Path -LiteralPath $p) { return $p }
  }
  # newest install tree the node may run from
  $d = Join-Path $InstallDir 'data\update\current'
  if (Test-Path -LiteralPath $d) {
    $hit = Get-ChildItem -LiteralPath $d -Filter 'gpn?node.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($hit) { return $hit.FullName }
  }
  return $null
}

function Get-RunningNodeVersion {
  # Ask the RUNNING process, not the directory: C:\gpn\bin can be stale.
  $p = Get-Process -Name 'gpn-node','gpn_node' -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $p -or -not $p.Path) { return $null }
  return (Get-ExeVersion $p.Path)
}

function Get-ExeVersion([string] $exe) {
  if (-not $exe -or -not (Test-Path -LiteralPath $exe)) { return $null }
  $out = & $exe --version 2>&1 | Out-String
  $m = [regex]::Match($out, '(\d+\.\d+\.\d+)')
  if ($m.Success) { return $m.Groups[1].Value }
  return $null
}

function Get-Manifest {
  $u = ($ApiBase.TrimEnd('/')) + '/releases.json'
  return Invoke-RestMethod -Uri $u -UseBasicParsing -TimeoutSec 30
}

function Get-WindowsAsset($manifest) {
  $a = @($manifest.assets | Where-Object { $_.os -eq 'windows' })
  if ($a.Count -eq 0) { throw 'releases.json has no windows asset' }
  return $a[0]
}

function Get-ApProviders {
  $u = ($ApUrl.TrimEnd('/')) + '/providers'
  $r = Invoke-RestMethod -Uri $u -UseBasicParsing -TimeoutSec 20
  if ($r.providers) { return @($r.providers) }
  return @()
}

function Get-PayoutAddress {
  # The keystore is the node's own money identity; ask the shipped wallet for
  # its ss58 rather than regexing an address out of a log (node.log carries
  # peer addresses too, and picking one of those silently breaks step 3).
  $ks = Join-Path $InstallDir 'data\keystore.json'
  $wallet = Join-Path $InstallDir 'bin\gpn_wallet.exe'
  if ((Test-Path -LiteralPath $ks) -and (Test-Path -LiteralPath $wallet)) {
    try {
      $old = $ErrorActionPreference
      $ErrorActionPreference = 'Continue'
      $out = & $wallet address $ks 2>&1
      $ErrorActionPreference = $old
      foreach ($l in @($out)) {
        $m = [regex]::Match([string]$l, '5[1-9A-HJ-NP-Za-km-z]{47}')
        if ($m.Success) { return $m.Groups[0].Value }
      }
    } catch { }
  }
  # fallback: the installer's own summary
  $sum = Join-Path $InstallDir 'startup-summary.txt'
  if (Test-Path -LiteralPath $sum) {
    $m = [regex]::Match((Get-Content -LiteralPath $sum -Raw), 'wallet account *: *(5[1-9A-HJ-NP-Za-km-z]{47})')
    if ($m.Success) { return $m.Groups[1].Value }
  }
  $log = Get-NodeLog
  foreach ($line in @(Tail-Log $log 400)) {
    $m = [regex]::Match($line, 'payout account: (5[1-9A-HJ-NP-Za-km-z]{47})')
    if ($m.Success) { return $m.Groups[1].Value }
  }
  return $null
}

# ------------------------------------------------------------------ step 1 --
function Invoke-Step1 {
  Say 'step 1: fetch the manifest and install exactly what the site snippet says'
  $m = Get-Manifest
  $asset = Get-WindowsAsset $m
  $ver = $m.version
  Say ("manifest version={0} windows size={1} sha256={2}" -f $ver, $asset.size, $asset.sha256)

  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  $zip = Join-Path $InstallDir 'gpn-windows.zip'
  if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
  $rc = Invoke-Native 'curl.exe' @('-fsSL','--retry','2','-o',$zip,$asset.url)
  if ($rc -ne 0 -or -not (Test-Path -LiteralPath $zip)) {
    Fail 1 'download' ("curl exit {0} fetching {1} -- the asset a stranger is told to fetch is not downloadable" -f $rc, $asset.url)
  }
  $f = Get-Item -LiteralPath $zip
  if ($f.Length -ne [int64]$asset.size) {
    Fail 1 'size' ("downloaded {0} bytes, manifest says {1}" -f $f.Length, $asset.size)
  }
  $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
  if ($hash -ne $asset.sha256.ToLower()) {
    Fail 1 'sha256' ("downloaded {0}, manifest says {1}" -f $hash, $asset.sha256)
  }
  Step-Result 1 'download integrity' $true ("{0} bytes, sha256 {1} == manifest" -f $f.Length, $hash.Substring(0,16))

  $stage = Join-Path $InstallDir 'bin.pending'
  if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
  Expand-Archive -LiteralPath $zip -DestinationPath $stage -Force
  $installer = Join-Path $stage 'install.ps1'
  if (-not (Test-Path -LiteralPath $installer)) {
    Fail 1 'installer' ("the zip has no install.ps1 (contents: {0})" -f ((Get-ChildItem -LiteralPath $stage -Name) -join ', '))
  }
  Say 'step 1b: run the shipped installer with no arguments (as the snippet does)'
  $log = Join-Path $InstallDir 'install-log.txt'
  $old = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  & powershell -NoProfile -ExecutionPolicy Bypass -File $installer *>&1 |
    ForEach-Object { $_ | Out-String | ForEach-Object { Add-Content -LiteralPath $log -Value $_ -ErrorAction SilentlyContinue } }
  $rc = $LASTEXITCODE
  $ErrorActionPreference = $old
  if ($rc -ne 0) {
    $tail = (Tail-Log $log 12) -join ' | '
    Fail 1 'install' ("install.ps1 exit {0}; last lines: {1}" -f $rc, $tail)
  }
  Step-Result 1 'installer exit' $true 'install.ps1 exit 0'
  return $ver
}

# ------------------------------------------------------------------ step 2 --
function Invoke-Step2([string] $expected) {
  if (-not $expected) { $expected = (Get-Manifest).version }
  $exe = Find-NodeExe
  if (-not $exe) { Fail 2 'version' 'no gpn-node.exe / gpn_node.exe under the install dir' }
  $got = Get-ExeVersion $exe
  if (-not $got) { Fail 2 'version' ("{0} --version printed no version string" -f $exe) }
  if ($got -ne $expected) {
    Fail 2 'version' ("installed binary reports {0}, manifest says {1} ({2})" -f $got, $expected, $exe)
  }
  Step-Result 2 'version' $true ("{0} reports {1}" -f $exe, $got)
}

# ------------------------------------------------------------------ step 3 --
function Invoke-Step3([double] $minutes = 10) {
  $deadline = (Get-Date).AddMinutes($minutes)
  $addr = Get-PayoutAddress
  Say ("step 3: wait up to {0} min for unattended registration (payout {1})" -f $minutes, $addr)
  $lastWhy = 'the access point never listed this provider'
  while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 20
    try {
      $provs = Get-ApProviders
      if (-not $addr) { $addr = Get-PayoutAddress }
      if ($addr) {
        $hit = $provs | Where-Object { $_.address -eq $addr } | Select-Object -First 1
        if ($hit) {
          Step-Result 3 'registration' $true ("AP lists {0} (healthy={1}, models={2})" -f $addr, $hit.healthy, (@($hit.models) -join ','))
          return $hit
        }
      }
      $lastWhy = 'AP /providers does not list this node yet'
    } catch {
      $lastWhy = 'AP /providers unreachable: ' + $_.Exception.Message
    }
  }
  # named reason from the node's own records
  $cause = $lastWhy
  $log = Get-NodeLog
  $onb = @(Tail-Log $log 2000 | Where-Object { $_ -match '\[onboard\]' })
  if ($onb.Count -gt 0) { $cause = 'node.log: ' + (($onb | Select-Object -Last 3) -join ' | ') }
  $le = Join-Path $InstallDir 'data\last-exit.json'
  if (Test-Path -LiteralPath $le) {
    $j = Get-Content -LiteralPath $le -Raw | ConvertFrom-Json
    if ($j.kind -eq 'onboard') { $cause = 'last-exit.json onboard reason: ' + $j.reason }
  }
  Fail 3 'registration' ("not registered within {0} min; {1}" -f $minutes, $cause)
}

# ------------------------------------------------------------------ step 4 --
function Invoke-Step4 {
  $addr = Get-PayoutAddress
  $provs = Get-ApProviders
  $me = $null
  if ($addr) { $me = $provs | Where-Object { $_.address -eq $addr } | Select-Object -First 1 }
  if (-not $me) { Fail 4 'job' 'the node is not listed on the AP, so it cannot be routed a job' }
  $model = $JobModel
  if (-not $model) { $model = @($me.models)[0] }
  if (-not $model) { Fail 4 'job' 'the node advertises no models, so no job can be routed to it' }
  Say ("step 4: submit one chat job for model {0} through {1}" -f $model, $ApUrl)
  $body = @{ model = $model; messages = @(@{ role = 'user'; content = 'Reply with the single word: pong' }); max_tokens = 16 } | ConvertTo-Json -Depth 6
  $u = ($ApUrl.TrimEnd('/')) + '/v1/chat/completions'
  $log = Get-NodeLog
  $before = @(Tail-Log $log 4000)
  try {
    $r = Invoke-RestMethod -Uri $u -Method Post -ContentType 'application/json' -Body $body -TimeoutSec 300
  } catch {
    Fail 4 'job' ('POST {0} failed: {1}' -f $u, $_.Exception.Message)
  }
  $txt = $null
  try { $txt = $r.choices[0].message.content } catch { }
  if (-not $txt) { Fail 4 'job' ('the AP answered but carried no completion text: ' + ($r | ConvertTo-Json -Depth 4 -Compress)) }
  $after = @(Tail-Log $log 4000)
  $served = @($after | Where-Object { $_ -match '\[assign\]' })
  Step-Result 4 'job' $true ("model={0} answer='{1}' ({2} [assign] lines in node.log)" -f $model, ($txt -replace '\s+',' ').Trim(), $served.Count)
}

# ------------------------------------------------------------------ step 5 --
function Invoke-Step5([double] $minutes = 75) {
  $cur = Get-RunningNodeVersion
  if (-not $cur) { Fail 5 'self-update' 'no running gpn-node process, so there is nothing to update' }
  Say ("step 5: running process reports {0}; wait up to {1} min for a newer published version" -f $cur, $minutes)
  $ulp = Join-Path $InstallDir 'update.log'
  $deadline = (Get-Date).AddMinutes($minutes)
  while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 60
    try { $m = Get-Manifest } catch { continue }
    $latest = $m.version
    $running = Get-RunningNodeVersion
    if ($running -and $running -ne $cur) {
      Step-Result 5 'self-update' $true ("{0} -> {1} with no human; latest published {2}" -f $cur, $running, $latest)
      return $running
    }
    Say ("  now {0}, published {1}, updater log: {2}" -f $running, $latest, ((Tail-Log $ulp 1) -join ''))
  }
  $cause = 'no update applied in ' + $minutes + ' min'
  $t = @(Tail-Log $ulp 6)
  if ($t.Count -gt 0) { $cause = 'update.log: ' + ($t -join ' | ') }
  if ($cur -eq (Get-Manifest).version) {
    $cause = "nothing newer is published than {0}; step 5 needs two published versions" -f $cur
  }
  Fail 5 'self-update' $cause
}

# ------------------------------------------------------------------ step 6 --
function Invoke-Step6 {
  Say 'step 6: after the update, is it still registered and still serving'
  $prov = Invoke-Step3 3
  $null = Invoke-Step4
  Step-Result 6 'still serving' $true 'registered and served one job after the update'
}

# --------------------------------------------------------------------- run --
if ($Report) { New-Item -ItemType File -Force -Path $Report | Out-Null }
Say ("stranger lifecycle on {0} ({1}); powershell {2}" -f $env:COMPUTERNAME, (Get-CimInstance Win32_OperatingSystem).Caption, $PSVersionTable.PSVersion)

$all = ($Step -eq 0)
$expected = $null
if ($all -or $Step -eq 1) { $expected = Invoke-Step1 }
if ($all -or $Step -eq 2) { Invoke-Step2 $expected }
if ($all -or $Step -eq 3) { $null = Invoke-Step3 10 }
if ($all -or $Step -eq 4) { Invoke-Step4 }
if ($all -or $Step -eq 5) { $null = Invoke-Step5 $UpdateWaitMinutes }
if ($all -or $Step -eq 6) { Invoke-Step6 }

Save-Report
$failed = @($script:Results | Where-Object { -not $_.Ok })
if ($failed.Count -gt 0) { exit $failed[0].Step }
if ($all) {
  Say 'RESULT: PASS -- the stranger lifecycle completed with no human and no credential'
} else {
  Say ("RESULT: PASS -- step {0} only (run without -Step for the whole lifecycle)" -f $Step)
}
exit 0
