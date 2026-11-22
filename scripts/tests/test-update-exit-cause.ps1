# Behavioural tests for #234: a failed start must name its cause.
#
# The field failure (5th machine, 2026-11-21) was a confirm line that said only
# "no process ever appeared" while the node's own ledger had already recorded a
# reason. #234 gives the ledger two distinct slugs for the two things a
# CTRL_SHUTDOWN_EVENT can mean (`console-teardown` vs `system-shutdown`) and makes
# the updater quote the record instead of reporting the symptom alone.
#
# Extraction follows test-update-confirm.ps1: take the function bodies out of
# install.ps1's $UpdateWrapperTemplate (up to the self-test marker, so nothing
# executes) and dot them in. Runs on Linux or Windows under pwsh 7.
#
# Covered:
#   A  a teardown record written during this attempt        -> quoted, named
#   B  a shutdown record written during this attempt        -> quoted, distinct
#   C  a record older than this attempt                     -> ignored
#   D  no record at all                                     -> empty, no guessing
#   E  malformed JSON                                       -> empty, no crash
#   F  the call site exists in the confirm failure path
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
$installer = Join-Path $root 'install.ps1'
$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('gpn-exit-cause-' + [guid]::NewGuid().ToString('N'))
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

# -- A: a teardown written during this attempt is quoted, and named --------------------
$teardown = '{"ts":"2026-11-21T18:35:00Z","ts_unix":' + ($now - 5) + ',"code":-1073741510,"kind":"console-teardown","detail":"the console this node was attached to was destroyed (a console teardown, not a machine shutdown)","remediation":"nothing to fix","version":"0.3.67","os":"windows"}'
$noteA = Get-ExitCauseNote @{ LastExit = $teardown; Tail = @() } ($now - 60)
Ok 'A teardown record is quoted' ($noteA -ne '') $noteA
Ok 'A teardown record names the kind' ($noteA.Contains('console-teardown')) $noteA
Ok 'A teardown record says what it is not' ($noteA.Contains('not a machine shutdown')) $noteA
Ok 'A teardown record quotes the exit code' ($noteA.Contains('-1073741510')) $noteA

# -- B: a real shutdown is a different slug, not silently identical --------------------
$shutdown = '{"ts":"2026-11-21T18:35:00Z","ts_unix":' + ($now - 5) + ',"code":0,"kind":"system-shutdown","detail":"the machine is shutting down (SM_SHUTTINGDOWN=1)","remediation":"nothing to fix","version":"0.3.67","os":"windows"}'
$noteB = Get-ExitCauseNote @{ LastExit = $shutdown; Tail = @() } ($now - 60)
Ok 'B shutdown record names its own kind' ($noteB.Contains('system-shutdown')) $noteB
Ok 'B the two events do not read the same' ($noteB -ne $noteA) $noteB

# -- C: a record that predates this attempt is not evidence about it --------------------
$noteC = Get-ExitCauseNote @{ LastExit = $teardown; Tail = @() } ($now + 60)
Ok 'C a stale record is ignored' ($noteC -eq '') $noteC

# -- D: no record means no claim --------------------
$noteD = Get-ExitCauseNote @{ LastExit = ''; Tail = @() } ($now - 60)
Ok 'D no record -> empty note' ($noteD -eq '') $noteD
Ok 'D no evidence object -> empty note' ((Get-ExitCauseNote $null 0) -eq '') 'null evidence'

# -- E: garbage in the ledger must not throw --------------------
$noteE = Get-ExitCauseNote @{ LastExit = 'not json at all'; Tail = @() } ($now - 60)
Ok 'E malformed JSON -> empty note' ($noteE -eq '') $noteE

# -- F: the call site exists (a definition with no caller is the failure mode) -
Ok 'F the confirm failure path quotes the cause' `
  ($body.Contains('Get-ExitCauseNote (Get-StartEvidence) $cycleStartUnix')) 'call site missing'
Ok 'F the cycle start is baselined somewhere' `
  ($body.Contains('$cycleStartUnix = [long]')) 'no baseline captured'

Write-Host ''
if ($script:fail -eq 0) { Write-Host 'RESULT: PASS (all checks)'; exit 0 }
Write-Host ('RESULT: FAIL (' + $script:fail + ' failures)')
exit 1
