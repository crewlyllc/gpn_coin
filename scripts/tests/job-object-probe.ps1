#Requires -Version 5.1
<#
  job-object-probe.ps1 -- row 291, the "no orphaned provider children" half.

  What it proves: a process that creates a job object with
  JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, puts ITSELF in it, spawns a child with a
  plain CreateProcess and then dies without running any shutdown code takes that
  child with it. That is the sequence `attach_kill_on_close_job()` performs in
  gpn-agent/src/bin/gpn_node.rs, and the case that failed on box 1 on
  2026-11-27 00:35 PT: `schtasks /End` killed the supervisor, the five provider
  children survived, held C:\gpn\node.log open, the next start died on its
  redirect, and Task Scheduler logged LastTaskResult 0.

  Two ways to run it:
    -NodeExe <path>   use the shipped binary's `--job-selftest` (the real thing;
                      available once the release that contains it is installed)
    (no argument)     compile the reference implementation embedded below with
                      the framework's csc.exe and run the same experiment

  A PowerShell-only version of this probe was tried first and produced a FALSE
  NEGATIVE: the same three P/Invoke calls from PowerShell left the child alive.
  The compiled reference (and, later, the binary itself) is what the claim rests
  on, so the probe compiles rather than trusts.

  Run: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/tests/job-object-probe.ps1 [-NodeExe C:\path\gpn_node.exe]
  Exit 0 = the guarantee holds on this machine.
#>

param([string]$NodeExe)

