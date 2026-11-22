# #235 live probe on a real Windows box: does the deferral classifier read a
# REAL last-exit.json file (not a hashtable assembled in a test)?
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Public\gpn235'
$text = Get-Content -LiteralPath (Join-Path $root 'install.ps1') -Raw
$marker = '$UpdateWrapperTemplate' + " = @'"
$start = $text.IndexOf($marker); $start = $text.IndexOf("`n", $start) + 1
$end = $text.IndexOf("`n'@", $start)
$body = $text.Substring($start, $end - $start)
$selfTest = $body.IndexOf('# Optional ops self-test')
. ([scriptblock]::Create($body.Substring(0, $selfTest)))

$tmp = Join-Path $env:TEMP ('gpn235-live-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$now = [long]([DateTime]::UtcNow - [DateTime]'1970-01-01').TotalSeconds

# 1. a real file, written the way report-exit.ps1 writes it (the launcher's
#    synthetic record for a status that never reached the program).
$file = Join-Path $tmp 'last-exit.json'
$entry = '{"ts":"2026-11-21T19:00:00Z","ts_unix":' + ($now - 3) + ',"code":-1073741205,"kind":"session-closing","detail":"the window station or session was closing (0xC000026B STATUS_DLL_INIT_FAILED_LOGOFF)","remediation":"nothing to fix in the build","version":"launcher","os":"windows","pid":0}'
Set-Content -LiteralPath $file -Value $entry -Encoding ascii
$ev = @{ LastExit = (Get-Content -LiteralPath $file -Tail 1); Tail = @() }
$r = Get-LoaderExit $ev ($now - 60)
"LIVE A real 0xC000026B record  -> " + $(if ($r) { $r.Hex + ' ' + $r.Name + ' defer=' + $r.Defer } else { 'NOT CLASSIFIED' })

# 2. the same file, but older than this cycle -> must be ignored
$r2 = Get-LoaderExit $ev ($now + 5)
"LIVE B record older than cycle -> " + $(if ($r2) { 'CLASSIFIED (wrong)' } else { 'ignored (correct)' })

# 3. the box's REAL last-exit.json, so the reader is shown not to misfire
$real = Join-Path $env:USERPROFILE '.gpn\last-exit.json'
if (Test-Path -LiteralPath $real) {
  $realEv = @{ LastExit = (Get-Content -LiteralPath $real -Tail 1); Tail = @() }
  $r3 = Get-LoaderExit $realEv 0
  "LIVE C this box's real record -> kind='" + (($realEv.LastExit | ConvertFrom-Json).kind) + "' code=" + (($realEv.LastExit | ConvertFrom-Json).code) + ' loader=' + $(if ($r3) { $r3.Hex + ' defer=' + $r3.Defer } else { 'none (correct for a normal exit)' })
} else {
  "LIVE C no $real on this box (no node exit recorded yet)"
}

# 4. the live node is healthy right now, i.e. this box is not refusing starts
$p = Get-Process -Name gpn-node,gpn_node -ErrorAction SilentlyContinue | Select-Object -First 1
"LIVE D running node: " + $(if ($p) { $p.ProcessName + ' pid ' + $p.Id + ' ' + $p.Path } else { 'none' })
Remove-Item -LiteralPath $tmp -Recurse -Force
'LIVE RESULT: done'
