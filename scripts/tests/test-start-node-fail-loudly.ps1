#Requires -Version 5.1
<#
  test-start-node-fail-loudly.ps1 -- row 291.

  "The Windows node start path reports success when it did not start the node."

  Measured on box 1, 2026-11-27 00:35 PT: after `schtasks /End /Run /TN GPN-Node`
  the old supervisor's five provider children kept C:\gpn\node.log open, so
  `start-node.cmd --task` died on its own `>> node.log` redirect with "The process
  cannot access the file because it is being used by another process" --
  `hidden-launch.vbs` exited 0 and Task Scheduler logged LastTaskResult 0. Node
  DOWN, exit code SUCCESS, no alarm.

  Two launchers ship that worker (install.ps1's template for a fresh install,
  .github/workflows/release-windows.yml's for every deploy), and two hidden
  launchers. This test asserts the contract on all four texts, and -- on Windows
  -- executes the real thing: lock node.log, run the worker, and require exit 78
  plus a message a human can act on. The pre-fix text is the negative control
  (it must still exit 0), so the test proves the behaviour changed, not just the
  wording.

  Run locally (static half only):  pwsh -NoProfile -File scripts/tests/test-start-node-fail-loudly.ps1
  Run on Windows (both halves):    same command; the behavioural half runs when
                                   $IsWindows is true.
  Exit 0 = all checks passed.
#>

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$installer = Join-Path $root 'install.ps1'
$workflow = Join-Path $root '.github/workflows/release-windows.yml'

$script:checks = 0
$script:failed = 0
function Check([bool]$ok, [string]$name, [string]$detail) {
  $script:checks++
  if ($ok) { Write-Host ("PASS  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
  else { $script:failed++; Write-Host ("FAIL  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
}

# ---- extract the four shipped texts ---------------------------------------
function Get-HereString([string]$path, [string]$var) {
  $raw = Get-Content -LiteralPath $path -Raw
  $i = $raw.IndexOf(('$' + $var + ' = @'))
  if ($i -lt 0) { throw "$var here-string not found in $path" }
  $start = $raw.IndexOf("@'", $i) + 2
  # The terminator is a line consisting of optional spaces then '@
  $rest = $raw.Substring($start)
  $m = [regex]::Match($rest, "(?m)^[ \t]*'@[ \t]*$")
  if (-not $m.Success) { throw "$var terminator not found in $path" }
  $body = $rest.Substring(0, $m.Index)
  # Workflow here-strings are indented inside the YAML block; strip the common
  # indentation the same way the YAML block scalar does at run time.
  $lines = $body -split "`r?`n"
  $indents = $lines | Where-Object { $_.Trim() -ne '' } |
    ForEach-Object { ([regex]::Match($_, '^[ \t]*')).Length } | Sort-Object
  $min = if ($indents.Count -gt 0) { $indents[0] } else { 0 }
  if ($min -gt 0) {
    $lines = $lines | ForEach-Object { if ($_.Length -ge $min) { $_.Substring($min) } else { $_.Trim() } }
  }
  ($lines -join "`r`n").TrimEnd()
}

$installLauncher = Get-HereString $installer 'LauncherTemplate'
$installVbs      = Get-HereString $installer 'HiddenLaunchTemplate'
$wfLauncher      = Get-HereString $workflow 'script'
$wfVbs           = Get-HereString $workflow 'vbs'

Check ($installLauncher.Length -gt 1000) 'install.ps1 launcher extracted' "$($installLauncher.Length) chars"
Check ($wfLauncher.Length -gt 1000)      'release-windows.yml launcher extracted' "$($wfLauncher.Length) chars"
Check ($installVbs.Length -gt 300)       'install.ps1 hidden launcher extracted' "$($installVbs.Length) chars"
Check ($wfVbs.Length -gt 300)            'release-windows.yml hidden launcher extracted' "$($wfVbs.Length) chars"

# ---- the contract ---------------------------------------------------------
function Get-LauncherProblems([string]$text) {
  $p = New-Object System.Collections.Generic.List[string]
  # 1. A start that cannot even open its log must fail, not fall through.
  #    The probe must be a share-mode open in PowerShell: cmd does not set
  #    ERRORLEVEL when its own redirect cannot open a file (proven on box 1,
  #    2026-11-27 -- the old worker printed the error and continued to exit 0),
  #    so a `type nul >> log` probe would silently pass the locked case.
  if ($text -notmatch '\[IO\.FileShare\]::None') { $p.Add('no share-mode log probe ([IO.FileShare]::None)') }
  if ($text -notmatch 'goto :log_locked')  { $p.Add('no `goto :log_locked` after the probe') }
  if ($text -notmatch '(?m)^:log_locked') { $p.Add('no :log_locked label') }
  if ($text -notmatch 'exit /b 78')       { $p.Add('the locked-log path does not exit 78') }
  # 2. The message must name the cause and one command that fixes it.
  if ($text -notmatch '--stop')           { $p.Add('the locked-log message does not name `gpn_node --stop`') }
  if ($text -notmatch 'holds it|holding it|cannot be opened') {
    $p.Add('the locked-log message does not name the cause (another process holds the log)')
  }
  # 3. A worker that exits with no live supervisor must report failure.
  if ($text -notmatch 'IMAGENAME eq gpn_node\.exe') { $p.Add('no liveness check before exit') }
  if ($text -notmatch 'exit /b %(RC|FINALRC)%')     { $p.Add('does not propagate an exit code at the end') }
  if ($text -notmatch '79')               { $p.Add('a silent supervisor exit is not turned into a failure (79)') }
  return $p
}

function Get-VbsProblems([string]$text) {
  $p = New-Object System.Collections.Generic.List[string]
  if ($text -notmatch 'rc\s*=\s*sh\.Run\(') { $p.Add('does not capture sh.Run''s return value') }
  if ($text -notmatch 'WScript\.Quit\s+rc') { $p.Add('does not propagate the worker''s exit code (WScript.Quit rc)') }
  if ($text -match '(?m)^\s*sh\.Run worker') { $p.Add('still calls sh.Run without capturing the code') }
  if ($text -notmatch 'ExecutablePath')      { $p.Add('single-instance guard is by name, not by install path') }
  return $p
}

foreach ($pair in @(
    @{ Name = 'install.ps1 launcher';        Text = $installLauncher; Kind = 'launcher' },
    @{ Name = 'release-windows.yml launcher'; Text = $wfLauncher;      Kind = 'launcher' })) {
  $problems = Get-LauncherProblems $pair.Text
  Check ($problems.Count -eq 0) "contract: $($pair.Name)" ($problems -join '; ')
}
foreach ($pair in @(
    @{ Name = 'install.ps1 hidden launcher';        Text = $installVbs },
    @{ Name = 'release-windows.yml hidden launcher'; Text = $wfVbs })) {
  $problems = Get-VbsProblems $pair.Text
  Check ($problems.Count -eq 0) "contract: $($pair.Name)" ($problems -join '; ')
}

# ---- negative control: the pre-fix text must be REJECTED -------------------
$legacyLauncher = @'
@echo off
:gpn_worker
set "GPN_BIN=C:\gpn\bin"
"%GPN_BIN%\gpn_node.exe" >> "C:\gpn\node.log" 2>&1
'@
$legacyProblems = Get-LauncherProblems $legacyLauncher
Check ($legacyProblems.Count -ge 4) 'negative control: the pre-fix launcher is rejected' \
  ("{0} problems found" -f $legacyProblems.Count)
$legacyVbs = @'
Option Explicit
sh.Run worker, 0, True
'@
Check ((Get-VbsProblems $legacyVbs).Count -ge 2) 'negative control: the pre-fix vbs is rejected'

# ---- behavioural half (Windows only) --------------------------------------
$onWindows = ($PSVersionTable.PSVersion.Major -ge 6 -and $IsWindows) -or ($env:OS -eq 'Windows_NT')
if (-not $onWindows) {
  Write-Host 'SKIP  behavioural half: not Windows (run this on box 1 for the real proof)'
} else {
  $tmp = Join-Path $env:TEMP ("gpn-291-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
  New-Item -ItemType Directory -Path $tmp -Force | Out-Null
  $log = Join-Path $tmp 'node.log'
  $report = Join-Path $tmp 'report-exit.ps1'
  Set-Content -LiteralPath $report -Value '# stub' -Encoding ascii
  $cmd = Join-Path $tmp 'start-node.cmd'
  $text = $installLauncher
  $text = $text.Replace('__INSTALL_DIR__', $tmp)
  $text = $text.Replace('__KEYFILE__', (Join-Path $tmp 'node-key'))
  $text = $text.Replace('__AP_URL__', 'http://127.0.0.1:1')
  $text = $text.Replace('__BOOTNODES__', '')
  $text = $text.Replace('__LOG__', $log)
  $text = $text.Replace('__REPORT__', $report)
  Set-Content -LiteralPath $cmd -Value $text -Encoding ascii

  # A native command that writes to stderr under $ErrorActionPreference='Stop'
  # turns into a terminating NativeCommandError, so capture through files and
  # read the codes explicitly. (cmd reports an unopenable batch redirect itself,
  # which is exactly the noise this test must survive.)
  function Invoke-Capture([string]$commandLine, [string]$tag) {
    $o = Join-Path $tmp "$tag.out"; $e = Join-Path $tmp "$tag.err"
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
      $null = & cmd /c "$commandLine > `"$o`" 2> `"$e`""
      $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $prev }
    $text = ''
    foreach ($f in @($o, $e)) { if (Test-Path -LiteralPath $f) { $text += (Get-Content -LiteralPath $f -Raw) } }
    if ($env:GPN_291_DEBUG) { Write-Host "DEBUG [$tag] cmd=[$commandLine] rc=$code text=[$text]" }
    [pscustomobject]@{ Code = $code; Text = $text }
  }

  # Hold the log open exclusively -- exactly what an orphaned provider does.
  $fs = [IO.File]::Open($log, [IO.FileMode]::Append, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try {
    $r = Invoke-Capture "`"$cmd`" --task" 'worker'
    Check ($r.Code -eq 78) 'behaviour: a locked node.log fails the worker with 78' "rc=$($r.Code)"
    Check ($r.Text -match 'cannot be opened') 'behaviour: the message names the log' (($r.Text -split "`n" | Where-Object { $_ -match 'cannot be opened' } | Select-Object -First 1))
    Check ($r.Text -match '--stop') 'behaviour: the message names the fix (gpn_node --stop)'
    Check ($r.Text -match 'did NOT start') 'behaviour: it says the node did not start'

    # And the VBS must carry that code to Task Scheduler.
    $vbsPath = Join-Path $tmp 'hidden-launch.vbs'
    Set-Content -LiteralPath $vbsPath -Value $installVbs -Encoding ascii
    # The guard looks for a supervisor under THIS directory; production runs in
    # C:\gpn, so a temp dir never matches it.
    $v = Invoke-Capture "cscript //nologo `"$vbsPath`"" 'vbs'
    Check ($v.Code -eq 78) 'behaviour: hidden-launch.vbs propagates the failure (78)' "rc=$($v.Code)"
  } finally {
    $fs.Dispose()
  }

  # Negative control: the SAME locked log, through the pre-fix pair. The worker
  # itself already failed (cmd reports it on stderr), but the old hidden launcher
  # threw the code away, so Task Scheduler saw success while the node was down --
  # the exact defect (box 1, 2026-11-27 00:35 PT, LastTaskResult 0).
  $legacyDir = Join-Path $tmp 'legacy'
  New-Item -ItemType Directory -Path $legacyDir -Force | Out-Null
  $legacyCmd = Join-Path $legacyDir 'start-node.cmd'
  Set-Content -LiteralPath $legacyCmd -Value (@'
@echo off
if /I "%~1"=="--task" goto :gpn_worker
exit /b 0
:gpn_worker
setlocal
set "GPN_BIN=__BIN__"
"__BIN__\gpn_node.exe" >> "__LOG__" 2>&1
'@.Replace('__BIN__', (Join-Path $legacyDir 'bin')).Replace('__LOG__', $log)) -Encoding ascii
  $legacyVbs = Join-Path $legacyDir 'hidden-launch.vbs'
  Set-Content -LiteralPath $legacyVbs -Value (@'
Option Explicit
Dim fso, sh, q, here, worker, wmi, procs
Set fso = CreateObject("Scripting.FileSystemObject")
Set sh  = CreateObject("WScript.Shell")
q = Chr(34)
here = fso.GetParentFolderName(WScript.ScriptFullName)
Set wmi = GetObject("winmgmts:\.\root\cimv2")
Set procs = wmi.ExecQuery("SELECT ProcessId FROM Win32_Process WHERE Name=''gpn_node.exe''")
If procs.Count > 0 Then WScript.Quit 0
worker = "cmd /c " & q & q & here & "\start-node.cmd" & q & " --task" & q
sh.Run worker, 0, True
'@) -Encoding ascii
  $fs2 = [IO.File]::Open($log, [IO.FileMode]::Append, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try {
    $l = Invoke-Capture "`"$legacyCmd`" --task" 'legacy-worker'
    $legacyRc = $l.Code
    $lv = Invoke-Capture "cscript //nologo `"$legacyVbs`"" 'legacy-vbs'
    $legacyVbsRc = $lv.Code
  } finally { $fs2.Dispose() }
  Check ($legacyRc -ne 0) 'control: the pre-fix worker still fails on its own redirect' "rc=$legacyRc"
  Check ($legacyVbsRc -eq 0) 'control: the pre-fix hidden launcher reports SUCCESS for that failure' "LastTaskResult would be $legacyVbsRc"
}

Write-Host ''
Write-Host ("RESULT: {0} checks, {1} failed" -f $script:checks, $script:failed)
if ($script:failed -gt 0) { exit 1 }
exit 0
