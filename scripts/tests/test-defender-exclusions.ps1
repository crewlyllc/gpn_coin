# Behavioural tests for the generated Windows Defender helper (#209).
#
# The helper adds antivirus exclusions, which is a security decision: it must
# never happen without consent, never exclude a whole writable directory, never
# fail an install when Defender is absent, and must say how to undo itself.
#
# The helper lives inside install.ps1 as $DefenderHelperTemplate. This script
# extracts it, prepends stubs for the Defender cmdlets, and runs the result in a
# child pwsh -- a child, because the helper legitimately calls `exit`, which
# would otherwise end this test run. Runs on Linux or Windows under pwsh 7.
#
# Covered:
#   -  no consent            -> nothing added, one logged line naming the undo
#   -  consent + empty prefs -> 4 file paths + 2 process names, verified, undo
#   -  rerun                 -> idempotent ("already present", no duplicates)
#   -  no Defender cmdlets   -> logged no-op, exit 0 (install must not fail)
#   -  not admin             -> exit 2 with the elevated command
#   -  narrowness            -> every path is a file, no directory exclusion
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
$installer = Join-Path $root 'install.ps1'
$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('gpn-defender-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmpRoot | Out-Null

$text = Get-Content -LiteralPath $installer -Raw
$marker = '$DefenderHelperTemplate' + " = @'"
$start = $text.IndexOf($marker)
if ($start -lt 0) { throw "could not find `$DefenderHelperTemplate in $installer" }
$start = $text.IndexOf("`n", $start) + 1
$end = $text.IndexOf("`n'@", $start)
if ($end -lt 0) { throw 'could not find the end of the Defender helper template' }
$template = $text.Substring($start, $end - $start)

# Strip the template's own param block: the generated file supplies one
# (param() must be the first statement of a script, so the test file owns it).
$lines = $template -split "`n"
$bodyLines = @()
$stripped = $false
foreach ($l in $lines) {
  if (-not $stripped -and $l -match '^\s*param\(.*\)\s*$') { $stripped = $true; continue }
  $bodyLines += $l
}
if (-not $stripped) { throw 'the helper template has no single-line param() block' }
$body = $bodyLines -join "`n"

$script:fail = 0
function Check($cond, $what) {
  if ($cond) { Write-Host "ok   $what" } else { Write-Host "FAIL $what"; $script:fail++ }
}

$stub = @'
$env:OS = 'Windows_NT'
$script:StatePath = '__STATE__'
function Read-State {
  if (Test-Path $script:StatePath) {
    return (Get-Content -LiteralPath $script:StatePath -Raw | ConvertFrom-Json)
  }
  return [pscustomobject]@{ Paths = @(); Procs = @() }
}
function Save-State($s) { $s | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $script:StatePath -Encoding utf8 }
'@

$withDefender = @'
function Get-MpPreference {
  $s = Read-State
  return [pscustomobject]@{ ExclusionPath = @($s.Paths); ExclusionProcess = @($s.Procs) }
}
function Add-MpPreference {
  param($ExclusionPath, $ExclusionProcess)
  $s = Read-State
  $paths = @($s.Paths); $procs = @($s.Procs)
  if ($ExclusionPath) { $paths += @($ExclusionPath) }
  if ($ExclusionProcess) { $procs += @($ExclusionProcess) }
  Save-State ([pscustomobject]@{ Paths = $paths; Procs = $procs })
}
function Get-Service { param($Name) return [pscustomobject]@{ Status = 'Running' } }
function Get-MpComputerStatus {
  return [pscustomobject]@{ RealTimeProtectionEnabled = $true; AMRunningMode = 'Normal' }
}
function Get-CimInstance {
  param($Namespace, $ClassName)
  if ($ClassName -eq 'AntiVirusProduct') {
    return @([pscustomobject]@{ displayName = 'Bitdefender Antivirus Free Antimalware' })
  }
  return @()
}
'@

# Runs one case. Returns @{ Code; Out; Log; State }.
function Invoke-Helper {
  param(
    [string]$Consent,
    [switch]$Admin,
    [switch]$NoDefenderCmds,
    [string]$Name
  )
  $dir = Join-Path $tmpRoot $Name
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $state = Join-Path $dir 'state.json'
  $pre = $stub.Replace('__STATE__', $state)
  if ($Admin) { $pre += "`nfunction Test-DfAdmin { return `$true }`n" }
  else { $pre += "`nfunction Test-DfAdmin { return `$false }`n" }
  if (-not $NoDefenderCmds) { $pre += $withDefender }

  $file = Join-Path $dir 'fix-defender.ps1'
  $full = "param([string]`$Consent = 'no', [switch]`$Elevated)`n" + $pre +
          "`n" + $body.Replace('__INSTALL_DIR__', $dir)
  Set-Content -LiteralPath $file -Value $full -Encoding utf8

  $out = @(& (Get-Process -Id $PID).Path -NoProfile -ExecutionPolicy Bypass -File $file -Consent $Consent 2>&1 |
            ForEach-Object { [string]$_ })
  $code = $LASTEXITCODE
  $logPath = Join-Path $dir 'defender.log'
  $log = if (Test-Path $logPath) { Get-Content -LiteralPath $logPath -Raw } else { '' }
  $st = if (Test-Path $state) { Get-Content -LiteralPath $state -Raw | ConvertFrom-Json } else { $null }
  return [pscustomobject]@{ Code = $code; Out = ($out -join "`n"); Log = $log; State = $st; Dir = $dir }
}

# -- 1. no consent ---------------------------------------------------------
$r = Invoke-Helper -Consent 'no' -Admin -Name 'noconsent'
Check ($r.Code -eq 0) "no consent: exit 0 (got $($r.Code))"
Check ($null -eq $r.State) 'no consent: nothing written to the exclusion store'
Check ($r.Log -match 'no consent') 'no consent: the log says consent was withheld'
Check ($r.Log -match 'Remove-MpPreference -ExclusionPath') 'no consent: the log still names the undo command'

# -- 2. consent, nothing excluded yet --------------------------------------
$r = Invoke-Helper -Consent 'yes' -Admin -Name 'firstrun'
Check ($r.Code -eq 0) "consent: exit 0 (got $($r.Code))"
Check ($r.State.Paths.Count -eq 4) "consent: 4 path exclusions (got $($r.State.Paths.Count))"
Check ($r.State.Procs.Count -eq 2) "consent: 2 process exclusions (got $($r.State.Procs.Count))"
Check ($r.Log -match 'added and verified') 'consent: the log says what was added'
Check ($r.Log -match 'undo: Remove-MpPreference') 'consent: exactly one line with the undo command'
Check ($r.Log -match 'real-time protection on') 'consent: the log states real-time protection'
Check (($r.Log -split "`n" | Where-Object { $_ -match '^\[' }).Count -eq 1) 'consent: one clear log line'
Check ($r.Log -match 'Bitdefender') 'consent: a third-party antivirus is named when present' 

# -- 3. narrowness: every exclusion is a file, never a directory -----------
$dirs = @($r.State.Paths | Where-Object { $_ -notmatch '\.exe$' })
Check ($dirs.Count -eq 0) "narrow: every path ends in .exe (offenders: $($dirs -join ', '))"
Check (($r.State.Paths -join ',') -match 'gpn-node\.exe') 'narrow: gpn-node.exe is covered'
Check (($r.State.Paths -join ',') -notmatch 'update\\current\s*$') 'narrow: current\ itself is NOT excluded'

# -- 4. idempotent ---------------------------------------------------------
$r = Invoke-Helper -Consent 'yes' -Admin -Name 'firstrun'
Check ($r.Code -eq 0) "rerun: exit 0 (got $($r.Code))"
Check ($r.State.Paths.Count -eq 4) "rerun: still 4 paths (got $($r.State.Paths.Count))"
Check ($r.State.Procs.Count -eq 2) "rerun: still 2 processes (got $($r.State.Procs.Count))"
Check ($r.Log -match 'already present') 'rerun: says already present'

# -- 5. Defender absent: a logged no-op, never an install failure ----------
$r = Invoke-Helper -Consent 'yes' -Admin -NoDefenderCmds -Name 'nodefender'
Check ($r.Code -eq 0) "no Defender: exit 0 (got $($r.Code))"
Check ($r.Log -match 'not present') 'no Defender: the log says so'
Check ($null -eq $r.State) 'no Defender: nothing was attempted'

# -- 5b. Defender present but passive (a third-party AV owns the machine) ---
# This is box 1's real state (Bitdefender active, Defender passive): adding a
# preference fails 0x800106ba, so the helper must say so instead of trying.
$passiveStub = @'
function Get-MpPreference { return [pscustomobject]@{ ExclusionPath = @(); ExclusionProcess = @() } }
function Add-MpPreference { throw 'should not be called when Defender is passive' }
function Get-Service { param($Name) return [pscustomobject]@{ Status = 'Stopped' } }
function Get-MpComputerStatus { return [pscustomobject]@{ RealTimeProtectionEnabled = $false; AMRunningMode = 'Passive' } }
function Get-CimInstance { param($Namespace, $ClassName) return @([pscustomobject]@{ displayName = 'Bitdefender Antivirus Free Antimalware' }) }
'@
$dir = Join-Path $tmpRoot 'passive'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$file = Join-Path $dir 'fix-defender.ps1'
$full = "param([string]`$Consent = 'no', [switch]`$Elevated)`n" + ($stub.Replace('__STATE__', (Join-Path $dir 'state.json'))) +
        "`nfunction Test-DfAdmin { return `$true }`n" + $passiveStub + "`n" +
        $body.Replace('__INSTALL_DIR__', $dir)
Set-Content -LiteralPath $file -Value $full -Encoding utf8
$out = @(& (Get-Process -Id $PID).Path -NoProfile -ExecutionPolicy Bypass -File $file -Consent 'yes' 2>&1 | ForEach-Object { [string]$_ })
$code = $LASTEXITCODE
$log = Get-Content -LiteralPath (Join-Path $dir 'defender.log') -Raw
Check ($code -eq 0) "passive Defender: exit 0 (got $code)"
Check ($log -match 'not active') 'passive Defender: the log says Defender is not active'
Check ($log -match 'Bitdefender') 'passive Defender: the log names the real scanner'
Check ($log -notmatch '0x800106ba') 'passive Defender: no raw HRESULT'

# -- 6. not admin and non-interactive: says how to do it -------------------
$r = Invoke-Helper -Consent 'yes' -Name 'noadmin'
Check ($r.Code -eq 2) "not admin: exit 2 (got $($r.Code))"
Check ($r.Log -match 'needs an elevated run') 'not admin: the log names the elevated command'

# -- 7. the installer asks, and defaults to NO when it cannot ask ----------
$env:CI = '1'
$env:OS = 'Windows_NT'
$env:GPN_DEFENDER_EXCLUSION = '0'
$installText = Get-Content -LiteralPath $installer -Raw
Check ($installText -match '\$DefenderConsent = .ask.') 'installer: consent defaults to ask'
Check ($installText -match 'GPN_DEFENDER_EXCLUSION') 'installer: env override exists'
Check ($installText -match '\[Environment\]::UserInteractive') 'installer: it only prompts when interactive'
Check ($installText -match 'Repair-Defender') 'installer: the step is wired into the install'
Remove-Item Env:GPN_DEFENDER_EXCLUSION -ErrorAction SilentlyContinue
Remove-Item Env:CI -ErrorAction SilentlyContinue

Remove-Item -LiteralPath $tmpRoot -Recurse -Force -ErrorAction SilentlyContinue
if ($script:fail -gt 0) { Write-Host "FAILED: $script:fail check(s)"; exit 1 }
Write-Host 'all defender-exclusion checks passed'
exit 0
