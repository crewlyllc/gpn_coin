# install-ollama.ps1 - make ollama a first-class GPN node prerequisite (Windows).
#
# A GPN node cannot serve LLM jobs without ollama: the provider calls
# http://127.0.0.1:11434. Mining works without it, but every AI request fails.
# This makes the node never fail with a bare
# `pull ... failed: error sending request for url (http://127.0.0.1:11434/...)`.
#
# What it does (idempotent):
#   1. installs ollama if missing (winget, else the official installer),
#   2. starts `ollama serve` in the background if it isn't already listening,
#   3. pulls the baseline model(s) so the node offers a usable LLM from day one.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\install-ollama.ps1
#   $env:GPN_BASELINE_MODELS = "llama3.2:1b,qwen2.5:0.5b"   # optional override

$ErrorActionPreference = "Stop"

function Info($m) { Write-Host "[ollama] $m" }
function Warn($m) { Write-Warning "[ollama] $m" }

$Baseline = if ($env:GPN_BASELINE_MODELS) { $env:GPN_BASELINE_MODELS } else { "llama3.2:1b,qwen2.5:0.5b" }
$Endpoint = if ($env:OLLAMA_URL) { $env:OLLAMA_URL } else { "http://127.0.0.1:11434" }

function Test-OllamaListening {
    try {
        Invoke-RestMethod -Uri "$Endpoint/api/tags" -TimeoutSec 2 | Out-Null
        return $true
    } catch { return $false }
}

# ---- download ----------------------------------------------------------------------------------------------------------------------------------
# Row 289. `Invoke-WebRequest -OutFile` in Windows PowerShell 5.1 buffers the body
# in memory instead of streaming it to disk, so the 1.9 GB ollama installer on a
# 3 GB VM thrashed and sat at a 0-byte file for 18 min with ~300 s of CPU, no
# timeout and no retry -- measured on GPN-210 2026-11-27 05:12-05:32 PT (a HEAD to
# the same URL answered 200 / 1,953,587,080 B in 0.9 s). This streams to disk in
# 1 MiB chunks, bounds each attempt, retries a stated number of times, and names
# the URL plus the HTTP status or the exception when it gives up.
function Save-FileStreaming {
    param(
        [string]$Url,
        [string]$Path,
        [int]$Attempts = 3,
        [int]$TimeoutSeconds = 3600
    )
    # Synchronous HttpWebRequest on purpose: HttpClient's .GetAwaiter().GetResult()
    # deadlocked under pwsh (measured 2026-11-27 while writing this fix), and a
    # deadlock on a download is the same defect as the buffering it replaced.
    try { [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12 } catch { }
    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        $resp = $null; $in = $null; $out = $null
        try {
            $req = [System.Net.HttpWebRequest]::Create($Url)
            $req.Timeout = [int]([Math]::Min(120, $TimeoutSeconds) * 1000)
            $req.ReadWriteTimeout = [int]([Math]::Min(300, $TimeoutSeconds) * 1000)
            $req.UserAgent = 'gpn-installer'
            $resp = $req.GetResponse()
            $total = [int64]$resp.ContentLength
            $in  = $resp.GetResponseStream()
            $out = [System.IO.File]::Create($Path)
            $buf  = New-Object byte[] 1048576
            $done = [int64]0
            $nextReport = [int64](250MB)
            while (($n = $in.Read($buf, 0, $buf.Length)) -gt 0) {
                $out.Write($buf, 0, $n)
                $done += $n
                if ($done -ge $nextReport) {
                    Info "  $([math]::Round($done/1MB,0)) MiB downloaded"
                    $nextReport += 250MB
                }
            }
            $out.Close(); $out = $null
            $in.Close();  $in = $null
            if ($total -gt 0 -and $done -ne $total) {
                throw "short read: $done of $total bytes"
            }
            Info "downloaded $([math]::Round($done/1MB,0)) MiB from $Url"
            return $true
        } catch {
            $why = $_.Exception.Message
            $status = $null
            try { if ($_.Exception.Response) { $status = "HTTP $([int]$_.Exception.Response.StatusCode)" } } catch { }
            if ($status) { $why = "$status $why" }
            if ($out) { try { $out.Close() } catch { } }
            if ($in)  { try { $in.Dispose() } catch { } }
            if ($resp) { try { $resp.Close() } catch { } }
            Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
            if ($attempt -lt $Attempts) {
                Warn "download attempt $attempt/$Attempts failed: $why -- retrying in 5s"
                Start-Sleep -Seconds 5
            } else {
                Warn "download FAILED after $Attempts attempt(s): $Url -- $why"
            }
        }
    }
    return $false
}

