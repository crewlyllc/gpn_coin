# Behavioural tests for #238: the GPN-Update task must run at the same integrity
# level as GPN-Node, or it cannot stop the node it is replacing.
#
# The field failure (box 1, 2026-11-22 03:15 PT): GPN-Update was RunLevel=Limited
# (registered 2026-11-01 by an older installer) while GPN-Node ran at Highest.
# Stop-NodeTree answered "Access is denied - it may be elevated", the wrapper
# refused to apply, and the box stayed on 0.3.66 forever while every check step
# succeeded. The XML has said HighestAvailable since 2026-11-17 -- the LIVE task
# is the only proof, which is what Assert-UpdateTaskElevation asks for.
#
# Covered:
#   A  live task at Highest                          -> pass, no re-registration
#   B  live task at Limited, re-register fixes it     -> pass, exactly one re-register
#   C  live task at Limited twice                     -> fail, warns it will not update
#   D  RunLevel unreadable (no ScheduledTasks module) -> fail with a clear warning
#   E  the update task XML carries HighestAvailable   -> the shipped config agrees
#   F  node registration asks for /RL HIGHEST         -> the two levels are meant to match
#   G  both installer paths call the verification     -> fresh install and deploy-only
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
$installer = Join-Path $root 'install.ps1'
$raw = Get-Content -LiteralPath $installer -Raw

$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$errors)
if ($errors -and $errors.Count -gt 0) { throw ('install.ps1 does not parse: ' + ($errors[0].Message)) }

$script:says = @()
$script:warns = @()
function Say($m) { $script:says += [string]$m }
function Warn($m) { $script:warns += [string]$m }
$UpdateTask = 'GPN-Update'
$NodeTask = 'GPN-Node'

