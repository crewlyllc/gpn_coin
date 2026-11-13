# deploy-windows-binaries.ps1 - stop the node, replace C:\gpn\bin, never leave the box dead.
#
# Why this is a file and not inline (2026-11-12): the release-windows workflow step that
# does this reached 22,234 characters and GitHub refused to parse the workflow at all
# ("Exceeded max expression length 21000"). The step's `run:` block counts against that
# limit, so every future edit to this logic was going to hit it again.
#
# Why the retry loop exists (same day, 0.3.43): the deploy failed with robocopy rc=11
# ("some files could not be copied") after robocopy's WHOLE 10x3s retry window on ~6
# files - 183 s - so the lock holder was not a 3-second transient. Two things a single
# long wait cannot handle:
#   * a process that comes back after the stop sweep (so the stop is repeated between
#     bounded copy attempts, not done once);
#   * Stop-Process -Force returns before the OS unmaps the image, and a node killed
#     mid-flush keeps its .exe mapped - hence Wait-Process before copying.
# And a failed copy must not leave the box with no node at all: the step used to throw
# before the restart block ran, so C:\gpn stayed dead until someone noticed.
param(
  [Parameter(Mandatory = $true)][string]$AgentDir,
  [Parameter(Mandatory = $true)][string]$NodeExe,
  [Parameter(Mandatory = $true)][string]$Dest,
  [int]$Attempts = 4,
  # 1/0, not [switch]: `powershell -File` passes every argument as a STRING, and a
  # SwitchParameter refuses "True" ("accept only Boolean values and numbers" - the
  # 2026-11-12 failure). Numbers convert cleanly.
  [int]$RestartOnFailure = 0,
  [string]$StartScript = 'C:\gpn\start-node.cmd'
)

$ErrorActionPreference = 'Continue'   # native schtasks writes to stderr; exit codes are checked

function Stop-GpnProcesses {
  foreach ($tn in @('GPN-Node', 'GPNProvider')) { schtasks /End /TN $tn 2>&1 | Out-Null }
  Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" |
    Where-Object { $_.CommandLine -like '*run-provider.ps1*' } |
    ForEach-Object {
      Write-Host "  stopping supervisor pid=$($_.ProcessId)"
      Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
    }
  $pids = @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -like 'gpn*' } | ForEach-Object {
      Write-Host "  stopping $($_.ProcessName) (pid $($_.Id))"
      Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
      $_.Id
    })
  foreach ($p in $pids) { Wait-Process -Id $p -Timeout 30 -ErrorAction SilentlyContinue }
  Start-Sleep -Seconds 3
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Stop-GpnProcesses
# belt-and-braces: the legacy task is retired by its own workflow step, but it has been
# recreated between steps before, and two supervisors fight over :8094 + the chain DB.
schtasks /Query /TN GPNProvider 2>&1 | Out-Null
if ($LASTEXITCODE -eq 0) {
  schtasks /Delete /TN GPNProvider /F 2>&1 | Out-Null
  Write-Host 'removed legacy GPNProvider task'
}

$log = Join-Path $env:TEMP 'gpn-robocopy.log'
Remove-Item $log -ErrorAction SilentlyContinue
$copied = $false
$rc = 0
$attempt = 0
for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
  # file names are kept in the log (no /NFL) so a failure can name what it could not replace
  robocopy $AgentDir $Dest '*.exe' /R:10 /W:3 /NDL /NJH /NJS /NP /LOG+:$log | Out-Null
  $rc = $LASTEXITCODE
  if ($rc -lt 8) { $copied = $true; break }
  Write-Host "::warning::robocopy attempt $attempt/$Attempts failed (rc=$rc) - re-stopping gpn*"
  Get-Content $log -ErrorAction SilentlyContinue | Select-String 'ERROR' | Select-Object -Last 6 |
    ForEach-Object { Write-Host "  robocopy: $($_.Line.Trim())" }
  if ($attempt -lt $Attempts) { Stop-GpnProcesses }
}

if (-not $copied) {
  Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -like 'gpn*' } |
    ForEach-Object { Write-Host "  still running: $($_.ProcessName) pid=$($_.Id)" }
  if ($RestartOnFailure -ne 0) {
    Write-Host '::warning::restarting GPN-Node on the previous binaries so the box is not left dead'
    schtasks /Create /TN 'GPN-Node' /TR "cmd /c $StartScript" /SC ONLOGON /RL HIGHEST /F 2>&1 | Out-Null
    schtasks /Run /TN 'GPN-Node' 2>&1 | Out-Null
  }
  Write-Host "::error::robocopy of agent binaries failed after $Attempts stop+copy cycles (last rc=$rc) - see $log"
  exit 1
}

Copy-Item $NodeExe $Dest -Force
Write-Host "deployed to $Dest (robocopy rc=$rc, attempt $attempt):"
Get-ChildItem $Dest -Filter *.exe | ForEach-Object { Write-Host "  $($_.Name)  $([math]::Round($_.Length/1MB,1))MB" }
exit 0
