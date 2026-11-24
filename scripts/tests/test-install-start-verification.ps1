#Requires -Version 5.1
<#
  test-install-start-verification.ps1 -- row 259.

  "install.ps1 prints 'node started (GPN-Node)' when the task did not start."
  Field failure 2026-11-24 on GPN-209 (headless Hyper-V guest): `schtasks /Run`
  returned 0, the task stayed Ready with LastTaskResult 267011 (0x41303, "task
  has not yet run"), no gpn-node process existed and no node.log was written --
  and the installer said the node had started.

  The claim under test: the installer now waits for a real witness (a live
  gpn-node process, or the RPC answering) before it says anything, and when
  there is none it names the principal/logon type and exits non-zero.

  The functions are extracted from the shipped install.ps1 by AST (not
  re-typed) and driven with injected probes, so the headless case runs on Linux
  pwsh with no Windows box involved.

  Run: pwsh -NoProfile -File scripts/tests/test-install-start-verification.ps1
  Exit 0 = all checks passed.
#>

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$installer = if ($env:GPN_INSTALLER) { $env:GPN_INSTALLER } else { Join-Path $root 'install.ps1' }
if (-not (Test-Path -LiteralPath $installer)) { throw "install.ps1 not found at $installer" }

$script:checks = 0
$script:failed = 0
function Check([bool]$ok, [string]$name, [string]$detail) {
  $script:checks++
  if ($ok) { Write-Host ("PASS  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
  else { $script:failed++; Write-Host ("FAIL  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
}

# ---- the shipped text, parsed and extracted -------------------------------
$raw = Get-Content -LiteralPath $installer -Raw
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$errors)
if ($errors -and $errors.Count -gt 0) { throw ('install.ps1 does not parse: ' + $errors[0].Message) }
Check $true 'install.ps1 parses' ''

function Get-FunctionText([string]$name) {
  $f = $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true) | Select-Object -First 1
  if (-not $f) { return $null }
  return $f.Extent.Text
}

foreach ($fn in @('Confirm-NodeStarted', 'Get-NodeTaskDiagnosis', 'Get-LiveNodeProcess')) {
  $txt = Get-FunctionText $fn
  Check ([bool]$txt) "function $fn exists in install.ps1" ''
  if ($txt) { . ([scriptblock]::Create($txt)) }
}
if (-not (Get-FunctionText 'Confirm-NodeStarted')) {
  Write-Host ''
  Write-Host ("FAILED: the start verification is absent ({0} of {1} checks failed)" -f $script:failed, $script:checks) -ForegroundColor Red
  exit 1
}

# Stubs for the defaults the shipped function references (never used when a
# probe is injected, but the default parameter values must resolve).
function Get-LiveNodeProcess { }
function Test-NodeUp { }
$NodeTask = 'GPN-Node'

# ---- A: a live process is a witness on the first poll --------------------
$script:procCalls = 0
$r = Confirm-NodeStarted -Seconds 0 -PollSeconds 0 `
  -ProcessProbe { $script:procCalls++; [pscustomobject]@{ Id = 4242 } } `
  -RpcProbe { $false } `
  -TaskProbe { [pscustomobject]@{ State = 'Running'; Principal = 'Interactive'; UserId = 'X'; LastTaskResult = '0' } }
Check ($r.Started -eq $true) 'A process present -> started' ("Started=" + $r.Started)
Check ($r.Witness -eq 'process') 'A witness recorded as process' ("Witness=" + $r.Witness)

# ---- B: the GPN-209 headless case -> not started, with the real cause ----
$headless = Confirm-NodeStarted -Seconds 0 -PollSeconds 0 `
  -ProcessProbe { $null } `
  -RpcProbe { $false } `
  -TaskProbe { [pscustomobject]@{ State = 'Ready'; Principal = 'Interactive'; UserId = 'GPN-209\Administrator'; LastTaskResult = '267011' } }
Check ($headless.Started -eq $false) 'B headless: no process, no RPC -> NOT started' ("Started=" + $headless.Started)
Check ($headless.Principal -eq 'Interactive') 'B the principal logon type is reported' ("Principal=" + $headless.Principal)
Check ($headless.LastTaskResult -eq '267011') 'B 267011/0x41303 is reported' ("LastTaskResult=" + $headless.LastTaskResult)
Check ($headless.State -eq 'Ready') 'B the task state is reported' ("State=" + $headless.State)

# ---- C: a slow but real start is waited for, not declared dead -----------
$script:polls = 0
$slow = Confirm-NodeStarted -Seconds 30 -PollSeconds 0 `
  -ProcessProbe { $script:polls++; if ($script:polls -ge 3) { [pscustomobject]@{ Id = 77 } } else { $null } } `
  -RpcProbe { $false } `
  -TaskProbe { [pscustomobject]@{ State = 'Running'; Principal = 'Interactive'; UserId = 'X'; LastTaskResult = '0' } }
Check ($slow.Started -eq $true) 'C a late process is still a start' ("Started=" + $slow.Started)
Check ($script:polls -ge 3) 'C the wait polled until it appeared' ("polls=" + $script:polls)

# ---- D: RPC answering is the other witness (process name may differ) -----
$rpc = Confirm-NodeStarted -Seconds 0 -PollSeconds 0 `
  -ProcessProbe { $null } `
  -RpcProbe { $true } `
  -TaskProbe { [pscustomobject]@{ State = 'Running'; Principal = 'Interactive'; UserId = 'X'; LastTaskResult = '0' } }
Check ($rpc.Started -eq $true) 'D RPC answering -> started' ("Started=" + $rpc.Started)
Check ($rpc.Witness -eq 'rpc') 'D witness recorded as rpc' ("Witness=" + $rpc.Witness)

# ---- E: the start block itself fails loudly (shipped text, not a copy) ---
$i = $raw.IndexOf('The node did NOT start')
Check ($i -gt 0) 'E the start block has a failure branch' ''
if ($i -gt 0) {
  $tail = $raw.Substring($i, [Math]::Min(3000, $raw.Length - $i))
  Check ($tail -match 'exit 1') 'E the failure branch exits non-zero' ''
  Check ($tail -match 'LogonType=') 'E the failure names the principal/logon type' ''
  Check ($tail -match 'desktop session') 'E the failure names the headless cause' ''
}
Check ($raw -notmatch 'Say\s+"node started \(\$NodeTask\)"') 'E rc=0 alone no longer claims a start' ''

# ---- F: the block is reached on the real install path -------------------
Check ($raw -match 'Confirm-NodeStarted -Seconds 60') 'F the install path waits for a witness' ''

Write-Host ''
if ($script:failed -gt 0) {
  Write-Host ("FAILED: {0} of {1} checks" -f $script:failed, $script:checks) -ForegroundColor Red
  exit 1
}
Write-Host ("all {0} checks passed" -f $script:checks) -ForegroundColor Green
exit 0