$wanted = @('Get-TaskRunLevel', 'Assert-UpdateTaskElevation')
$found = @{}
foreach ($f in $ast.FindAll({ $args[0] -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true)) {
  if ($wanted -contains $f.Name) { $found[$f.Name] = $true; . ([scriptblock]::Create($f.Extent.Text)) }
}
foreach ($n in $wanted) { if (-not $found.ContainsKey($n)) { throw "could not find function $n in $installer" } }

$script:fail = 0
function Ok($name, $cond, $detail) {
  if ($cond) { Write-Host ("ok   " + $name) }
  else { Write-Host ("FAIL " + $name + "  " + $detail); $script:fail++ }
}
function Reset-Log { $script:says = @(); $script:warns = @() }

# ---- A: already correct, nothing to do ------------------------------------
Reset-Log
$calls = 0
$r = Assert-UpdateTaskElevation -RunLevelProbe { param($n) 'Highest' } -Reregister { $script:calls++ }
Ok 'A a Highest task passes' ($r -eq $true) ("r=" + $r)
Ok 'A nothing is re-registered' ($script:calls -eq 0) ("calls=" + $script:calls)
Ok 'A the pass is reported' (@($script:says).Count -gt 0 -and (@($script:says) -join ' ') -like '*RunLevel=Highest*') ("says=" + (@($script:says) -join '|'))

# ---- B: stale Limited task, one re-register fixes it ----------------------
Reset-Log
$script:calls = 0
$script:probeN = 0
$r = Assert-UpdateTaskElevation -RunLevelProbe {
  param($n)
  if ($n -eq 'GPN-Node') { return 'Highest' }
  $script:probeN++
  if ($script:probeN -eq 1) { return 'Limited' } else { return 'Highest' }
} -Reregister { $script:calls++ }
Ok 'B a Limited task is replaced and then passes' ($r -eq $true) ("r=" + $r)
Ok 'B exactly one re-registration' ($script:calls -eq 1) ("calls=" + $script:calls)
Ok 'B the mismatch is named' ((@($script:warns) -join ' ') -like '*RunLevel=Limited*') ("warns=" + (@($script:warns) -join '|'))

# ---- C: still Limited after re-register -> fail, loudly -------------------
Reset-Log
$script:calls = 0
$r = Assert-UpdateTaskElevation -RunLevelProbe { param($n) 'Limited' } -Reregister { $script:calls++ }
Ok 'C a task that stays Limited fails' ($r -eq $false) ("r=" + $r)
Ok 'C it re-registered once before giving up' ($script:calls -eq 1) ("calls=" + $script:calls)
Ok 'C it says the box will not auto-update' ((@($script:warns) -join ' ') -like '*will not auto-update*') ("warns=" + (@($script:warns) -join '|'))

# ---- D: cannot read the answer -> never a silent pass ---------------------
Reset-Log
$script:calls = 0
$r = Assert-UpdateTaskElevation -RunLevelProbe { param($n) '' } -Reregister { $script:calls++ }
Ok 'D an unreadable RunLevel fails' ($r -eq $false) ("r=" + $r)
Ok 'D it does not re-register on an unreadable answer' ($script:calls -eq 0) ("calls=" + $script:calls)
Ok 'D it says what could not be verified' ((@($script:warns) -join ' ') -like '*could not read*') ("warns=" + (@($script:warns) -join '|'))

# ---- E/F: the shipped config agrees --------------------------------------
Ok 'E the update task XML carries HighestAvailable' ($raw.Contains('<RunLevel>HighestAvailable</RunLevel>')) 'XML changed'
Ok 'E the XML comment names the Access-is-denied failure' ($raw.Contains('could not be terminated. Reason: Access is denied')) 'rationale missing'
Ok 'F node registration asks for /RL HIGHEST' ($raw.Contains("@('/RL', 'HIGHEST')")) 'node RunLevel changed'

# ---- G: both installer paths verify --------------------------------------
$v1 = $raw.IndexOf("`$ok = Register-UpdateTask`r`n", [StringComparison]::Ordinal)
$installPath = $raw.IndexOf('if ($UpdateTaskOnly) {')
$deployVerify = $raw.IndexOf('[void](Assert-UpdateTaskElevation)', $installPath)
$freshVerify = $raw.LastIndexOf('[void](Assert-UpdateTaskElevation)')
Ok 'G the deploy-only path verifies the task level' ($deployVerify -gt $installPath) 'deploy path missing verification'
Ok 'G the fresh-install path verifies the task level' ($freshVerify -gt $deployVerify) 'install path missing verification'

# ---- H: the rendered task XML is well-formed XML --------------------------
# Box 1, 2026-11-22: a "--" inside the XML comment made schtasks /Create /XML
# answer rc=1 on every box, so GPN-Update was never re-registered at Highest and
# the box fell back to the Startup loop. A string check cannot see that; parsing
# the rendered document can.
$xmlStart = $raw.IndexOf("`$UpdateTaskXml = @'")
$xmlEnd = $raw.IndexOf("'@", $xmlStart)
$xmlRender = ''
if ($xmlStart -ge 0 -and $xmlEnd -gt $xmlStart) {
  $xmlRender = $raw.Substring($xmlStart + "`$UpdateTaskXml = @'".Length, $xmlEnd - $xmlStart - "`$UpdateTaskXml = @'".Length)
  $xmlRender = $xmlRender.TrimStart().Replace('__SCRIPT__', 'C:\gpn\update-and-restart.ps1').Replace('__USER__', 'BOX\user')
}
$parseErr = ''
try {
  $doc = New-Object System.Xml.XmlDocument
  $doc.LoadXml($xmlRender)
} catch { $parseErr = $_.Exception.Message }
Ok 'H the rendered update-task XML is well-formed' ($parseErr -eq '') ("parse error: " + $parseErr)
$lvl = ''
if ($parseErr -eq '') {
  $lvl = [string]$doc.SelectSingleNode("/*[local-name()='Task']/*[local-name()='Principals']/*[local-name()='Principal']/*[local-name()='RunLevel']").InnerText
}
Ok 'H the parsed principal asks for HighestAvailable' ($lvl -eq 'HighestAvailable') ("RunLevel='" + $lvl + "'")
Ok 'H the installer validates the XML before registering' ($raw.Contains('$doc.LoadXml($xmlText)')) 'parse guard missing'

# ---- the extracted code stays pure ASCII (Windows PowerShell 5.1) ---------
$nonAscii = 0
$fnStart = $raw.IndexOf('function Assert-UpdateTaskElevation')
$fnEnd = $raw.IndexOf('# -- Update fallback', $fnStart)
$body = $raw.Substring($fnStart, $fnEnd - $fnStart)
foreach ($ch in $body.ToCharArray()) { if ([int]$ch -gt 127) { $nonAscii++ } }
Ok 'ascii the elevation code is pure ASCII' ($nonAscii -eq 0) ("$nonAscii non-ASCII chars")

if ($script:fail -gt 0) {
  Write-Host ("FAILURES: " + $script:fail)
  exit 1
}
Write-Host 'RESULT: PASS (all checks)'
exit 0
