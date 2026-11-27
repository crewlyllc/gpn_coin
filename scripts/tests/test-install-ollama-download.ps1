# test-install-ollama-download.ps1 -- row 289
#
# The ollama installer download in scripts/install-ollama.ps1 used
# `Invoke-WebRequest -OutFile`, which on Windows PowerShell 5.1 buffers the body
# in memory: on the 3 GB GPN-210 guest the 1.9 GB file sat at 0 bytes for 18 min
# with ~300 s CPU (2026-11-27), and the failure message afterwards blamed ollama
# rather than the download.
#
# This test drives the real script (no mocks) twice:
#   A. a URL that answers with bytes -> the file lands on disk, byte-exact, and
#      the download is reported;
#   B. a URL that cannot be reached -> the script retries exactly the stated
#      number of times, then exits 1 naming the URL and the reason, and never
#      claims anything about ollama.
#
# Usage: pwsh -NoProfile -File scripts/tests/test-install-ollama-download.ps1
#        (also runs under Windows PowerShell 5.1)

$ErrorActionPreference = 'Stop'
$here = (Resolve-Path -LiteralPath $PSCommandPath).Path
$repo = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $here))
$target = Join-Path $repo 'scripts/install-ollama.ps1'
$pass = 0; $fail = 0

function Ok($m)   { $script:pass++; Write-Host "ok $m" -ForegroundColor Green }
function Bad($m)  { $script:fail++; Write-Host "FAIL $m" -ForegroundColor Red }
function Check($cond, $m) { if ($cond) { Ok $m } else { Bad $m } }

if (-not (Test-Path $target)) { Bad "script missing: $target"; exit 1 }

$psExe = if ($PSVersionTable.PSEdition -eq 'Desktop') { 'powershell' } else { (Get-Process -Id $PID).Path }

# ---- a local HTTP server with a known payload ---------------------------------
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("gpn289-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $work | Out-Null
$payload = Join-Path $work 'OllamaSetup.exe'
$bytes = New-Object byte[] (3 * 1024 * 1024)
(New-Object System.Random 289).NextBytes($bytes)
[System.IO.File]::WriteAllBytes($payload, $bytes)
$sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $payload).Hash

# A minimal HTTP server over a raw TcpListener: HttpListener needs a URL ACL on
# Windows and a Start-Job child process cannot share the listener object, and this
# test has to run on both pwsh 7 and Windows PowerShell 5.1.
$port = 38917
$server = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, $port)
try { $server.Start() } catch { Bad "could not listen on 127.0.0.1:$port -- $($_.Exception.Message)"; exit 1 }
$serverPs = [PowerShell]::Create()
$serverPs.AddScript({
  param($srv, $bytes)
  while ($true) {
    try { $client = $srv.AcceptTcpClient() } catch { break }
    try {
      $stream = $client.GetStream()
      $buf = New-Object byte[] 4096
      $sb = New-Object System.Text.StringBuilder
      while ($true) {
        $n = $stream.Read($buf, 0, $buf.Length)
        if ($n -le 0) { break }
        [void]$sb.Append([System.Text.Encoding]::ASCII.GetString($buf, 0, $n))
        if ($sb.ToString() -match "\r\n\r\n") { break }
      }
      $head = "HTTP/1.1 200 OK`r`nContent-Length: $($bytes.Length)`r`nContent-Type: application/octet-stream`r`nConnection: close`r`n`r`n"
      $hb = [System.Text.Encoding]::ASCII.GetBytes($head)
      $stream.Write($hb, 0, $hb.Length)
      $stream.Write($bytes, 0, $bytes.Length)
      $stream.Flush()
      $client.Close()
    } catch { try { $client.Close() } catch { } }
  }
}) | Out-Null
[void]$serverPs.AddArgument($server)
[void]$serverPs.AddArgument($bytes)
$serverHandle = $serverPs.BeginInvoke()

function Stop-Server {
  try { $server.Stop() } catch { }
  try { $serverPs.Stop() } catch { }
  try { $serverPs.Dispose() } catch { }
}
Start-Sleep -Milliseconds 400

