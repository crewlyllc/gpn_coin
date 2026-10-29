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
      (default in-network model: llama3.2:1b). REQUIRED to serve LLM jobs —
      without it the box still mines but cannot serve. Use -InstallOllama to
      auto-install/start it via scripts\install-ollama.ps1.
    * The Windows release bundle unpacked next to this script in .\bin\
      (gpn_node.exe plus gpn-node.exe, gpn_llm_provider.exe,
      gpn_provider_status.exe, gpn_register.exe).
    * A funded wallet. For testnet, drip it from the faucet first.
    * Your provider's PUBLIC /status URL port-forwarded (default 8093).

  Usage:
    .\run-windows-provider.ps1 -Signer "<your own seed or mnemonic>" `
        -PublicStatusUrl "http://<your-public-ip>:8093/status"

  IDENTITY (queue #96): -Signer is REQUIRED and has no default. This box must
  sign and be paid with an account that belongs to whoever runs it — a dev URI
  such as //Alice or //Bob is public knowledge, so shipping it as a default
  would make every install mine for the same account. Omit -Signer and the
  script stops and tells you how to create/attach a wallet on this machine.
  Explicitly passing -Signer (or GPN_SIGNER) is an operator/fleet OVERRIDE: it
  takes precedence over the local keystore, and the node logs it as such.
#>
param(
  [string]$Chain           = "testnet",                # testnet | mainnet | dev | path to chain-spec.json
  [string]$Rpc             = "ws://gpn-testnet.duckdns.org:9944",
  # Identity RPC for the heartbeat: MUST be this box's own node. It used to be
  # $Rpc — a seed node — so /status published the SEED's peer id in our
  # multiaddr (2026-10-23) and every peer failed to dial us.
  [string]$NodeRpc         = "ws://127.0.0.1:9944",
  [string]$Signer          = "",                       # REQUIRED: this box's own seed/mnemonic/dev URI (no default — see header)
  [string]$Address         = "",                     # ss58 payout/identity address (optional; shown if given)
  [string]$PublicStatusUrl = "",                     # e.g. http://1.2.3.4:8093/status  (enables on-chain discovery)
  [int]$ListenPort         = 8093,
  [int]$Gflops             = 0,          # upper bound in the MEASURED unit (~825 on an RTX 4060);
                                           # 0 = advertise the on-chain attestation (recommended)
  [string]$Ollama          = "http://127.0.0.1:11434",
  [string]$BaselineModel   = "llama3.2:1b",           # pulled if Ollama has no models
  # Referral code of whoever referred this box (4-32 chars, [A-Za-z0-9_-]).
  # Data collection only: written ONCE to %USERPROFILE%\.gpn\referral and
  # published on the provider heartbeat. Invalid/absent just warns.
  [string]$Ref             = $env:GPN_REF,
  [switch]$InstallOllama                              # auto-install/start Ollama if missing
)

$ErrorActionPreference = "Stop"
$bin = Join-Path $PSScriptRoot "bin"
function Need($exe) {
  $p = Join-Path $bin $exe
  if (-not (Test-Path $p)) { throw "missing $p — unpack the gpn-windows artifact into .\bin\" }
  return $p
}

# 0. Money identity (queue #96). No default signer: a shared dev URI in a
#    shipped script means every install mines and is paid as the same account.
if ([string]::IsNullOrWhiteSpace($Signer)) {
  Write-Host "[!] -Signer is empty: refusing to start a node without a money identity." -ForegroundColor Red
  Write-Host "    Option A (recommended): create/attach this machine's own wallet, then run the node:" -ForegroundColor Yellow
  Write-Host "        gpn_node.exe --ensure-wallet      # prints: payout account: 5XXXX..." -ForegroundColor Yellow
  Write-Host "        gpn_node.exe" -ForegroundColor Yellow
  Write-Host "    Option B: pass this box's own account:  -Signer '<your seed/mnemonic>'" -ForegroundColor Yellow
  exit 2
}

# 1. Ollama must be up — it is the inference backend. Make it a first-class,
#    self-healing step: if missing, either auto-install (-InstallOllama) or
#    fail with the exact, copy-pasteable commands.
function Test-Ollama {
  try { Invoke-RestMethod "$Ollama/api/tags" -TimeoutSec 2 | Out-Null; return $true } catch { return $false }
}
Write-Host "[1/4] checking Ollama at $Ollama ..."
if (-not (Test-Ollama)) {
  Write-Warning "Ollama not reachable at $Ollama — the node would still MINE but could not serve LLM jobs."
  if ($InstallOllama) {
    $helper = Join-Path $PSScriptRoot "scripts\install-ollama.ps1"
    if (-not (Test-Path $helper)) { throw "install helper not found: $helper" }
    Write-Host "      running $helper ..."
    & powershell -NoProfile -ExecutionPolicy Bypass -File $helper
  }
  if (-not (Test-Ollama)) {
    throw @"
Ollama is REQUIRED to serve LLM jobs (mining works without it).
Install + start it, then re-run this script:
    winget install Ollama.Ollama            # or https://ollama.com/download
    ollama serve
    ollama pull $BaselineModel
Or auto-install now:
    powershell -ExecutionPolicy Bypass -File .\scripts\install-ollama.ps1
Or re-run this script with:  -InstallOllama
"@
  }
}
$tags = Invoke-RestMethod "$Ollama/api/tags"
$models = @($tags.models | ForEach-Object { $_.name })
if ($models.Count -eq 0) {
  Write-Host "      Ollama has no models — pulling baseline: $BaselineModel ..."
  & ollama pull $BaselineModel
  $tags = Invoke-RestMethod "$Ollama/api/tags"
  $models = @($tags.models | ForEach-Object { $_.name })
}
if ($models.Count -eq 0) { throw "Ollama has no models. Run: ollama pull $BaselineModel" }
Write-Host "      models: $($models -join ', ')"

# 2. (optional) register this provider on-chain. Idempotent in practice — a
#    second call is harmless but logs the pallet error. Needs a funded wallet.
Write-Host "[2/4] registering provider (min_gflops=150) ..."
& (Need "gpn_register.exe") $Rpc $Signer

# 2b. Referral code (data collection only) — written ONCE, never fatal.
#     Same storage + rules as Linux: %USERPROFILE%\.gpn\referral, 4–32 chars of
#     [A-Za-z0-9_-], first value wins so a box cannot re-refer itself later.
if (-not [string]::IsNullOrWhiteSpace($Ref)) {
  $refCode = $Ref.Trim()
  $refDir  = Join-Path $env:USERPROFILE ".gpn"
  $refFile = Join-Path $refDir "referral"
  $refOk = ($refCode.Length -ge 4 -and $refCode.Length -le 32 -and $refCode -match '^[A-Za-z0-9_-]+$')
  if (-not $refOk) {
    Write-Host "[!] [referral] invalid code '$refCode' (need 4-32 chars of A-Z a-z 0-9 _ -) — continuing WITHOUT a referral" -ForegroundColor Yellow
  } elseif (Test-Path $refFile) {
    $existing = (Get-Content $refFile -TotalCount 1)
    Write-Host "[!] [referral] already set to '$existing'; ignoring new value '$refCode'" -ForegroundColor Yellow
  } else {
    try {
      New-Item -ItemType Directory -Force -Path $refDir | Out-Null
      # CreateNew = O_EXCL create: it throws if the file exists (even if another
      # run created it a microsecond ago), so the first value always wins.
      $fs = [System.IO.File]::Open($refFile, [System.IO.FileMode]::CreateNew,
                                   [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
      try {
        $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes("$refCode`n")
        $fs.Write($bytes, 0, $bytes.Length)
        $fs.Flush()
      } finally { $fs.Dispose() }
      Write-Host "      referral recorded: '$refCode' -> $refFile (write-once)"
    } catch [System.IO.IOException] {
      $existing = (Get-Content $refFile -TotalCount 1)
      Write-Host "[!] [referral] already set to '$existing'; ignoring new value '$refCode'" -ForegroundColor Yellow
    } catch {
      Write-Host "[!] [referral] could not write $refFile ($_) — continuing without a referral" -ForegroundColor Yellow
    }
  }
}

# 3. Configure the node (env overrides for the supervisor). A node is always
#    miner + provider, so we set BOTH sides: chain/node settings and provider.
Write-Host "[3/4] configuring node (miner + provider) ..."
$env:GPN_CHAIN   = $Chain
$env:GPN_ENDPOINT = $Rpc
# Peer identity is read from THIS box's node, never from the (remote) chain RPC.
$env:GPN_NODE_ENDPOINT = $NodeRpc
$env:GPN_SIGNER  = $Signer
Write-Host "      signer: explicit -Signer OVERRIDES the local keystore (fleet/operator override)" -ForegroundColor Yellow
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
