# r194-build-shallow.ps1 - rebuild the SHALLOW branch for row 194 on box 3.
#
# Why this is needed (2026-11-24 16:00 PT): the pair left by stage A is stale.
# `C:\gpn\r194\A` kept mining after the stage-A run and is now #167 work
# 1,648,549,690, while `C:\gpn\r194\S` stopped at #118 work 719,423,696 - so the
# branch is now LIGHTER than ours and can never win. A shallow accepted reorg
# needs: divergent tip, heavier work, height <= ours.
#
# Construction (the stage-A recipe, which produced a real accepted reorg at
# depth 22 on 0.3.74):
#   1. copy A to S3 (A stays frozen and is "ours" for the test);
#   2. `revert --blocks 20` on the copy, moving its head to the fork point;
#   3. attach a fresh witness (an empty data dir on the same spec) - sealing
#      pauses with 0 peers (service.rs peer-watch), and without a peer the
#      branch cannot mine at all;
#   4. mine on the RTX 4060 until branch work > A work while height <= A height.
#      Same GPU, no competing miner: blocks arrive faster than target, the
#      retarget hardens difficulty, and a shorter branch ends up heavier.
#
# The reverted blocks stay in S3's DB, but its own branch is the heavier head,
# so it keeps its tip (stage A behaved the same way; stage C healed only because
# that branch was never heavier under the 0.3.74 work_for_tip bug).
#
# Usage: launched detached; writes C:\gpn\r194\S3-BUILD.md and s3-build.log.

$ErrorActionPreference = "Continue"
$root   = "C:\gpn\r194\S3"
$wroot  = "C:\gpn\r194\SW"
$src    = "C:\gpn\r194\A"
$bin    = "C:\gpn\data\update\current\gpn-node.exe"
$spec   = "C:\gpn\scratch194\specs\raw241.json"
$log    = "C:\gpn\r194\s3-build.log"
$out    = "C:\gpn\r194\S3-BUILD.md"
$RPCB   = 10191
$RPCW   = 10192
$REVERT = 20

function Log($m) { "$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss')) $m" | Add-Content -Path $log }

function Rpc($port, $method, $params) {
  $body = @{ id = 1; jsonrpc = "2.0"; method = $method; params = $params } | ConvertTo-Json -Compress -Depth 5
  try {
    return Invoke-RestMethod -Uri "http://127.0.0.1:$port" -Method Post -ContentType "application/json" -Body $body -TimeoutSec 8
  } catch { return $null }
}

function PeerId($port) {
  $r = Rpc $port "system_localPeerId" @()
  if ($r -and $r.result) { return $r.result }
  return ""
}

function WorkOf($dir) {
  $f = Join-Path $dir "chain-work.json"
  if (Test-Path $f) {
    try {
      $j = Get-Content $f -Raw | ConvertFrom-Json
      return @{ work = [double]$j.work; height = [int]$j.height; hash = $j.hash }
    } catch { }
  }
  return @{ work = 0; height = -1; hash = "" }
}

Log "=== shallow build start (bin $bin, spec $spec)"
$ours = WorkOf $src
Log "ours (A, frozen): #$($ours.height) work=$($ours.work)"
if ($ours.height -lt 10) { Log "ABORT: A has no usable chain"; exit 2 }