# ---- 1. install --------------------------------------------------------------------------------------------------------------------------------
function Update-PathNow {
    # winget / the installer write to the MACHINE+USER PATH, but this process keeps
    # its old copy. Without this, the check right after a successful winget install
    # still says "not found" and we download the 1.5 GB installer a second time.
    $machine = [System.Environment]::GetEnvironmentVariable("Path", "Machine")
    $user    = [System.Environment]::GetEnvironmentVariable("Path", "User")
    if ($machine -or $user) { $env:Path = "$machine;$user" }
    # Each root is optional: on a non-Windows host (the test suite runs this file
    # under pwsh 7 on Linux) LOCALAPPDATA/ProgramFiles are unset and Join-Path
    # would throw instead of simply finding nothing.
    $roots = @()
    if ($env:LOCALAPPDATA)   { $roots += (Join-Path $env:LOCALAPPDATA 'Programs\Ollama') }
    if ($env:ProgramFiles)   { $roots += (Join-Path $env:ProgramFiles 'Ollama') }
    if (${env:ProgramFiles(x86)}) { $roots += (Join-Path ${env:ProgramFiles(x86)} 'Ollama') }
    foreach ($p in $roots) {
        if ($p -and (Test-Path (Join-Path $p 'ollama.exe'))) {
            if ($env:Path -notlike "*$p*") { $env:Path = "$p;$env:Path" }
        }
    }
}

function Find-Ollama { Get-Command ollama -ErrorAction SilentlyContinue }

$ollama = Find-Ollama
if (-not $ollama) { Update-PathNow; $ollama = Find-Ollama }

if ($ollama) {
    Info "ollama already installed: $($ollama.Source)"
} else {
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($winget) {
        Info "installing ollama via winget (Ollama.Ollama) -- this downloads ~1.5 GB"
        try {
            & winget install --id Ollama.Ollama -e --accept-source-agreements --accept-package-agreements --disable-interactivity
        } catch {
            Warn "winget install failed ($_). Falling back to the official installer."
        }
        Update-PathNow
        $ollama = Find-Ollama
        if ($ollama) { Info "winget installed ollama: $($ollama.Source)" }
    }

    if (-not $ollama) {
        $setup = if ($env:GPN_OLLAMA_SETUP_PATH) { $env:GPN_OLLAMA_SETUP_PATH } else { Join-Path $env:TEMP "OllamaSetup.exe" }
        if (-not (Test-Path (Split-Path -Parent $setup))) { New-Item -ItemType Directory -Force -Path (Split-Path -Parent $setup) | Out-Null }
        $haveIt = (Test-Path $setup) -and ((Get-Item $setup).Length -gt 100MB)
        if (-not $haveIt) {
            $url = if ($env:GPN_OLLAMA_SETUP_URL) { $env:GPN_OLLAMA_SETUP_URL } else { "https://ollama.com/download/OllamaSetup.exe" }
            $attempts = 3
            if ($env:GPN_OLLAMA_DOWNLOAD_ATTEMPTS -as [int]) { $attempts = [int]$env:GPN_OLLAMA_DOWNLOAD_ATTEMPTS }
            Info "downloading the official ollama installer (~1.9 GB, streamed to disk, no progress bar) ..."
            $ok = Save-FileStreaming -Url $url -Path $setup -Attempts $attempts
            if (-not $ok) {
                Warn "could not download the ollama installer from $url"
                Warn "this is a download failure, NOT an ollama problem: nothing was installed and the node was not started"
                Warn "fix the network or firewall, then retry: rerun install.ps1 (or this script), it resumes this step"
                Warn "if winget exists: winget install --id Ollama.Ollama -e"
                exit 1
            }
        }
        if ((Test-Path $setup) -and ((Get-Item $setup).Length -gt 100MB)) {
            Info "running $setup (/VERYSILENT) -- a few minutes, no output ..."
            Start-Process -FilePath $setup -ArgumentList "/VERYSILENT" -Wait
            Update-PathNow
            $ollama = Find-Ollama
        }
    }
}

if (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
    Warn "ollama is still not on PATH. Open a NEW terminal and re-run this script, or install from https://ollama.com/download"
    exit 1
}

# ---- 2. serve ------------------------------------------------------------------------------------------------------------------------------------
if (Test-OllamaListening) {
    Info "ollama is already serving at $Endpoint"
} else {
    Info "starting 'ollama serve' in the background ..."
    Start-Process -FilePath "ollama" -ArgumentList "serve" -WindowStyle Hidden
    $ok = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Seconds 1
        if (Test-OllamaListening) { $ok = $true; break }
    }
    if (-not $ok) {
        Warn "ollama did not come up at $Endpoint within 30s - start it manually with: ollama serve"
    } else {
        Info "ollama is up at $Endpoint"
    }
}

# ---- 3. baseline model(s) ------------------------------------------------------------------------------------------------------------
foreach ($m in ($Baseline -split "," | ForEach-Object { $_.Trim() } | Where-Object { $_ })) {
    Info "pulling baseline model: $m"
    & ollama pull $m
    if ($LASTEXITCODE -ne 0) { Warn "pull failed (will retry when the provider starts): $m" }
}

Info "done. The node can now serve LLM jobs (ollama at $Endpoint; baseline: $Baseline)"
