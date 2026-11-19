# verify-209-defender.ps1 — the Windows-Defender proof for GPN-LIST row 209.
#
# WHY THIS FILE EXISTS
#   209's code (Defender exclusions + "blocked by security software" classification) is
#   implemented and tested, but its DONE WHEN needs a machine where Windows Defender is the
#   ACTIVE scanner with real-time protection ON. Box 1 runs Bitdefender (Defender passive,
#   WinDefend cannot be started, Add-MpPreference fails 0x800106ba), so the proof could not be
#   observed there. Box 3 is the machine that can prove it — this script is what to run the
#   moment box 3 is back on the tailnet.
#
# RUN IT (from the Linux box, over the userspace SOCKS proxy — NOT via a runner dispatch):
#   scripts/box3.sh "powershell -ExecutionPolicy Bypass -File C:\gpn\verify-209-defender.ps1 -Consent yes"
#
# Pass criteria are printed as PASS/FAIL lines and a final SUMMARY; paste the whole output into
# docs/releases/<version>.md next to the 209 box. A single FAIL means 209 is not done.

[CmdletBinding()]
param(
    [ValidateSet('yes', 'no')] [string] $Consent = 'yes',
    [string] $InstallRoot = 'C:\gpn',
    [switch] $SkipUpdateCycle
)

$ErrorActionPreference = 'Stop'
$fail = 0
$pass = 0

function Check([string] $name, [scriptblock] $test) {
    try {
        $ok = & $test
        if ($ok) { $script:pass++; Write-Host "PASS  $name" }
        else { $script:fail++; Write-Host "FAIL  $name" }
    } catch {
        $script:fail++; Write-Host "FAIL  $name  ($($_.Exception.Message))"
    }
}

Write-Host "== verify-209-defender  $(Get-Date -Format o) =="
Write-Host "host: $env:COMPUTERNAME   user: $env:USERNAME   admin: $((New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"

# ---- preconditions: this machine must be the one that can answer the question ----------
$mp = $null
try { $mp = Get-MpComputerStatus } catch { $mp = $null }
$defenderActive = $mp -and $mp.RealTimeProtectionEnabled -and $mp.AntivirusEnabled
Write-Host "defender: present=$([bool]$mp) realtime=$($mp.RealTimeProtectionEnabled) antivirus=$($mp.AntivirusEnabled) amrunning=$($mp.AMServiceEnabled)"

if (-not $defenderActive) {
    Write-Host ""
    Write-Host "STOP: Windows Defender is not the active scanner on this host."
    Write-Host "      209 can only be proven where Defender has real-time protection ON."
    Write-Host "      Third-party AV found: $((Get-CimInstance -Namespace root\SecurityCenter2 -ClassName AntiVirusProduct -ErrorAction SilentlyContinue | ForEach-Object { $_.displayName }) -join ', ')"
    Write-Host "SUMMARY: 0 pass, 0 fail, NOT PROVEN (wrong host)"
    exit 2
}

$exes = @(
    (Join-Path $InstallRoot 'bin\gpn-node.exe'),
    (Join-Path $InstallRoot 'data\update\current\gpn-node.exe')
)

# ---- 1. the exclusions the installer adds are the ones we expect ------------------------
$pre = (Get-MpPreference).ExclusionPath
Write-Host "pre-existing exclusions: $($pre -join '; ')"

Check 'install.ps1 ships a Defender helper' {
    Test-Path (Join-Path $InstallRoot 'fix-defender.ps1')
}

if (Test-Path (Join-Path $InstallRoot 'fix-defender.ps1')) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $InstallRoot 'fix-defender.ps1') -Consent $Consent | Write-Host
}

$post = (Get-MpPreference).ExclusionPath
Write-Host "exclusions after helper: $($post -join '; ')"

if ($Consent -eq 'yes') {
    foreach ($e in $exes) {
        Check "exclusion present: $e" { $post -contains $e }
    }
    Check 'no directory-wide exclusion was added' {
        -not ($post | Where-Object { $_.TrimEnd('\') -eq $InstallRoot.TrimEnd('\') -or $_.TrimEnd('\') -eq (Join-Path $InstallRoot 'bin').TrimEnd('\') })
    }
    Check 'helper is idempotent (second run adds nothing new)' {
        $before = (Get-MpPreference).ExclusionPath
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $InstallRoot 'fix-defender.ps1') -Consent yes | Out-Null
        $after = (Get-MpPreference).ExclusionPath
        ($before -join '|') -eq ($after -join '|')
    }
} else {
    Check 'consent withheld adds nothing' {
        -not ($post | Where-Object { $exes -contains $_ })
    }
}

# ---- 2. a real update with real-time protection on: nothing is quarantined --------------
if (-not $SkipUpdateCycle) {
    $before = (Get-MpThreatDetection | Measure-Object).Count
    Write-Host "threat detections before: $before"
    try {
        & (Join-Path $InstallRoot 'bin\gpn_update.exe') --apply --timeout 900 | Write-Host
    } catch {
        Write-Host "update cycle error: $($_.Exception.Message)"
    }
    $after = (Get-MpThreatDetection | Measure-Object).Count
    Write-Host "threat detections after: $after"
    Check 'no new Defender detection during one update cycle' { $after -le $before }
    Check 'node is running after the update' {
        $p = Get-Process gpn-node -ErrorAction SilentlyContinue
        [bool]$p
    }
}

# ---- 3. the classification: a blocked binary must be NAMED, not time out ----------------
# Delete the staged binary the way quarantine would, then let the updater's own confirm run.
$staged = $exes[1]
$backup = "$staged.bak-verify209"
if (Test-Path $staged) {
    Copy-Item $staged $backup -Force
    Remove-Item $staged -Force
    try {
        $out = & (Join-Path $InstallRoot 'bin\gpn_update.exe') --confirm --version 9.9.9 --timeout 20 2>&1 | Out-String
        Write-Host $out.Trim()
        Check 'missing binary produces signal=security' { $out -match 'signal=security' }
        Check 'the message says it is not a timeout'   { $out -match 'not a timeout' }
        Check 'the message names the excluded paths'   { $out -match 'gpn-node\.exe' }
    } finally {
        if (Test-Path $backup) { Move-Item $backup $staged -Force }
    }
} else {
    Write-Host "SKIP  blocked-binary case: $staged not present"
}

Write-Host ""
Write-Host "SUMMARY: $pass pass, $fail fail"
if ($fail -eq 0) { Write-Host "209 PROVEN on this host — paste this output into the checklist and tick the box." }
else             { Write-Host "209 NOT proven — $fail check(s) failed; do not tick." }
exit $(if ($fail -eq 0) { 0 } else { 1 })
