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

# ---- 9. the old fail-open shape is gone --------------------------------
Check (-not ($text -match '\[version\]\(\$here -replace')) 'no unguarded [version] cast on the installed version' 'the old expression threw on +build'
Check (-not ($text -match "\*older build\*")) 'no stale *older build* message match' 'it never matched the throw text'

Write-Host ""
Write-Host ("{0} checks, {1} failed" -f $checks, $failed)
if ($failed -gt 0) { exit 1 }
exit 0
