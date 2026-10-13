<#
  GPN Windows — run this box as a GPN node (miner + AI provider).

  A GPN node is ALWAYS both a miner and an AI provider — there is no role
  option, and no provider-only or mine-only mode. This script brings the whole
  node up together: the chain node/miner, the provider heartbeat, and the
  provider claim/infer loop. The one GPU is auto-yielded to an AI job while it
  runs (mining fully stops, then resumes — task #24).

  It does this by running the single-node supervisor `gpn_node.exe`, which
  starts, logs ([node]/[heartbeat]/[provider]), monitors and cleanly stops all
  of the above. Use Ctrl-C (or close the window) to stop everything.

  Prereqs (see shared/artifacts/project7/windows-client-test.md):
    * Ollama installed + running with at least one model pulled
      (default in-network model: llama3.2:1b).
    * The Windows release bundle unpacked next to this script in .\bin\
      (gpn_node.exe plus gpn-node.exe, gpn_llm_provider.exe,
      gpn_provider_status.exe, gpn_register.exe).
    * A funded wallet. For testnet, drip it from the faucet first.
    * Your provider's PUBLIC /status URL port-forwarded (default 8093).

  Usage:
    .\run-windows-provider.ps1 -Signer "//Alice" `
        -PublicStatusUrl "http://<your-public-ip>:8093/status" -Gflops 2500
#>
param(
  [string]$Chain           = "testnet",                # testnet | mainnet | dev | path to chain-spec.json
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

# 3. Configure the node (env overrides for the supervisor). A node is always
#    miner + provider, so we set BOTH sides: chain/node settings and provider.
Write-Host "[3/4] configuring node (miner + provider) ..."
$env:GPN_CHAIN   = $Chain
$env:GPN_ENDPOINT = $Rpc
$env:GPN_SIGNER  = $Signer
$env:GPN_OLLAMA  = $Ollama
$env:GPN_GFLOPS  = "$Gflops"
$env:GPN_LISTEN  = "0.0.0.0:$ListenPort"
if ($Address) { $env:GPN_ACCOUNT = $Address } else { Remove-Item Env:\GPN_ACCOUNT -ErrorAction SilentlyContinue }
if ($PublicStatusUrl) {
  $env:GPN_PROVIDER_ENDPOINT = $PublicStatusUrl
  Write-Host "      advertising on-chain endpoint: $PublicStatusUrl"
} else {
  Remove-Item Env:\GPN_PROVIDER_ENDPOINT -ErrorAction SilentlyContinue
  Write-Host "      GPN_PROVIDER_ENDPOINT unset — on-chain discovery disabled"
}

# 4. Run the supervisor: chain node/miner + heartbeat + provider loop, together.
Write-Host "[4/4] starting gpn_node (miner + provider, chain=$Chain signer=$Signer) ..."
& (Need "gpn_node.exe")
