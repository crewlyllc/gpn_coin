@echo off
REM ============================================================================
REM  GPN Windows — start this box as a GPN node.
REM
REM  A GPN node is ALWAYS both a miner and an AI provider — there is no role
REM  option and no mine-only / provider-only mode. This launches the single-node
REM  supervisor gpn_node.exe, which brings up the chain node/miner, the provider
REM  heartbeat (:8093) and the AI provider claim/infer loop TOGETHER, with one
REM  log stream and clean shutdown (Ctrl-C stops everything).
REM
REM  The one GPU is auto-yielded to an AI job while it runs (mining fully stops
REM  for the job, then resumes — task #24).
REM
REM  Configure via env / a config file (see docs/run-a-node.md). Set:
REM    GPN_CHAIN, GPN_PROVIDER_ENDPOINT, GPN_OLLAMA, GPN_GFLOPS,
REM    GPN_ASSIGN_TOKEN, GPN_NODE_KEY_FILE, GPN_BOOTNODES
REM  Money identity (GPN_SIGNER_FILE -> keystore.json) is created on this
REM  machine on first run; only set GPN_SIGNER/GPN_ACCOUNT to OVERRIDE it
REM  (operator override / fleet install).
REM  Rewards go to the node's own (or the configured payout) account.
REM
REM  Ollama is REQUIRED to serve LLM jobs (mining works without it). If it is
REM  missing the node still mines, but cannot serve AI until you install + start
REM  it (see the Ollama step below).
REM
REM  NOTE: a second author on the SAME authoring key forks the chain. Only one
REM  designated author should mine per key. The external miner (if used) must run
REM  on a SEPARATE machine — gpn_external_miner refuses a localhost node, so one
REM  GPU can never double-mine.
REM ============================================================================

REM --- Node: miner + AI provider, supervised together --------------------------
set "GPN_CHAIN=testnet"
REM Money identity (queue #96). NEVER hard-code a signer or payout account here:
REM every install would then mine, sign and be PAID as the same account — and a
REM dev URI (a well-known test key) is derivable by anyone. The wallet is
REM generated once on this machine and reused forever; GPN_SIGNER/GPN_ACCOUNT
REM left empty on purpose (setting them OVERRIDES the local keystore).
set "GPN_BASE_PATH=C:\gpn\data"
if not exist "%GPN_BASE_PATH%" mkdir "%GPN_BASE_PATH%" >nul 2>&1
set "GPN_SIGNER="
set "GPN_ACCOUNT="
set "GPN_SIGNER_FILE=%GPN_BASE_PATH%\keystore.json"
"%~dp0..\gpn-agent\target\release\gpn_node.exe" --ensure-wallet
if errorlevel 1 (
  echo [gpn] ERROR: could not set up this machine's wallet - refusing to start.
  echo [gpn] ERROR: fix the keystore at %GPN_SIGNER_FILE%, or delete it on purpose to mine for a new account.
  pause
  exit /b 1
)
REM Per-machine identity. Never hard-code a key here: a shared key gives every
REM install the same peer id, so the boxes collide instead of peering.
set "GPN_NODE_KEY="
set "GPN_NODE_KEY_FILE=%USERPROFILE%\.gpn\node-key"
if not exist "%USERPROFILE%\.gpn" mkdir "%USERPROFILE%\.gpn" >nul 2>&1
if not exist "%GPN_NODE_KEY_FILE%" (
  echo [gpn] generating this machine's node identity: %GPN_NODE_KEY_FILE%
  gpn-node.exe key generate-node-key --file "%GPN_NODE_KEY_FILE%"
)
if not exist "%GPN_NODE_KEY_FILE%" (
  echo [gpn] ERROR: no node key at %GPN_NODE_KEY_FILE% - refusing to start with a shared identity.
  echo [gpn] ERROR: run:  gpn-node.exe key generate-node-key --file "%GPN_NODE_KEY_FILE%"
  pause
  exit /b 1
)
set "GPN_BIN=%~dp0..\gpn-agent\target\release"
REM ── Fast sync: weekly snapshot (default; GPN_FAST_SYNC=0 opts out) ────────
REM  A fresh node otherwise verifies ~200k blocks from peers (hours). The
REM  default imports the published weekly snapshot, but only after checking the
REM  ed25519-signed snapshot.json (release key built into gpn_snapshot.exe) and
REM  the archive's exact size + sha256. ANY failure falls back to a normal full
REM  sync; the install never fails because of fast sync.
if "%GPN_FAST_SYNC%"=="0" (
  echo [gpn] fast sync disabled by GPN_FAST_SYNC=0 - the node will sync from peers.
  goto :gpn_fast_sync_done
)
set "GPN_HAVE_DB="
if exist "%GPN_BASE_PATH%\chains" for /f "delims=" %%F in ('dir /b "%GPN_BASE_PATH%\chains" 2^>nul') do set "GPN_HAVE_DB=1"
if defined GPN_HAVE_DB (
  if exist "%GPN_BASE_PATH%\.gpn-pruning-archive" (
    echo [gpn] existing chain DB under %GPN_BASE_PATH% - fast sync not needed.
  ) else (
    echo [gpn] existing chain DB with no archive-pruning marker - NOT touching it; fast sync skipped.
  )
  goto :gpn_fast_sync_done
)
set "GPN_SNAPSHOT_URL=https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/gpn-testnet-snapshot.tar.gz"
set "GPN_SNAPSHOT_META_URL=https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json"
set "GPN_FS_TMP=%TEMP%\gpn-fast-sync-%RANDOM%%RANDOM%"
mkdir "%GPN_FS_TMP%" >nul 2>&1
where curl.exe >nul 2>&1
if errorlevel 1 (
  echo [gpn] fast sync unavailable (curl.exe not found) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
curl.exe -fsSL --connect-timeout 15 --max-time 60 -o "%GPN_FS_TMP%\snapshot.json" "%GPN_SNAPSHOT_META_URL%"
if errorlevel 1 (
  echo [gpn] fast sync unavailable (snapshot metadata not available) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
"%GPN_BIN%\gpn_snapshot.exe" verify --meta "%GPN_FS_TMP%\snapshot.json" > "%GPN_FS_TMP%\verify.txt" 2>&1
if errorlevel 1 (
  echo [gpn] fast sync unavailable (snapshot metadata failed verification) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
set "GPN_SNAP_SIZE="
set "GPN_SNAP_BLOCK="
for /f "tokens=1,* delims==" %%A in ('findstr /b "size=" "%GPN_FS_TMP%\verify.txt"') do set "GPN_SNAP_SIZE=%%B"
for /f "tokens=1,* delims==" %%A in ('findstr /b "block=" "%GPN_FS_TMP%\verify.txt"') do set "GPN_SNAP_BLOCK=%%B"
echo [gpn] weekly snapshot: block %GPN_SNAP_BLOCK%, %GPN_SNAP_SIZE% bytes - downloading...
for /f "delims=" %%F in ('powershell -NoProfile -Command "if ((Get-Item '%GPN_BASE_PATH%').PSDrive.Free -lt ([int64]%GPN_SNAP_SIZE% * 3)) { 'low' } else { 'ok' }"') do set "GPN_SPACE=%%F"
if not "%GPN_SPACE%"=="ok" (
  echo [gpn] fast sync skipped (free space is less than 3x the snapshot size).
  goto :gpn_fast_sync_cleanup
)
curl.exe -fL --retry 2 --retry-delay 2 --connect-timeout 15 --max-time 3600 -o "%GPN_FS_TMP%\snap.tar.gz" "%GPN_SNAPSHOT_URL%"
if errorlevel 1 (
  echo [gpn] fast sync unavailable (snapshot download failed) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
"%GPN_BIN%\gpn_snapshot.exe" verify --meta "%GPN_FS_TMP%\snapshot.json" --snapshot "%GPN_FS_TMP%\snap.tar.gz"
if errorlevel 1 (
  echo [gpn] fast sync unavailable (downloaded bytes failed verification) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
"%GPN_BIN%\gpn_snapshot.exe" import --snapshot "%GPN_FS_TMP%\snap.tar.gz" --base-path "%GPN_FS_TMP%\stage" --pruning archive
if errorlevel 1 (
  echo [gpn] fast sync unavailable (snapshot import failed) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
if not exist "%GPN_FS_TMP%\stage\chains" (
  echo [gpn] fast sync unavailable (import produced no chain DB) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
if exist "%GPN_BASE_PATH%\chains" (
  echo [gpn] fast sync unavailable (a chain DB appeared during import) - not overwriting it.
  goto :gpn_fast_sync_cleanup
)
move "%GPN_FS_TMP%\stage\chains" "%GPN_BASE_PATH%\chains" >nul
if errorlevel 1 (
  echo [gpn] fast sync unavailable (could not move the imported DB) - falling back to a normal full peer sync.
  goto :gpn_fast_sync_cleanup
)
if exist "%GPN_FS_TMP%\stage\.gpn-pruning-archive" move "%GPN_FS_TMP%\stage\.gpn-pruning-archive" "%GPN_BASE_PATH%\.gpn-pruning-archive" >nul 2>&1
echo [gpn] bootstrapped from the weekly snapshot at block %GPN_SNAP_BLOCK% - skipping the full peer sync.
:gpn_fast_sync_cleanup
if exist "%GPN_FS_TMP%" rmdir /s /q "%GPN_FS_TMP%" >nul 2>&1
:gpn_fast_sync_done
set "GPN_RPC_PORT=9944"
set "GPN_BOOTNODES=/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/23.241.109.255/tcp/30333/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz"
REM
REM --- Ollama: first-class prerequisite (needed to serve LLM jobs) -------------
REM  A GPN node mines without it, but CANNOT serve LLM jobs until Ollama runs.
where ollama >nul 2>nul
if errorlevel 1 (
  echo [gpn] Ollama NOT found - the node will MINE but cannot serve LLM jobs.
  echo [gpn]   install:  winget install Ollama.Ollama   ^(or: powershell -ExecutionPolicy Bypass -File scripts\install-ollama.ps1^)
  echo [gpn]   then:     ollama serve  ^^&^&  ollama pull llama3.2:1b
) else (
  echo [gpn] Ollama found - ensuring it is running...
  powershell -NoProfile -Command "try { Invoke-RestMethod 'http://127.0.0.1:11434/api/tags' -TimeoutSec 2 | Out-Null } catch { Write-Host '[gpn] starting Ollama...'; Start-Process ollama -ArgumentList 'serve' -WindowStyle Hidden }"
)
REM  Provider side (AI serving) -- set to match this box:
REM set "GPN_PROVIDER_ENDPOINT=http://<your-public-ip>:8093/status"
REM set "GPN_ASSIGN_TOKEN=<shared AP-provider secret>"
start "gpn-node" "C:\actions-runner\_work\gpn_coin\gpn_coin\gpn-agent\target\release\gpn_node.exe"

REM --- (OPTIONAL) external miner — run on a SEPARATE machine ----------------
REM  Uncomment the line below on a DIFFERENT PC (it will refuse to run here,
REM  since the node is on the same machine). Replace <NODE_IP> with the node's
REM  public IP, and <WALLET_SEED> with the miner's own wallet seed.
REM start "gpn-miner" "C:\actions-runner\_work\gpn_coin\gpn_coin\gpn-agent\target\release\gpn_external_miner.exe" http://<NODE_IP>:9944 0 1048576 2000 <WALLET_SEED>
