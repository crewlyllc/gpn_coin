#Requires -Version 5.1
<#
  test-install-version-guard.ps1 -- row 247, the second half.

  "install.ps1 refuses to install when its own version is older than the
   manifest's latest" is a claim about a Windows-only binary check. The bundle's
   installer is PowerShell, so the shipped text can be extracted and executed
   here (pwsh on Linux) with no Windows involved -- no copy of the logic, the
   functions themselves.

  Why it is written this way: the first version of this guard could silently
  disable itself. `[version]('0.3.70+build7')` throws, the cast sat inside a
  try/catch whose catch meant "offline, skip", and the rethrow keyed on
  '*older build*' while the throw said "OLD build" -- two paths where a refused
  install becomes a skipped check. A guard that can fail open is not a guard.
  Cases 5 and 9 below are those two paths, named.

  Run: pwsh -NoProfile -File scripts/tests/test-install-version-guard.ps1
  Exit 0 = all checks passed.
#>

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$installer = if ($env:GPN_INSTALLER) { $env:GPN_INSTALLER } else { Join-Path $repo 'install.ps1' }
if (-not (Test-Path -LiteralPath $installer)) { throw "install.ps1 not found at $installer" }

$checks = 0
$failed = 0
function Check([bool]$ok, [string]$name, [string]$detail) {
  $script:checks++
  if ($ok) { Write-Host ("PASS  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
  else { $script:failed++; Write-Host ("FAIL  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
}

# ---- extract the shipped block, do not re-type it -------------------------
$text = Get-Content -LiteralPath $installer -Raw
$m = [regex]::Match($text, '(?s)# BEGIN version-guard.*?# END version-guard')
Check $m.Success 'BEGIN/END version-guard markers exist in install.ps1' ''
if (-not $m.Success) { exit 1 }
. ([scriptblock]::Create($m.Value))

# ---- 1. an older install is refused, loudly ------------------------------
$err = $null
try { Assert-InstallNotOlderThanPublished ([version]'0.3.64') ([version]'0.3.70') }
catch { $err = $_.Exception.Message }
Check ([bool]$err) 'older binaries are refused' ''
Check ($err -like '*would leave an OLD version running*') 'refusal names the consequence' $err
Check ($err -like '*0.3.64*' -and $err -like '*0.3.70*') 'refusal names both versions' ''

# ---- 2. equal and newer are allowed -------------------------------------
$threw = $false
try { Assert-InstallNotOlderThanPublished ([version]'0.3.70') ([version]'0.3.70') } catch { $threw = $true }
Check (-not $threw) 'equal version is allowed' ''
$threw = $false
try { Assert-InstallNotOlderThanPublished ([version]'0.3.71') ([version]'0.3.70') } catch { $threw = $true }
Check (-not $threw) 'newer local build is allowed' ''

# ---- 3. unknown is not old (offline installs keep working) ---------------
$threw = $false
try { Assert-InstallNotOlderThanPublished $null ([version]'0.3.70') } catch { $threw = $true }
Check (-not $threw) 'unknown installed version does not refuse' 'offline bundle stays installable'
$threw = $false
try { Assert-InstallNotOlderThanPublished ([version]'0.3.64') $null } catch { $threw = $true }
Check (-not $threw) 'unknown published version does not refuse' ''

# ---- 4. version normalisation -------------------------------------------
$cases = @(
  @('0.3.70',        '0.3.70'),
  @(' v0.3.70 ',     '0.3.70'),
  @('0.3.70+build7', '0.3.70'),
  @('0.3.70-rc1',    '0.3.70'),
  @('0.3.70 (abc)',  '0.3.70')
)
foreach ($c in $cases) {
  $got = ConvertTo-GpnVersion $c[0]
  Check ("$got" -eq $c[1]) "ConvertTo-GpnVersion('$($c[0])')" "-> $got"
}

# ---- 5. the fail-open regression: +build metadata used to throw ----------
$threw = $false
$got = $null
try { $got = ConvertTo-GpnVersion '0.3.70+build7' } catch { $threw = $true }
Check (-not $threw -and "$got" -eq '0.3.70') 'build metadata parses instead of throwing' 'the old [version] cast threw here and disabled the guard'

# ---- 6. junk is null, never an exception --------------------------------
foreach ($junk in @('', 'unknown', $null, 'not-a-version')) {
  $got = $null; $threw = $false
  try { $got = ConvertTo-GpnVersion $junk } catch { $threw = $true }
  Check (-not $threw -and $null -eq $got) "ConvertTo-GpnVersion('$junk') is null, not a throw" ''
}

# ---- 7. no binaries yet = unknown, not a crash --------------------------
$missing = Join-Path ([IO.Path]::GetTempPath()) ("gpn-guard-none-" + [guid]::NewGuid().ToString('N'))
$v = Get-InstalledNodeVersion $missing
Check ($null -eq $v) 'missing gpn-node.exe reads as unknown version' ''

# ---- 8. the catch cannot swallow a refusal -----------------------------
$match = [regex]::Match($text, "(?s)catch \{\s*if \(\`$_\.Exception\.Message -like \('\*' \+ \`$script:VersionGuardRefusal \+ '\*'\)\) \{ throw \}")
Check $match.Success 'catch rethrows on the refusal phrase it matches' ''
# the phrase the function throws must be the phrase the variable holds
$phraseBlock = [regex]::Match($text, "(?s)\`$script:VersionGuardRefusal = '([^']+)'")
Check $phraseBlock.Success 'VersionGuardRefusal phrase is declared' ($phraseBlock.Groups[1].Value)
# the source splits the sentence across a -f and a concatenation, so compare the
# message the function actually throws (case 1), not the source text
Check ($err -like ('*' + $phraseBlock.Groups[1].Value + '*')) 'the thrown message contains the refusal phrase' ''

# ---- 10. row 297: the released exe carries NO version resource ----------
# Measured on the clean GPN-210 guest 2026-11-27: (Get-Item C:\gpn\bin\gpn-node.exe).VersionInfo
# had neither ProductVersion nor FileVersion, so Get-InstalledNodeVersion returned $null and a
# stale install printed "version check skipped". The guard has to ask the binary itself.
# $IsWindows does not exist in Windows PowerShell 5.1 (it is $null, so `-not $IsWindows` is
# TRUE on a real Windows box) -- key on the environment variable instead, which is set on both.
$onWindows = ($env:OS -eq 'Windows_NT')
if (-not $onWindows) {
  $runDir = Join-Path ([IO.Path]::GetTempPath()) ("gpn-guard-run-" + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $runDir | Out-Null
  $runExe = Join-Path $runDir 'gpn-node.exe'
  Set-Content -LiteralPath $runExe -Value "#!/bin/sh`necho 'gpn-node 0.3.77 (spec 55, abc1234)'" -NoNewline
  & chmod +x $runExe
  $vi = (Get-Item -LiteralPath $runExe).VersionInfo
  Check (-not $vi.ProductVersion -and -not $vi.FileVersion) 'the fixture carries no version resource' 'same shape as the released exe'
  $got = Get-InstalledNodeVersion $runDir
  Check ("$got" -eq '0.3.77') 'version read by running the binary when metadata is empty' "-> $got"

  $slowDir = Join-Path ([IO.Path]::GetTempPath()) ("gpn-guard-slow-" + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $slowDir | Out-Null
  $slowExe = Join-Path $slowDir 'gpn-node.exe'
  Set-Content -LiteralPath $slowExe -Value "#!/bin/sh`nsleep 30" -NoNewline
  & chmod +x $slowExe
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $got = Get-InstalledNodeVersion $slowDir
  $sw.Stop()
  Check ($null -eq $got) 'a binary that never answers reads as unknown, not a hang' ('{0:N1}s' -f $sw.Elapsed.TotalSeconds)
  Check ($sw.Elapsed.TotalSeconds -lt 20) 'the fallback is time-bounded' ('{0:N1}s' -f $sw.Elapsed.TotalSeconds)

  # 'gpn-node.exe 0.3.84' is the exact string box 1's released Windows binary prints
  # (measured 2026-11-28) -- the .exe suffix is why the first cut of this regex missed it.
  foreach ($c in @(@('gpn-node 0.3.77', '0.3.77'), @('gpn-node 0.3.77 (spec 55, abc1234)', '0.3.77'), @("`r`n0.3.77`r`n", '0.3.77'), @('gpn-node.exe 0.3.84', '0.3.84'))) {
    $p = ConvertFrom-NodeVersionOutput $c[0]
    Check ("$p" -eq $c[1]) "ConvertFrom-NodeVersionOutput('$($c[0].Trim())')" "-> $p"
  }
  foreach ($junk in @('', 'gpn-node', 'error: unknown flag')) {    $p = ConvertFrom-NodeVersionOutput $junk
    Check ($null -eq $p) "ConvertFrom-NodeVersionOutput('$junk') is null" ''
  }
} else {
  Write-Host 'SKIP  row 297 fallback cases (fixtures are shell scripts, this is Windows)'
}

# ---- 9. the old fail-open shape is gone --------------------------------
Check (-not ($text -match '\[version\]\(\$here -replace')) 'no unguarded [version] cast on the installed version' 'the old expression threw on +build'
Check (-not ($text -match "\*older build\*")) 'no stale *older build* message match' 'it never matched the throw text'

Write-Host ""
Write-Host ("{0} checks, {1} failed" -f $checks, $failed)
if ($failed -gt 0) { exit 1 }
exit 0
