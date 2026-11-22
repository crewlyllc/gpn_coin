# Behavioural tests for #235: never roll back on a teardown-class start failure.
#
# The field failure (5th machine, 2026-11-21): a start died with 0xC000026B
# (STATUS_DLL_INIT_FAILED_LOGOFF -- the window station was closing) while the
# box was shutting down, the updater called it a code fault, rolled 0.3.66 back
# to 0.3.64, and that build was refused the same way. The environment refused
# the process; the build was fine.
#
# Extraction follows test-update-exit-cause.ps1: take the update wrapper's
# function bodies out of install.ps1 (up to the self-test marker, so nothing
# executes) and dot them in. Runs on Linux or Windows under pwsh 7.
#
# Covered:
#   A  0xC000026B (signed)                 -> named, deferred
#   B  0xC000026B (unsigned)               -> named, deferred
#   C  0xC000026B (hex string)             -> named, deferred
#   D  0xC000013A console teardown          -> named, deferred
#   E  0xC0000142 / 0xC0000135 loader codes -> named, deferred
#   F  0xC0000005 access violation at load -> named, NOT deferred (build fault)
#   G  a record older than this attempt     -> ignored
#   H  an ordinary exit code (69)           -> not a loader status
#   I  no record / malformed JSON           -> empty, no crash
#   J  the deferral branch runs BEFORE any rollback and retries once after 10 s
#   K  a deferral reports DEFERRED with exit 75, not FAILED
#   L  the launcher's own report names 0xC000026B instead of "unknown"
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
$installer = Join-Path $root 'install.ps1'
$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('gpn-defer-' + [guid]::NewGuid().ToString('N'))
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
$InstallDir = Join-Path $tmpRoot 'gpn'
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$body = $body.Replace('__INSTALL_DIR__', $InstallDir.Replace('\', '/'))
$selfTest = $body.IndexOf('# Optional ops self-test')
if ($selfTest -lt 0) { throw 'the self-test marker moved; this test extracts functions only' }
. ([scriptblock]::Create($body.Substring(0, $selfTest)))

$LogPath = Join-Path $tmpRoot 'update.log'
$script:fail = 0

function Ok($name, $cond, $detail) {
  if ($cond) { Write-Host ("ok   " + $name) }
  else { Write-Host ("FAIL " + $name + "  " + $detail); $script:fail++ }
}

$now = [long]([DateTime]::UtcNow - [DateTime]'1970-01-01').TotalSeconds

function Rec([string]$code, [long]$age = 5) {
  return ('{"ts":"2026-11-21T19:00:00Z","ts_unix":' + ($now - $age) + ',"code":' + $code +
          ',"kind":"x","detail":"d","version":"0.3.67","os":"windows"}')
}
function Probe([string]$code, [long]$since = 0, [long]$age = 5) {
  return (Get-LoaderExit @{ LastExit = (Rec $code $age); Tail = @() } $(if ($since -gt 0) { $since } else { $now - 60 }))
}

# ---- A/B/C: the code that cost the 5th machine, in every form it arrives ----
foreach ($form in @(@('-1073741205','A signed'), @('3221226091','B unsigned'), @('"0xC000026B"','C hex string'))) {
  $r = Probe $form[0]
  Ok ($form[1] + ' is recognised') ($null -ne $r) 'null'
  Ok ($form[1] + ' names 0xC000026B') ($r -and $r.Hex -eq '0xC000026B') ("hex=" + $r.Hex)
  Ok ($form[1] + ' names the NTSTATUS') ($r -and $r.Name -eq 'STATUS_DLL_INIT_FAILED_LOGOFF') ("name=" + $r.Name)
  Ok ($form[1] + ' defers') ($r -and $r.Defer) 'not deferred'
}

# ---- D/E: the rest of the teardown/loader family ---------------------------
foreach ($pair in @(@('-1073741510','STATUS_CONTROL_C_EXIT'), @('-1073741502','STATUS_DLL_INIT_FAILED'), @('-1073741515','STATUS_DLL_NOT_FOUND'))) {
  $r = Probe $pair[0]
  Ok ($pair[1] + ' is named') ($r -and $r.Name -eq $pair[1]) ("name=" + $r.Name)
  Ok ($pair[1] + ' defers') ($r -and $r.Defer) 'not deferred'
  Ok ($pair[1] + ' line says deferred, not failed') ($r -and $r.Line.StartsWith('deferred: environment did not allow the process to start (0x')) $r.Line
}

# ---- F: an access violation at load is named but is the build's fault ------
$r = Probe '-1073741819'
Ok 'F 0xC0000005 is named' ($r -and $r.Hex -eq '0xC0000005') ("hex=" + $r.Hex)
Ok 'F 0xC0000005 does NOT defer' ($r -and -not $r.Defer) 'was deferred'

# ---- G/H/I: never quote what is not evidence ------------------------------
$r = Get-LoaderExit @{ LastExit = (Rec '-1073741205' 600); Tail = @() } $now
Ok 'G a record older than this attempt is ignored' ($null -eq $r) ("got " + $r.Hex)
$r = Probe '69'
Ok 'H an ordinary exit (69 no-ollama) is not a loader status' ($null -eq $r) ("got " + $r.Hex)
Ok 'I  no record is not a guess' ($null -eq (Get-LoaderExit @{ LastExit = ''; Tail = @() } 0)) 'got something'
Ok 'I  malformed JSON does not crash' ($null -eq (Get-LoaderExit @{ LastExit = '{not json'; Tail = @() } 0)) 'got something'

# ---- J: the ordering the row's acceptance actually turns on ---------------
$notConfirmed = $body.LastIndexOf('NOT CONFIRMED: ')
$loaderAt = $body.IndexOf('$loader = Get-LoaderExit', $notConfirmed)
$rollbackAt = $body.LastIndexOf('$rb = Invoke-Rollback')
Ok 'J the loader classification exists in the confirm failure path' ($loaderAt -gt $notConfirmed) ("at " + $loaderAt)
Ok 'J the classification runs BEFORE the rollback' ($loaderAt -gt 0 -and $rollbackAt -gt $loaderAt) ("loader=" + $loaderAt + " rollback=" + $rollbackAt)

$deferAt = $body.IndexOf('if ($loader -and $loader.Defer)', $notConfirmed)
$deferBranchEnd = $body.IndexOf('if ($loader) {', $deferAt)
Ok 'J the deferral branch exists' ($deferAt -gt 0 -and $deferBranchEnd -gt $deferAt) 'missing'
$branch = if ($deferAt -ge 0 -and $deferBranchEnd -gt $deferAt) { $body.Substring($deferAt, $deferBranchEnd - $deferAt) } else { '' }
Ok 'J the deferral branch attempts NO rollback' ($branch -ne '' -and -not $branch.Contains('Invoke-Rollback')) 'rollback inside the branch'
Ok 'J the deferral retries after 10 s' ($branch.Contains('Start-Sleep -Seconds 10')) 'no 10 s retry'
Ok 'J the deferral keeps the new build installed' ($branch.Contains('NO rollback was attempted')) 'message missing'
Ok 'J the deferral exits 75 (not 1)' ($branch.Contains('exit 75')) 'wrong exit code'

# ---- K: it must read as deferred, not as failed ---------------------------
$deferFn = $body.IndexOf('function Show-UpdateDeferred')
Ok 'K Show-UpdateDeferred exists' ($deferFn -gt 0) 'missing'
$fnBody = if ($deferFn -ge 0) { $body.Substring($deferFn, 700) } else { '' }
Ok 'K it logs DEFERRED' ($fnBody.Contains("'DEFERRED: '")) 'no DEFERRED line'
Ok 'K it says nothing was rolled back' ($fnBody.Contains('Nothing was rolled back')) 'missing reassurance'
Ok 'K the branch reports through it' ($branch.Contains('Show-UpdateDeferred')) 'not called'

# ---- L: the launcher's report names the status too ------------------------
$repMarker = '$ReportTemplate' + " = @'"
$repStart = $text.IndexOf($repMarker)
$repEnd = if ($repStart -ge 0) { $text.IndexOf("`n'@", $repStart) } else { -1 }
Ok 'L the report template exists' ($repStart -ge 0 -and $repEnd -gt $repStart) 'missing'
if ($repEnd -gt $repStart) {
  $rep = $text.Substring($repStart, $repEnd - $repStart)
  Ok 'L it names 0xC000026B (signed)' ($rep.Contains('-1073741205')) 'missing'
  Ok 'L it names 0xC000026B (unsigned)' ($rep.Contains('3221226091')) 'missing'
  Ok 'L it files it as session-closing' ($rep.Contains("session-closing")) 'missing'
  Ok 'L it names 0xC0000142 and 0xC0000005' ($rep.Contains('-1073741502') -and $rep.Contains('-1073741819')) 'missing'
}

# ---- the extracted code stays pure ASCII (Windows PowerShell 5.1 decoding) --
$nonAscii = 0
foreach ($ch in $body.ToCharArray()) { if ([int]$ch -gt 127) { $nonAscii++ } }
Ok 'ascii the wrapper template is pure ASCII' ($nonAscii -eq 0) ("$nonAscii non-ASCII chars")

if ($script:fail -gt 0) {
  Write-Host ("FAILURES: " + $script:fail)
  exit 1
}
Write-Host 'RESULT: PASS (all checks)'
exit 0
