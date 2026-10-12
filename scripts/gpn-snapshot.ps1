# gpn-snapshot.ps1 — thin wrapper around the `gpn_snapshot.exe` binary (GPN-TASKS #23).
#
# Fast node onboarding on Windows: export an already-synced node's RocksDB + a
# manifest, then import it into a fresh node so it starts near the tip instead
# of full-syncing every block (with its Ethash seal) from genesis. This is the
# fix for the Windows box spending hours re-syncing ~123k blocks after a restart.
#
#   .\scripts\gpn-snapshot.ps1 info   [--rpc URL] [--out meta.json]
#   .\scripts\gpn-snapshot.ps1 export --base-path C:\path\to\node --out C:\snap.tar.gz [--freeze]
#   .\scripts\gpn-snapshot.ps1 import --snapshot C:\snap.tar.gz --base-path C:\new\dir [--force]
#
# NOTE: `--freeze` (SIGSTOP/SIGCONT) is a Unix feature; on Windows STOP the node
# before exporting (the tool detects a running node and refuses otherwise).
#
# Env:
#   GPN_SNAPSHOT_BIN  path to gpn_snapshot.exe (default: $env:USERPROFILE\.local\bin\gpn_snapshot.exe)
#   GPN_NODE_DATA     default --base-path

$ErrorActionPreference = "Stop"
$here = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

$bin = $env:GPN_SNAPSHOT_BIN
if (-not $bin) {
    $candidates = @(
        (Join-Path $env:USERPROFILE ".local\bin\gpn_snapshot.exe"),
        (Join-Path $here "gpn-agent\target\release\gpn_snapshot.exe"),
        (Join-Path $here "gpn-agent\target\debug\gpn_snapshot.exe")
    )
    foreach ($c in $candidates) { if (Test-Path $c) { $bin = $c; break } }
}
if (-not $bin -or -not (Test-Path $bin)) {
    Write-Error "gpn_snapshot.exe not found. Build it: cargo build --release --bin gpn_snapshot (or set GPN_SNAPSHOT_BIN)."
    exit 127
}

$argsList = @($args)
$hasBase = $false
foreach ($a in $argsList) { if ($a -like "--base-path*") { $hasBase = $true } }
if (-not $hasBase -and $env:GPN_NODE_DATA) { $argsList += @("--base-path", $env:GPN_NODE_DATA) }

& $bin @argsList
exit $LASTEXITCODE
