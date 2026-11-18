# Behavioural tests for the generated Windows updater's confirm gate (#207).
#
# The gate is the part that decides whether an update is done, and it used to be
# untestable without a real Windows box: every branch needs a restart, a dead
# child, or fifteen minutes of waiting. This script extracts the function bodies
# from `install.ps1`'s `$UpdateWrapperTemplate` (up to the self-test marker, so
# nothing executes), dots them in, and replaces the IO (process liveness, RPC,
# task control) with scripted answers. It runs on Linux or Windows under pwsh 7.
#
# Covered:
#   A  slow start: process up, RPC answers after a few polls      -> confirmed
#   C  process exits at ~6 s                                      -> fails at once
#   B  process up, RPC never answers                              -> ceiling, rpc
#   -  unknown liveness is never reported as death
#   -  classifier: lock / pruning / keystore / unknown / bind error
#   -  one remediation+retry succeeds, a second failure stops at one
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
$installer = Join-Path $root 'install.ps1'
$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('gpn-confirm-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmpRoot | Out-Null
$env:USERPROFILE = Join-Path $tmpRoot 'home'
New-Item -ItemType Directory -Force -Path $env:USERPROFILE | Out-Null

$text = Get-Content -LiteralPath $installer -Raw
$marker = '$UpdateWrapperTemplate' + " = @'"
$start = $text.IndexOf($marker)
if ($start -lt 0) { throw "could not find the update wrapper template in $installer" }
$start = $text.IndexOf("`n", $start) + 1
$end = $text.IndexOf("`n'@", $start)
if ($end -lt 0) { throw "could not find the end of the update wrapper template" }
$body = $text.Substring($start, $end - $start)
$body = $body.Replace('__INSTALL_DIR__', (Join-Path $tmpRoot 'gpn').Replace('\', '/'))
$selfTest = $body.IndexOf('# Optional ops self-test')
if ($selfTest -lt 0) { throw 'the self-test marker moved; this test extracts functions only' }
. ([scriptblock]::Create($body.Substring(0, $selfTest)))

$LogPath = Join-Path $tmpRoot 'update.log'
$script:fail = 0
function Check($cond, $what) {
  if ($cond) { Write-Host "ok   $what" } else { Write-Host "FAIL $what"; $script:fail++ }
}
function Reset-Log { if (Test-Path $LogPath) { Remove-Item $LogPath -Force } }

# -- A. slow start: RPC answers after a few polls --------------------------
Reset-Log
$script:rpcCalls = 0
function global:Test-NodeAlive { param([string]$Version) return $true }
function global:Invoke-RestMethod {
  $script:rpcCalls++
  if ($script:rpcCalls -lt 3) { throw 'connection refused' }
  return [pscustomobject]@{ result = '0.3.59' }
}
$sw = [Diagnostics.Stopwatch]::StartNew()
$r = Confirm-Running -Expect '0.3.59' -WindowSecs 120 -CeilingSecs 900
$sw.Stop()
Check ($r -and $r.Ok -and $r.Version -eq '0.3.59') 'A: slow start confirms when RPC arrives'
Check ($sw.Elapsed.TotalSeconds -lt 30) 'A: it does not wait out the window'
$logs = Get-Content $LogPath -Raw
Check ($logs -match 'process=up rpc=no') 'A: logs process=up rpc=no separately'
Check ($logs -match 'rpc=yes .*version=0\.3\.59') 'A: logs rpc=yes with the observed version'

# -- C. process dies -> immediate failure ----------------------------------
Reset-Log
$script:calls = 0
function global:Test-NodeAlive {
  param([string]$Version)
  $script:calls++
  if ($script:calls -le 3) { return $true }
  return $false
}
function global:Invoke-RestMethod { throw 'connection refused' }
$sw = [Diagnostics.Stopwatch]::StartNew()
$r = Confirm-Running -Expect '0.3.59' -WindowSecs 900 -CeilingSecs 900
$sw.Stop()
Check ($null -eq $r) 'C: a process that exits fails the confirm'
Check ($script:ConfirmSignal -eq 'process') 'C: the failing signal is "process"'
Check ($sw.Elapsed.TotalSeconds -lt 30) 'C: it fails immediately, not at the 900 s ceiling'
$logs = Get-Content $LogPath -Raw
Check ($logs -match 'process=down rpc=no .*exited before it confirmed') 'C: the log says the process exited'

# -- B. process up, RPC never answers -> ceiling then "process up, RPC down" ---
Reset-Log
function global:Test-NodeAlive { param([string]$Version) return $true }
$r = Confirm-Running -Expect '0.3.59' -WindowSecs 2 -CeilingSecs 8
Check ($null -eq $r) 'B: RPC that never answers fails'
Check ($script:ConfirmSignal -eq 'rpc') 'B: the failing signal is "rpc"'
Check ($script:ConfirmReason -match 'process up, RPC down') ('B: it reports both signals: ' + $script:ConfirmReason)
$logs = Get-Content $LogPath -Raw
Check ($logs -match 'past the 2s window') 'B: it says when it passes the window'

# -- unknown liveness is never "dead" --------------------------------------
function global:Test-NodeAlive { param([string]$Version) return $null }
$r = Confirm-Running -Expect '0.3.59' -WindowSecs 2 -CeilingSecs 4
Check ($null -eq $r -and $script:ConfirmSignal -eq 'rpc') 'unknown liveness -> rpc failure, not a death claim'

# -- classifier ------------------------------------------------------------
function E([string]$exit, [string[]]$tail) { return @{ LastExit = $exit; Tail = $tail } }
$m = Get-Remediation -Evidence (E '' @('Error: The process cannot access the file because it is being used by another process. (os error 32)')) -Port 9944
Check ($m.Kind -eq 'lock' -and $m.Actionable) 'lock -> actionable orphan-family stop'
$m = Get-Remediation -Evidence (E '' @('Error: Incompatible pruning modes [stored: Constrained(256); requested: Archive]')) -Port 9944
Check ($m.Kind -eq 'pruning' -and $m.Actionable -and $m.Stored -match '256') ('pruning -> actionable, stored=' + $m.Stored)
$m = Get-Remediation -Evidence (E '' @('Error: node-key not found at data/network/secret_ed25519')) -Port 9944
Check ($m.Kind -eq 'config' -and -not $m.Actionable) 'missing node-key -> no automatic repair'
$m = Get-Remediation -Evidence (E '' @("thread 'main' panicked at 'index out of bounds'")) -Port 9944
Check ($m.Kind -eq 'unknown' -and -not $m.Actionable -and $m.Detail -match 'panicked') 'unknown exit keeps the line and does not retry'
$m = Get-Remediation -Evidence (E '' @('Error: failed to bind JSON-RPC 127.0.0.1:9944: Address already in use')) -Port 9944
Check ($m.Kind -eq 'port-foreign') 'bind error with no identifiable gpn owner -> foreign, not touched'

# -- the port owner that IS the build under test must not be stopped ---------
$child = Start-Process -FilePath 'sleep' -ArgumentList '20' -PassThru
Start-Sleep -Milliseconds 300
$savedBin = $BinDir
$BinDir = [System.IO.Path]::GetDirectoryName($child.Path)
Check (Test-OwnerIsExpected @{ Pid = $child.Id; Name = 'gpn_node' } '0.0.0') 'a listener from the expected tree is left alone'
$BinDir = '/nonexistent'
Check (-not (Test-OwnerIsExpected @{ Pid = $child.Id; Name = 'gpn_node' } '0.0.0')) 'a gpn process outside the expected tree is stale'
$BinDir = $savedBin
$child | Stop-Process -Force

# -- one remediation then success (stale child holding the DB lock) --------
Reset-Log
$script:phase = 0
$script:aliveProbe = 0
function global:Test-NodeAlive {
  param([string]$Version)
  if ($script:phase -eq 0) {
    $script:aliveProbe++
    if ($script:aliveProbe -le 2) { return $true }
    return $false
  }
  return $true
}
function global:Invoke-RestMethod {
  if ($script:phase -eq 0) { throw 'connection refused' }
  return [pscustomobject]@{ result = '0.3.59' }
}
function global:Get-StartEvidence {
  return @{ LastExit = ''; Tail = @('Error: The process cannot access the file because it is being used by another process. (os error 32)') }
}
$script:stopped = 0
function global:Stop-GpnFamily { param([int]$TimeoutSecs = 15) $script:stopped++; $script:phase = 1; return 0 }
function global:Stop-NodeTree { param([int]$GraceSeconds = 60) return $true }
function global:schtasks { $global:LASTEXITCODE = 0 }
function global:Test-PortListening { param([int]$Port) return $false }
$r = Confirm-Update -Expect '0.3.59' -FirstStart $true
Check ($r -and $r.Ok -and $r.Version -eq '0.3.59') 'remediation: lock -> stop family -> retry confirms'
Check ($script:stopped -eq 1) 'remediation: the family is stopped exactly once'
$logs = Get-Content $LogPath -Raw
Check ($logs -match 'remediate: lock held by another process') 'remediation: logged cause -> action -> retry'

# -- second failure after remediation must stop (never loops) --------------
Reset-Log
$script:phase = 0
$script:aliveProbe = 0
$script:stopped = 0
function global:Stop-GpnFamily { param([int]$TimeoutSecs = 15) $script:stopped++; return 0 }
$r = Confirm-Update -Expect '0.3.59' -FirstStart $true
Check (-not $r.Ok) 'second failure: reported as a failure'
Check ($script:stopped -eq 1) ('second failure: exactly one remediation (stopped=' + $script:stopped + ')')
Check ($r.Reason -match 'remediated once' -and $r.Reason -match 'retry then failed') 'second failure: says what was tried'
$logs = Get-Content $LogPath -Raw
Check (([regex]::Matches($logs, 'remediate: lock held by another process')).Count -le 1) 'second failure: it does not loop'

Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
Write-Host ("FAILURES: " + $script:fail)
exit $script:fail