# ---- A. the good path: bytes land, byte-exact ---------------------------------
$outA = Join-Path $work 'out-a.txt'
$dest = Join-Path $work 'downloaded\OllamaSetup.exe'
$env:GPN_OLLAMA_SETUP_URL = "http://127.0.0.1:$port/OllamaSetup.exe"
$env:GPN_OLLAMA_SETUP_PATH = $dest
$env:GPN_OLLAMA_DOWNLOAD_ATTEMPTS = '1'
$env:OLLAMA_URL = 'http://127.0.0.1:9'      # never listening: keeps step 2 fast
& $psExe -NoProfile -ExecutionPolicy Bypass -File $target *> $outA
$rcA = $LASTEXITCODE
$logA = (Get-Content -LiteralPath $outA -Raw)

Check (Test-Path $dest) "A1 the download created its destination file"
if (Test-Path $dest) {
  $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $dest).Hash
  Check ($got -eq $sha) "A2 the downloaded bytes are identical (sha256 $($sha.Substring(0,12))...)"
}
Check ($logA -match 'downloaded 3 MiB from http://127\.0\.0\.1') "A3 the log reports the streaming download and its URL"
Check ($logA -notmatch 'Invoke-WebRequest') "A4 no Invoke-WebRequest path is exercised any more"
Check ($rcA -ne 0) "A5 the run still exits non-zero because ollama is genuinely absent here (rc=$rcA)"
Check ($logA -match 'still not on PATH') "A6 and it says that, rather than claiming a download failure"

# ---- B. the dead path: bounded retries, the URL named, no blaming ollama ------
$outB = Join-Path $work 'out-b.txt'
$dead = 'http://127.0.0.1:1/OllamaSetup.exe'
$env:GPN_OLLAMA_SETUP_URL = $dead
$env:GPN_OLLAMA_SETUP_PATH = (Join-Path $work 'dead\OllamaSetup.exe')
$env:GPN_OLLAMA_DOWNLOAD_ATTEMPTS = '2'
$sw = [System.Diagnostics.Stopwatch]::StartNew()
& $psExe -NoProfile -ExecutionPolicy Bypass -File $target *> $outB
$rcB = $LASTEXITCODE
$sw.Stop()
$logB = (Get-Content -LiteralPath $outB -Raw)

Check ($rcB -eq 1) "B1 a failed download exits 1 (rc=$rcB)"
Check ($logB -match [regex]::Escape($dead)) "B2 the failure names the URL that failed"
Check ($logB -match 'attempt 1/2 failed') "B3 it reports each bounded attempt"
Check ($logB -match 'download FAILED after 2 attempt\(s\)') "B4 it gives up after exactly the stated attempt count"
Check ($logB -match 'download failure, NOT an ollama problem') "B5 it distinguishes a download failure from an ollama problem"
Check ($logB -match 'rerun install\.ps1') "B6 it tells the stranger what to do next"
Check ($logB -notmatch 'ollama did not answer') "B7 it does not blame the ollama service for a download that never happened"
Check (-not (Test-Path (Join-Path $work 'dead\OllamaSetup.exe'))) "B8 a failed download leaves no partial file behind"
Check ($sw.Elapsed.TotalSeconds -lt 60) "B9 the dead path is bounded in time ($([math]::Round($sw.Elapsed.TotalSeconds,1)) s)"

# ---- C. the installer must not carry the old buffering call ------------------
$inst = Get-Content -LiteralPath (Join-Path $repo 'install.ps1') -Raw
Check ($inst -notmatch 'Invoke-WebRequest -Uri ''https://ollama\.com/download') "C1 install.ps1's direct ollama path streams too"
Check ($inst -notmatch 'Invoke-WebRequest -Uri \$SnapshotUrl') "C2 the snapshot download streams too"

Stop-Server
Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue

Write-Host ''
if ($fail -eq 0) { Write-Host "RESULT: PASS ($pass checks)" -ForegroundColor Green; exit 0 }
Write-Host "RESULT: FAIL ($fail failed, $pass passed)" -ForegroundColor Red
exit 1
