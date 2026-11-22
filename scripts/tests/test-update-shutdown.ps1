# Behavioural tests for #236: never begin an update while Windows is going down.
#
# The field failure (5th machine, 2026-11-21): the update wrapper stopped the
# node and applied a new build while the box was mid shutdown; the start was
# refused at load (0xC000026B) and the only attempt of that boot was spent on it.
# #235 makes that refusal safe; this row stops the cycle being started at all.
#
# Extraction follows test-update-defer.ps1: take the update wrapper's function
# bodies out of install.ps1 (up to the self-test marker) and dot them in.
#
# Covered:
#   A  metrics probe says a shutdown began      -> pending, reason names it
#   B  metrics probe says no shutdown           -> not pending
#   C  registry probe reports one reason        -> pending, reason carried
#   D  both probes report                       -> both reasons present
#   E  registry probe returns nothing / null    -> not pending, no crash
#   F  the default probes run on this host      -> a hashtable, never a throw
#   G  the check sits BEFORE the node is stopped and before --apply
#   H  a pending shutdown logs `deferred: system is shutting down` and exits 75
#   I  nothing is stopped or installed in that branch (no Stop-NodeTree / --apply)
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
$installer = Join-Path $root 'install.ps1'
$text = Get-Content -LiteralPath $installer -Raw

$marker = '$UpdateWrapperTemplate' + " = @'"
$start = $text.IndexOf($marker)
if ($start -lt 0) { throw "could not find the update wrapper template in $installer" }
$start = $text.IndexOf("`n", $start) + 1
$end = $text.IndexOf("`n'@", $start)
if ($end -lt 0) { throw "could not find the end of the update wrapper template" }
$body = $text.Substring($start, $end - $start)

$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('gpn-sd-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmpRoot | Out-Null
$env:USERPROFILE = Join-Path $tmpRoot 'home'
New-Item -ItemType Directory -Force -Path $env:USERPROFILE | Out-Null
$InstallDir = Join-Path $tmpRoot 'gpn'
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$LogPath = Join-Path $tmpRoot 'update.log'
$body = $body.Replace('__INSTALL_DIR__', $InstallDir.Replace('\', '/'))
$selfTest = $body.IndexOf('# Optional ops self-test')
if ($selfTest -lt 0) { throw 'the self-test marker moved; this test extracts functions only' }
. ([scriptblock]::Create($body.Substring(0, $selfTest)))

$script:fail = 0
function Ok($name, $cond, $detail) {
  if ($cond) { Write-Host ("ok   " + $name) }
  else { Write-Host ("FAIL " + $name + "  " + $detail); $script:fail++ }
}

# ---- A/B: the metrics probe decides on its own ----------------------------
$r = Get-ShutdownState -MetricsProbe { 1 } -RegistryProbe { @() }
Ok 'A a shutdown in progress is pending' ($r.Pending) 'not pending'
Ok 'A the reason names SM_SHUTTINGDOWN' ($r.Why -like '*SM_SHUTTINGDOWN*') ("why=" + $r.Why)

$r = Get-ShutdownState -MetricsProbe { 0 } -RegistryProbe { @() }
Ok 'B no shutdown and no pending reboot is not pending' (-not $r.Pending) ("why=" + $r.Why)
Ok 'B the answer is a hashtable with both keys' ($null -ne $r.Pending -and $null -ne $r.Why) 'shape changed'

# ---- C/D: a pending reboot blocks the cycle too ---------------------------
$r = Get-ShutdownState -MetricsProbe { 0 } -RegistryProbe { @('a reboot is pending (CBS RebootPending)') }
Ok 'C a pending reboot is deferred' ($r.Pending) 'not pending'
Ok 'C the reason is carried through' ($r.Why -like '*CBS RebootPending*') ("why=" + $r.Why)

$r = Get-ShutdownState -MetricsProbe { 1 } -RegistryProbe { @('a reboot is pending (Windows Update RebootRequired)') }
Ok 'D both signals are reported' ($r.Pending -and $r.Why -like '*SM_SHUTTINGDOWN*' -and $r.Why -like '*RebootRequired*') ("why=" + $r.Why)

# ---- E: never a guess, never a crash --------------------------------------
$r = Get-ShutdownState -MetricsProbe { 0 } -RegistryProbe { @() }
Ok 'E an empty registry probe is not pending' (-not $r.Pending) ("why=" + $r.Why)
$r = Get-ShutdownState -MetricsProbe { 0 } -RegistryProbe { $null }
Ok 'E a null registry probe does not crash' (-not $r.Pending) ("why=" + $r.Why)
$r = Get-ShutdownState -MetricsProbe { $null } -RegistryProbe { @() }
Ok 'E an unreadable metric is not treated as a shutdown' (-not $r.Pending) ("why=" + $r.Why)

# ---- F: the real probes run on a real machine -----------------------------
$live = Get-ShutdownState
Ok 'F the default probes return a hashtable' ($null -ne $live.Pending) 'no answer'
Ok 'F a healthy host is not deferred' (-not $live.Pending) ("why=" + $live.Why)

# ---- G: it must run before anything is stopped or installed ---------------
$stopAt = $body.IndexOf("Write-Log 'update available - stopping GPN-Node before applying")
$checkAt = $body.IndexOf('$sd = Get-ShutdownState')
$applyAt = $body.IndexOf('& $updater --apply')
Ok 'G the shutdown check exists before the stop' ($checkAt -gt 0 -and $checkAt -lt $stopAt) ("check=" + $checkAt + " stop=" + $stopAt)
Ok 'G it runs before --apply' ($checkAt -gt 0 -and $checkAt -lt $applyAt) ("check=" + $checkAt + " apply=" + $applyAt)

# ---- H/I: the deferral branch itself -------------------------------------
$branchStart = $body.IndexOf('if ($sd.Pending) {', $checkAt)
$branchEnd = if ($branchStart -ge 0) { $body.IndexOf("`n  Write-Log 'update available", $branchStart) } else { -1 }
Ok 'H the deferral branch exists' ($branchStart -gt 0 -and $branchEnd -gt $branchStart) 'missing'
$branch = if ($branchEnd -gt $branchStart) { $body.Substring($branchStart, $branchEnd - $branchStart) } else { '' }
Ok 'H it logs the exact line the row requires' ($branch.Contains('deferred: system is shutting down')) 'message missing'
Ok 'H it exits 75 (deferred, not failed)' ($branch.Contains('exit 75')) 'wrong exit code'
Ok 'I it does not stop the node' (-not $branch.Contains('Stop-NodeTree')) 'stop inside the branch'
Ok 'I it does not install anything' (-not ($branch.Contains('--apply'))) 'apply inside the branch'
Ok 'I it leaves the node running' ($branch.Contains('the node was NOT stopped')) 'reassurance missing'

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