Remove-Item -Recurse -Force $root, $wroot -ErrorAction SilentlyContinue
Log "copying A -> S3"
Copy-Item -Path $src -Destination $root -Recurse -Force
Get-ChildItem -Path $root -Recurse -Filter "secret_ed25519" -ErrorAction SilentlyContinue |
  ForEach-Object { Remove-Item -Force $_.FullName -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path $wroot | Out-Null

Log "reverting $REVERT blocks on the copy"
# NUM is positional (sc_cli RevertCmd): `revert [OPTIONS] [BLOCKS]`, default 256.
$rev = & $bin revert --chain $spec --base-path $root $REVERT 2>&1 | Out-String
Log ("revert output: " + ($rev -replace "`r?`n", " | "))
$base = WorkOf $root
Log "after revert: #$($base.height) work=$($base.work)"
if ($base.height -lt 1) { Log "ABORT: revert did not move the head"; exit 2 }

$env:GPN_REQUIRE_DIALABLE = "0"
$env:RUST_LOG = "info,gpn_node=debug,runtime::gpu-pow=debug"

$SELF = "/ip4/127.0.0.1/tcp/31399/p2p/12D3KooWKiDgAJ53LdVn8vCJcwBPVZNfKEAfYV6QT3XYfLyiPGQe"

$argsB = @(
  "--chain", $spec, "--base-path", $root, "--name", "r194-s3",
  "--listen-addr", "/ip4/127.0.0.1/tcp/31391",
  "--no-mdns", "--no-seed", "--no-discoverable", "--no-upnp",
  "--no-telemetry", "--no-prometheus",
  "--rpc-port", "$RPCB", "--rpc-methods", "unsafe", "--rpc-cors", "all",
  "--sync", "full", "--force-authoring", "--reserved-only",
  "--reserved-nodes", $SELF
)
Log "starting branch S3 (GPU miner)"
$pB = Start-Process -FilePath $bin -ArgumentList $argsB -WindowStyle Hidden -PassThru `
  -RedirectStandardOutput "C:\gpn\r194\S3.out.log" -RedirectStandardError "C:\gpn\r194\S3.err.log"
Log "S3 pid $($pB.Id)"

$idB = ""
for ($i = 0; $i -lt 60; $i++) { Start-Sleep -Seconds 3; $idB = PeerId $RPCB; if ($idB) { break } }
if (-not $idB) { Log "ABORT: S3 RPC never answered on $RPCB"; exit 3 }
Log "S3 peer id $idB"

# Fresh witness (empty dir, same spec): any peer lifts the sealing pause.
$env:CUDA_VISIBLE_DEVICES = "X"
$argsW = @(
  "--chain", $spec, "--base-path", $wroot, "--name", "r194-sw",
  "--listen-addr", "/ip4/127.0.0.1/tcp/31392",
  "--no-mdns", "--no-seed", "--no-discoverable", "--no-upnp",
  "--no-telemetry", "--no-prometheus",
  "--rpc-port", "$RPCW", "--rpc-methods", "unsafe", "--rpc-cors", "all",
  "--sync", "full", "--reserved-only",
  "--reserved-nodes", "/ip4/127.0.0.1/tcp/31391/p2p/$idB"
)
Log "starting witness SW (CPU-only, fresh dir)"
$pW = Start-Process -FilePath $bin -ArgumentList $argsW -WindowStyle Hidden -PassThru `
  -RedirectStandardOutput "C:\gpn\r194\SW.out.log" -RedirectStandardError "C:\gpn\r194\SW.err.log"
Log "SW pid $($pW.Id)"
Remove-Item Env:CUDA_VISIBLE_DEVICES -ErrorAction SilentlyContinue

$idW = ""
for ($i = 0; $i -lt 40; $i++) { Start-Sleep -Seconds 3; $idW = PeerId $RPCW; if ($idW) { break } }
if ($idW) {
  Log "SW peer id $idW"
  $null = Rpc $RPCB "system_addReservedPeer" @("/ip4/127.0.0.1/tcp/31392/p2p/$idW")
} else {
  Log "WARN: witness never answered - branch sealing will stay paused"
}

$deadline = (Get-Date).AddMinutes(45)
$lastH = -1
$stall = 0
$ok = $false
while ((Get-Date) -lt $deadline) {
  Start-Sleep -Seconds 20
  $w = WorkOf $root
  $peers = 0
  $health = Rpc $RPCB "system_health" @()
  if ($health -and $health.result) { $peers = $health.result.peers }
  Log ("t+ S3 #$($w.height) work=$($w.work) peers=$peers (target: > $($ours.work) at height <= $($ours.height))")
  if ($w.work -gt $ours.work -and $w.height -le $ours.height) { $ok = $true; break }
  if ($w.height -gt $ours.height) { break }
  if ($w.height -eq $lastH) { $stall += 20 } else { $lastH = $w.height; $stall = 0 }
  if ($stall -ge 600) { Log "STALL: height stuck at #$($w.height) for ${stall}s"; break }
}

$w = WorkOf $root
Log "freezing: stopping S3 + SW (work=$($w.work) height=$($w.height))"
Stop-Process -Id $pB.Id -Force -ErrorAction SilentlyContinue
Stop-Process -Id $pW.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 5

$verdict = if ($ok) { "READY" } else { "INCOMPLETE" }
$md = @(
  "# row 194 shallow branch (S3) - $verdict",
  "",
  "Built $((Get-Date).ToString('yyyy-MM-dd HH:mm:ss')) on box 3 by r194-build-shallow.ps1.",
  "Copy of A (frozen ours), reverted $REVERT blocks, re-mined on the RTX 4060.",
  "",
  "- ours (A):  ``C:\gpn\r194\A``  height #$($ours.height) work $($ours.work)",
  "- branch S3: ``C:\gpn\r194\S3`` height #$($w.height) work $($w.work) tip $($w.hash)",
  "- witness:   ``C:\gpn\r194\SW`` (fresh dir, same spec)",
  "- fork point after revert: #$($base.height) work $($base.work)",
  "",
  "heavier = $($w.work) -gt $($ours.work) => $($w.work -gt $ours.work)",
  "shorter/equal = #$($w.height) -le #$($ours.height) => $($w.height -le $ours.height)",
  "",
  "Next: run r194-fork-test.ps1 -OursDir C:\gpn\r194\A -TheirsDir C:\gpn\r194\S3 -Expect accept"
) -join "`r`n"
$md | Set-Content -Path $out -Encoding UTF8
Log "wrote $out ($verdict)"
Log "=== shallow build end"
