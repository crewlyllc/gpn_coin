# Behavioural tests for the generated Windows updater's start path (#233).
#
# 5th machine, 2026-11-21: a new node died at load with 0xC000026B
# (STATUS_DLL_INIT_FAILED_LOGOFF) on a box that was not shutting down, because
# the replacement was started while the old tree was still being torn down /
# from the console that was dying. This script pins the two properties that
# remove that class:
#   A  an old gpn* process is still present      -> NO start, log says why
#   B  tree gone, schtasks works                 -> start, log records the
#                                                   stop time before the start
#   C  schtasks fails                            -> detached wscript fallback
#   D  both start paths fail                     -> returns false, logs FAILED
#   E  the new node's real parent is recorded    -> "parent wscript.exe"
#
# It extracts Wait-NodeExit / Stop-NodeTree / Start-NodeAfterStop out of
# install.ps1's $UpdateWrapperTemplate (nothing executes) and stubs the IO. Runs
# on Linux or Windows under pwsh 7.
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
$installer = Join-Path $root 'install.ps1'
$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('gpn-start-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmpRoot | Out-Null

$text = Get-Content -LiteralPath $installer -Raw
$marker = '$UpdateWrapperTemplate' + " = @'"
$start = $text.IndexOf($marker)
if ($start -lt 0) { throw "could not find the update wrapper template in $installer" }
$start = $text.IndexOf("`n", $start) + 1
$end = $text.IndexOf("`n'@", $start)
if ($end -lt 0) { throw "could not find the end of the update wrapper template" }
$body = $text.Substring($start, $end - $start)

# The slice that holds the three functions under test.
$from = $body.IndexOf('# True once no GPN node process is left')
if ($from -lt 0) { throw 'the Wait-NodeExit marker moved; this test extracts functions only' }
$to = $body.IndexOf('# One updater at a time')
if ($to -lt 0 -or $to -le $from) { throw 'the Start-NodeAfterStop slice moved; update this test' }
$slice = $body.Substring($from, $to - $from)

$InstallDir = Join-Path $tmpRoot 'gpn'
$NodeTask = 'GPN-Node'
$LogPath = Join-Path $tmpRoot 'update.log'
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

function Write-Log {
  param([string]$Message)
  Add-Content -Path $LogPath -Value ('[' + (Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + '] ' + $Message)
}
function Start-Sleep { param($Milliseconds, $Seconds) }
# IO stubs, replaced per case.
function Get-Process { }
function Get-CimInstance { }
function Invoke-CimMethod { }
function schtasks { }

# The functions are extracted as text and evaluated now; the stubs above must
# already exist so the extracted code binds to them at call time.
. ([scriptblock]::Create($slice))

$script:fail = 0
function Check($cond, $what) {
  if ($cond) { Write-Host "ok   $what" } else { Write-Host "FAIL $what"; $script:fail++ }
}
function Reset-Log { if (Test-Path $LogPath) { Remove-Item $LogPath -Force } }
function LogText { if (Test-Path $LogPath) { return (Get-Content $LogPath -Raw) } return '' }

# -- A. an old tree is still present -> refuse to start --------------------
Reset-Log
function global:Get-Process { [pscustomobject]@{ ProcessName = 'gpn_node'; Id = 4242 } }
function global:schtasks { $global:StartCalled = $true; $global:LASTEXITCODE = 0 }
$global:StartCalled = $false
$r = Start-NodeAfterStop 'after apply' 1
Check ($r -eq $false) 'A: returns false while a gpn process is present'
Check ($global:StartCalled -eq $false) 'A: no start is attempted'
Check ((LogText) -match 'start refused \(after apply\)') 'A: the log names the refusal and the reason'

# -- B. tree gone, schtasks works -> start after the stop ------------------
Reset-Log
function global:Get-Process { }
function global:schtasks { $global:StartCalled = $true; $global:LASTEXITCODE = 0 }
function global:Get-CimInstance { [pscustomobject]@{ Name = 'wscript.exe'; ParentProcessId = 1 } }
$global:StartCalled = $false
Write-Log 'GPN-Node stopped'
$r = Start-NodeAfterStop 'after apply'
$log = LogText
Check ($r -eq $true) 'B: returns true when the tree is gone and the start succeeds'
Check ($global:StartCalled -eq $true) 'B: the start is attempted'
Check ($log -match 'start \(after apply\): schtasks \(Task Scheduler service\)') 'B: the log records the start method'
Check ($log.IndexOf('GPN-Node stopped') -lt $log.IndexOf('start (after apply)')) 'B: the start line comes after the stop line'
Check ($log -match 'stopped tree gone at \d{4}-\d\d-\d\d \d\d:\d\d:\d\d') 'B: the stop time is recorded'

# -- C. schtasks fails -> detached wscript fallback -----------------------
# (Get-Process stays empty so the liveness wait returns at once.)
function global:schtasks { $global:LASTEXITCODE = 1 }
function global:Invoke-CimMethod { [pscustomobject]@{ ReturnValue = 0; ProcessId = 777 } }
Reset-Log
$r = Start-NodeAfterStop 'rollback'
$log = LogText
Check ($r -eq $true) 'C: the detached fallback still starts the node'
Check ($log -match 'schtasks /Run exited 1') 'C: the log names the schtasks failure'
Check ($log -match 'start \(rollback\): Win32_Process\.Create \(wscript pid 777\)') 'C: the fallback is logged with its pid'

# -- D. both paths fail -> reported, not silently swallowed ----------------
function global:schtasks { $global:LASTEXITCODE = 1 }
function global:Invoke-CimMethod { [pscustomobject]@{ ReturnValue = 2; ProcessId = 0 } }
Reset-Log
$r = Start-NodeAfterStop 'after apply'
$log = LogText
Check ($r -eq $false) 'D: returns false when neither path started the node'
Check ($log -match 'detached start refused: Win32_Process\.Create returned 2') 'D: the CIM refusal is logged'
Check ($log -match 'start FAILED \(after apply\)') 'D: the failure is named'

# -- E. the new node's parent is recorded ----------------------------------
# Get-Process answers "nothing" for the liveness check and then (after the start)
# reports the node, which is also what lets Start-NodeAfterStop find its parent.
function global:Get-Process { if ($global:Started) { [pscustomobject]@{ ProcessName = 'gpn_node'; Id = 555 } } }
function global:schtasks { $global:Started = 1; $global:LASTEXITCODE = 0 }
function global:Get-CimInstance {
  param($ClassName, $Filter)
  if ($Filter -match '555') { return [pscustomobject]@{ ParentProcessId = 9 } }
  return [pscustomobject]@{ Name = 'wscript.exe' }
}
$global:Started = 0
Reset-Log
$r = Start-NodeAfterStop 'after apply'
Check ((LogText) -match 'node pid 555 parent wscript\.exe') 'E: the new node pid and its parent are recorded'

Write-Host ''
if ($script:fail -gt 0) { Write-Host "RESULT: FAIL ($script:fail)"; exit 1 }
Write-Host 'RESULT: PASS (all checks)'
exit 0
