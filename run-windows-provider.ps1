<#
  GPN Windows — run this box as an AI PROVIDER (serves LLM inference jobs).

  A GPN node is one entity: mine OR serve AI. This script runs the *provider*
  side only (no mining) — the node's own miner stays off so the one GPU is
  dedicated to inference while the provider serves.

  Prereqs (see shared/artifacts/project7/windows-client-test.md §"Run your
  Windows box as an AI provider"):
    * Ollama installed + running with at least one model pulled
      (default in-network model: llama3.2:1b).
    * The Windows release bundle unpacked next to this script in .\bin\
      (gpn-node.exe, gpn_llm_provider.exe, gpn_provider_status.exe, gpn_register.exe).
    * A funded provider wallet. For testnet, drip it from the faucet first.
    * Your provider's PUBLIC /status URL port-forwarded (default 8093).

  Usage:
    .\run-windows-provider.ps1 -Signer "//Alice" `
        -PublicStatusUrl "http://<your-public-ip>:8093/status" -Gflops 2500
#>
param(
  [string]$Rpc             = "ws://gpn-testnet.duckdns.org:9944",
  [string]$Signer          = "//Alice",
  [string]$Address         = "",                     # ss58 payout/identity address (optional; shown if given)
  [string]$PublicStatusUrl = "",                     # e.g. http://1.2.3.4:8093/status  (enables on-chain discovery)
  [int]$ListenPort         = 8093,
  [int]$Gflops             = 2500,
  [string]$Ollama          = "http://127.0.0.1:11434"
)

$ErrorActionPreference = "Stop"
$bin = Join-Path $PSScriptRoot "bin"
function Need($exe) {
  $p = Join-Path $bin $exe
  if (-not (Test-Path $p)) { throw "missing $p — unpack the gpn-windows artifact into .\bin\" }
  return $p
}

# 1. Ollama must be up (it is the inference backend).
Write-Host "[1/4] checking Ollama at $Ollama ..."
try { $tags = Invoke-RestMethod "$Ollama/api/tags" } catch { throw "Ollama not reachable at $Ollama — start it (ollama serve)." }
$models = @($tags.models | ForEach-Object { $_.name })
if ($models.Count -eq 0) { throw "Ollama has no models. Run: ollama pull llama3.2:1b" }
Write-Host "      models: $($models -join ', ')"

# 2. (optional) register this provider on-chain. Idempotent in practice — a
#    second call is harmless but logs the pallet error. Needs a funded wallet.
Write-Host "[2/4] registering provider (min_gflops=150) ..."
& (Need "gpn_register.exe") $Rpc $Signer

# 3. Heartbeat: serve GET /status on :$ListenPort so the scheduler can route here.
Write-Host "[3/4] starting /status heartbeat on 0.0.0.0:$ListenPort ..."
$statusArgs = @("--listen", "0.0.0.0:$ListenPort", "--gflops", "$Gflops", "--ollama", $Ollama)
if ($Address) { $statusArgs += @("--address", $Address) }
Start-Process -FilePath (Need "gpn_provider_status.exe") -ArgumentList $statusArgs -WindowStyle Minimized

# 4. The provider loop: claims assigned/claimable jobs, runs inference, settles.
Write-Host "[4/4] starting gpn_llm_provider (RPC=$Rpc signer=$Signer) ..."
if ($PublicStatusUrl) { $env:GPN_PROVIDER_ENDPOINT = $PublicStatusUrl; Write-Host "      advertising on-chain endpoint: $PublicStatusUrl" }
else { Remove-Item Env:\GPN_PROVIDER_ENDPOINT -ErrorAction SilentlyContinue; Write-Host "      GPN_PROVIDER_ENDPOINT unset — on-chain discovery disabled" }
& (Need "gpn_llm_provider.exe") $Rpc $Signer