$ErrorActionPreference = 'Stop'
$script:failed = 0
function Check([bool]$ok, [string]$name, [string]$detail) {
  if ($ok) { Write-Host ("PASS  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
  else { $script:failed++; Write-Host ("FAIL  {0}{1}" -f $name, $(if ($detail) { " -- $detail" } else { '' })) }
}
function Alive([int]$id) { $null -ne (Get-Process -Id $id -ErrorAction SilentlyContinue) }

$tmp = Join-Path $env:TEMP ("gpn-291-job-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
$childFile = Join-Path $tmp 'child.txt'

$reference = @'
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;

// Reference implementation of attach_kill_on_close_job() in gpn_node.rs:
// CreateJobObjectW -> SetInformationJobObject(KILL_ON_JOB_CLOSE) ->
// AssignProcessToJobObject(GetCurrentProcess()) -> spawn a child -> die.
class GpnJobTest {
  [StructLayout(LayoutKind.Sequential)]
  struct IO_COUNTERS { public ulong a,b,c,d,e,f; }
  [StructLayout(LayoutKind.Sequential)]
  struct BASIC {
    public long PerProcessUserTimeLimit; public long PerJobUserTimeLimit; public uint LimitFlags;
    public UIntPtr MinimumWorkingSetSize; public UIntPtr MaximumWorkingSetSize; public uint ActiveProcessLimit;
    public UIntPtr Affinity; public uint PriorityClass; public uint SchedulingClass; }
  [StructLayout(LayoutKind.Sequential)]
  struct EXT {
    public BASIC BasicLimitInformation; public IO_COUNTERS IoInfo;
    public UIntPtr ProcessMemoryLimit; public UIntPtr JobMemoryLimit; public UIntPtr PeakProcessMemoryUsed; public UIntPtr PeakJobMemoryUsed; }
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern IntPtr CreateJobObject(IntPtr a, string n);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetInformationJobObject(IntPtr j, int c, ref EXT i, uint l);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool AssignProcessToJobObject(IntPtr j, IntPtr p);
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();

  static int Main(string[] args) {
    IntPtr job = CreateJobObject(IntPtr.Zero, null);
    if (job == IntPtr.Zero) { Console.WriteLine("CreateJobObject failed " + Marshal.GetLastWin32Error()); return 1; }
    EXT info = new EXT();
    info.BasicLimitInformation.LimitFlags = 0x2000;   // KILL_ON_JOB_CLOSE
    if (!SetInformationJobObject(job, 9, ref info, (uint)Marshal.SizeOf(info))) { Console.WriteLine("SetInformationJobObject failed " + Marshal.GetLastWin32Error()); return 1; }
    if (!AssignProcessToJobObject(job, GetCurrentProcess())) { Console.WriteLine("AssignProcessToJobObject failed " + Marshal.GetLastWin32Error()); return 1; }
    ProcessStartInfo psi = new ProcessStartInfo("cmd.exe", "/c ping -n 120 127.0.0.1");
    psi.UseShellExecute = false;
    psi.CreateNoWindow = true;
    Process child = Process.Start(psi);
    File.WriteAllText(args[0], child.Id.ToString());
    Console.WriteLine("child " + child.Id);
    if (args.Length > 1 && args[1] == "stay") { System.Threading.Thread.Sleep(300000); }
    return 0;   // exit with no cleanup: the OS must do the killing
  }
}
'@

$runner = ''
$label = ''


# -- Mode A: the shipped binary's own self-test ------------------------------
if ($NodeExe) {
  Check (Test-Path -LiteralPath $NodeExe) 'binary exists' $NodeExe
  $prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
  $out = & $NodeExe --job-selftest 2>&1 | Out-String
  $ErrorActionPreference = $prev
  $code = $LASTEXITCODE
  Check ($code -eq 0) 'gpn_node --job-selftest exited 0' "rc=$code"
  Check ($out -match 'kill-on-close job attached') 'the supervisor attached the kill-on-close job' (($out -split "`n" | Select-Object -First 1))
  if ($out -match 'child pid (\d+)') { $childId = [int]$Matches[1] } else { $childId = 0 }
  Check ($childId -gt 0) 'the self-test reported its child pid' "child pid=$childId"
  if ($childId -gt 0) {
    # The self-test exits immediately, so the child must already be dying.
    $childGone = $false
    for ($i = 0; $i -lt 40; $i++) { if (-not (Alive $childId)) { $childGone = $true; break }; Start-Sleep -Milliseconds 250 }
    Check $childGone 'the child died when the supervisor exited without cleanup' "child pid=$childId"
    if (-not $childGone) { Stop-Process -Id $childId -Force -ErrorAction SilentlyContinue }
  }
  Write-Host ''
  Write-Host ("RESULT: {0} failed" -f $script:failed)
  if ($script:failed -gt 0) { exit 1 }
  exit 0
}

# -- Mode B: the compiled reference, held alive then hard-killed -------------
$csc = @(
  'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe',
  'C:\Windows\Microsoft.NET\Framework\v4.0.30319\csc.exe'
) | Where-Object { Test-Path $_ } | Select-Object -First 1
Check ($null -ne $csc) 'csc.exe found (reference implementation)' $(if ($csc) { $csc } else { 'none' })
if (-not $csc) { Write-Host "RESULT: $script:failed failed"; exit 1 }
$src = Join-Path $tmp 'GpnJobTest.cs'
$runner = Join-Path $tmp 'GpnJobTest.exe'
Set-Content -LiteralPath $src -Value $reference -Encoding ascii
$out = & $csc /nologo "/out:$runner" $src 2>&1
Check ($LASTEXITCODE -eq 0) 'reference implementation compiled' (($out | Select-Object -Last 2) -join ' ')
if ($LASTEXITCODE -ne 0) { Write-Host "RESULT: $script:failed failed"; exit 1 }

$proc = Start-Process $runner -ArgumentList @("`"$childFile`"", 'stay') -PassThru -WindowStyle Hidden
Check ($null -ne $proc) 'reference supervisor started' ("pid=$($proc.Id)")

$childId = 0
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Milliseconds 500
  if (Test-Path -LiteralPath $childFile) {
    $t = (Get-Content -LiteralPath $childFile -Raw).Trim()
    if ($t -match '^\d+$') { $childId = [int]$t; break }
  }
  if (-not (Alive $proc.Id)) { break }
}
Check ($childId -gt 0) 'the reference supervisor reported its child pid' ("child pid=$childId")
if ($childId -le 0) { Write-Host "RESULT: $script:failed failed"; exit 1 }
Check (Alive $childId) 'the child is running before the kill'

# Hard kill: no shutdown code in the supervisor runs -- exactly schtasks /End,
# Task Manager, or an OOM kill.
if (Alive $proc.Id) { Stop-Process -Id $proc.Id -Force }
$gone = $false
for ($i = 0; $i -lt 20; $i++) { Start-Sleep -Milliseconds 250; if (-not (Alive $proc.Id)) { $gone = $true; break } }
Check $gone 'the supervisor died without running any shutdown code'

$childGone = $false
for ($i = 0; $i -lt 60; $i++) { Start-Sleep -Milliseconds 250; if (-not (Alive $childId)) { $childGone = $true; break } }
Check $childGone 'the child died WITH the supervisor (the guarantee row 291 needs)' ("child pid=$childId")

if (-not $childGone) { Stop-Process -Id $childId -Force -ErrorAction SilentlyContinue }
Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue

Write-Host ''
Write-Host ("RESULT: {0} failed" -f $script:failed)
if ($script:failed -gt 0) { exit 1 }
exit 0
