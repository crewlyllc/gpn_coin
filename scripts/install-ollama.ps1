# install-ollama.ps1 — make ollama a first-class GPN node prerequisite (Windows).
#
# A GPN node cannot serve LLM jobs without ollama: the provider calls
# http://127.0.0.1:11434. Mining works without it, but every AI request fails.
# This makes the node never fail with a bare
# `pull … failed: error sending request for url (http://127.0.0.1:11434/…)`.
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

# ── 1. install ────────────────────────────────────────────────────────────────
$ollama = Get-Command ollama -ErrorAction SilentlyContinue
if ($ollama) {
    Info "ollama already installed: $($ollama.Source)"
} else {
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($winget) {
        Info "installing ollama via winget (Ollama.Ollama) …"
        try {
            winget install --id Ollama.Ollama -e --accept-source-agreements --accept-package-agreements
        } catch {
            Warn "winget install failed ($_). Falling back to the official installer."
        }
    }
    if (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
        Info "downloading the official ollama installer (OllamaSetup.exe) …"
        $setup = Join-Path $env:TEMP "OllamaSetup.exe"
        Invoke-WebRequest -Uri "https://ollama.com/download/OllamaSetup.exe" -OutFile $setup
        Info "running $setup (/VERYSILENT) …"
        Start-Process -FilePath $setup -ArgumentList "/VERYSILENT" -Wait
    }
    # winget/installer put it on PATH, but this process's PATH may be stale.
    $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                [System.Environment]::GetEnvironmentVariable("Path", "User")
}

if (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
    Warn "ollama is still not on PATH. Open a NEW terminal and re-run this script, or install from https://ollama.com/download"
    exit 1
}

# ── 2. serve ──────────────────────────────────────────────────────────────────
if (Test-OllamaListening) {
    Info "ollama is already serving at $Endpoint"
} else {
    Info "starting 'ollama serve' in the background …"
    Start-Process -FilePath "ollama" -ArgumentList "serve" -WindowStyle Hidden
    $ok = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Seconds 1
        if (Test-OllamaListening) { $ok = $true; break }
    }
    if (-not $ok) {
        Warn "ollama did not come up at $Endpoint within 30s — start it manually with: ollama serve"
    } else {
        Info "ollama is up at $Endpoint"
    }
}

# ── 3. baseline model(s) ──────────────────────────────────────────────────────
foreach ($m in ($Baseline -split "," | ForEach-Object { $_.Trim() } | Where-Object { $_ })) {
    Info "pulling baseline model: $m"
    & ollama pull $m
    if ($LASTEXITCODE -ne 0) { Warn "pull failed (will retry when the provider starts): $m" }
}

Info "done. The node can now serve LLM jobs (ollama at $Endpoint; baseline: $Baseline)"
