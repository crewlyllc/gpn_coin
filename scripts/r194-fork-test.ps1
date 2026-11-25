# r194-fork-test.ps1 - run ONE row-194 fork-choice scenario on box 3.
#
# Purpose: the live half of row 194 needs a scenario run with a binary that
# contains 87d77b3b, and the frozen rigs on box 3 already hold the chains.
# This script never mutates a frozen rig: it COPIES ours/theirs to
# C:\gpn\r194\run\<label>\ and works on the copies.
#
# Two scenarios (same script, different arguments):
#   shallow  (-Expect accept): ours A, theirs S  -> expect "Reorg on #..." plus
#            "import: fork block" lines, i.e. the heavier shorter branch wins.
#   deep     (-Expect refuse): ours E1, theirs B2 (+ witness BW)
#            -> expect "reorg=refused:deep-fork", gpn-forked marker, sealing
#            paused, and ZERO "import: fork block" lines.
#
# Lessons baked in (from C:\gpn\r194\ROW194-2026-11-24.md):
#   * delete the copied chains/<x>/network/secret_ed25519 in every copy, or the
#     two nodes share a peer id and refuse to peer with themselves;
#   * a reverted copy is NOT an isolated fork - only use fresh-from-genesis rigs;
#   * sealing pauses with 0 peers, so a branch rig needs its witness attached;
#   * never reuse fork\BL2 (lighter than ours under 0.3.74).
#
# ASCII only: Windows PowerShell 5.1 decodes run: blocks with the ANSI codepage.
#
# Example (deep, after the zip is on the box):
#   powershell -NoProfile -ExecutionPolicy Bypass -File C:\gpn\r194\r194-fork-test.ps1 `
#     -ZipPath C:\gpn\r194\gpn-windows.zip -Spec C:\gpn\scratch194\specs\raw244.json `
#     -OursDir C:\gpn\r194\E1 -TheirsDir C:\gpn\r194\B2 -WitnessDir C:\gpn\r194\BW `
#     -OursLabel e1 -TheirsLabel b2 -Expect refuse -TimeoutSec 300

param(
  [string]$ZipPath = "",
  [string]$BinDir = "",
  [string]$Version = "0.3.75",
  [Parameter(Mandatory = $true)][string]$Spec,
  [Parameter(Mandatory = $true)][string]$OursDir,
  [Parameter(Mandatory = $true)][string]$TheirsDir,
  [string]$WitnessDir = "",
  [Parameter(Mandatory = $true)][string]$OursLabel,
  [Parameter(Mandatory = $true)][string]$TheirsLabel,
  [string]$Expect = "refuse",
  [int]$RpcOurs = 10297,
  [int]$RpcTheirs = 10298,
  [int]$RpcWitness = 10299,
  [int]$PortOurs = 31497,
  [int]$PortTheirs = 31498,
  [int]$PortWitness = 31499,
  [int]$TimeoutSec = 300,
  [string]$OutFile = ""
)

$ErrorActionPreference = "Continue"
$root = "C:\gpn\r194"
if (-not $OutFile) {
  $OutFile = Join-Path $root ("R194-TEST-" + $OursLabel + "-vs-" + $TheirsLabel + ".md")
}
$logPath = Join-Path $root ("R194-TEST-" + $OursLabel + "-vs-" + $TheirsLabel + ".log")

function Log($m) {
  $line = "$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss')) $m"
  $line | Add-Content -Path $logPath
  Write-Output $line
}

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

# ------------------------------------------------------------------ binary
if ($ZipPath) {
  $BinDir = Join-Path $root ("bin-" + $Version)
  if (-not (Test-Path (Join-Path $BinDir "gpn-node.exe"))) {
    Log "expanding $ZipPath -> $BinDir"
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    Expand-Archive -Path $ZipPath -DestinationPath $BinDir -Force
  }
}
if (-not $BinDir) { Log "ABORT: need -ZipPath or -BinDir"; exit 2 }
$bin = Join-Path $BinDir "gpn-node.exe"
if (-not (Test-Path $bin)) {
  # zip may nest one directory
  $found = Get-ChildItem -Path $BinDir -Recurse -Filter gpn-node.exe -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($found) { $bin = $found.FullName } else { Log "ABORT: gpn-node.exe not found under $BinDir"; exit 2 }
}
$ver = & $bin --version 2>&1 | Out-String
Log "binary: $bin"
Log "version: $($ver.Trim())"
if ($ver -notmatch [regex]::Escape($Version)) {
  Log "ABORT: binary version does not contain $Version (got '$($ver.Trim())')"
  exit 2
}

# ------------------------------------------------------------------ copies
$runDir = Join-Path $root ("run\" + $OursLabel + "-vs-" + $TheirsLabel)
if (Test-Path $runDir) { Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path $runDir | Out-Null
$oursCopy = Join-Path $runDir "ours"
$theirsCopy = Join-Path $runDir "theirs"
$witCopy = Join-Path $runDir "witness"

$before = WorkOf $OursDir
$theirsBefore = WorkOf $TheirsDir
Log ("before: ours " + $OursLabel + " #" + $before.height + " work=" + $before.work + " | theirs " + $TheirsLabel + " #" + $theirsBefore.height + " work=" + $theirsBefore.work)

Log "copying frozen rigs into $runDir"
Copy-Item -Path $OursDir -Destination $oursCopy -Recurse -Force
Copy-Item -Path $TheirsDir -Destination $theirsCopy -Recurse -Force
if ($WitnessDir) { Copy-Item -Path $WitnessDir -Destination $witCopy -Recurse -Force }

# distinct peer identities (a copied node key makes both sides refuse the link)
foreach ($d in @($oursCopy, $theirsCopy, $witCopy)) {
  if (Test-Path $d) {
    Get-ChildItem -Path $d -Recurse -Filter "secret_ed25519" -ErrorAction SilentlyContinue |
      ForEach-Object { Remove-Item -Force $_.FullName -ErrorAction SilentlyContinue }
  }
}

$env:GPN_REQUIRE_DIALABLE = "0"
$env:RUST_LOG = "info,gpn_node=debug,runtime::gpu-pow=debug"

function Start-Node($dir, $name, $port, $rpc, $extra, $tag) {
  $a = @(
    "--chain", $Spec, "--base-path", $dir, "--name", $name,
    "--listen-addr", "/ip4/127.0.0.1/tcp/$port",
    "--no-mdns", "--no-seed", "--no-discoverable", "--no-upnp",
    "--no-telemetry", "--no-prometheus",
    "--rpc-port", "$rpc", "--rpc-methods", "unsafe", "--rpc-cors", "all",
    "--sync", "full", "--reserved-only"
  ) + $extra
  $outF = Join-Path $root ($tag + ".out.log")
  $errF = Join-Path $root ($tag + ".err.log")
  Remove-Item -Force $outF, $errF -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $bin -ArgumentList $a -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $outF -RedirectStandardError $errF
  Log "$tag pid $($p.Id) rpc $rpc listen $port"
  return @{ proc = $p; out = $outF; err = $errF }
}

# Placeholder reserved entry: keeps the node from adopting public oracle seeds.
$SELF = "/ip4/127.0.0.1/tcp/31399/p2p/12D3KooWKiDgAJ53LdVn8vCJcwBPVZNfKEAfYV6QT3XYfLyiPGQe"

# ------------------------------------------------------- theirs (+ witness)
$t = Start-Node $theirsCopy ("r194-" + $TheirsLabel) $PortTheirs $RpcTheirs @(
  "--force-authoring", "--reserved-nodes", $SELF
) ("theirs-" + $TheirsLabel)
$idT = ""
for ($i = 0; $i -lt 60; $i++) { Start-Sleep -Seconds 3; $idT = PeerId $RpcTheirs; if ($idT) { break } }
if (-not $idT) { Log "ABORT: theirs RPC never answered on $RpcTheirs (see $($t.err))"; exit 3 }
Log "theirs peer id $idT"

$w = $null
$idW = ""
if ($WitnessDir) {
  $env:CUDA_VISIBLE_DEVICES = "X"
  $w = Start-Node $witCopy ("r194-" + $TheirsLabel + "-w") $PortWitness $RpcWitness @(
    "--reserved-nodes", "/ip4/127.0.0.1/tcp/$PortTheirs/p2p/$idT"
  ) ("witness-" + $TheirsLabel)
  Remove-Item Env:CUDA_VISIBLE_DEVICES -ErrorAction SilentlyContinue
  for ($i = 0; $i -lt 40; $i++) { Start-Sleep -Seconds 3; $idW = PeerId $RpcWitness; if ($idW) { break } }
  if ($idW) {
    Log "witness peer id $idW"
    $null = Rpc $RpcTheirs "system_addReservedPeer" @("/ip4/127.0.0.1/tcp/$PortWitness/p2p/$idW")
  } else {
    Log "WARN: witness RPC never answered - branch sealing may pause (not fatal, B2 is already mined)"
  }
}

# Keep the GPU for the branch node only while it may still need to mine.
# ------------------------------------------------------------------ ours
# Ours does not mine: the scenario is about how it reacts to a peer's announce.
$env:CUDA_VISIBLE_DEVICES = "X"
$o = Start-Node $oursCopy ("r194-" + $OursLabel) $PortOurs $RpcOurs @(
  "--reserved-nodes", "/ip4/127.0.0.1/tcp/$PortTheirs/p2p/$idT"
) ("ours-" + $OursLabel)
Remove-Item Env:CUDA_VISIBLE_DEVICES -ErrorAction SilentlyContinue

$idO = ""
for ($i = 0; $i -lt 60; $i++) { Start-Sleep -Seconds 3; $idO = PeerId $RpcOurs; if ($idO) { break } }
if (-not $idO) { Log "ABORT: ours RPC never answered on $RpcOurs (see $($o.err))"; exit 3 }
Log "ours peer id $idO"
$null = Rpc $RpcTheirs "system_addReservedPeer" @("/ip4/127.0.0.1/tcp/$PortOurs/p2p/$idO")

# ------------------------------------------------------------------ observe
# Watch our node's log for the verdict. Fork choice is logged on the side that
# receives the announce.
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$verdictSeen = $false
$reorgSeen = $false
$forkImport = 0
$refused = ""
while ((Get-Date) -lt $deadline) {
  Start-Sleep -Seconds 15
  $txt = ""
  if (Test-Path $o.err) { $txt = Get-Content $o.err -Raw -ErrorAction SilentlyContinue }
  if (-not $txt) { $txt = "" }
  if ($txt -match "reorg=refused:([a-z\-]+)") { $refused = $Matches[1]; $verdictSeen = $true }
  if ($txt -match "fork-choice: work ours=") { $verdictSeen = $true }
  if ($txt -match "Reorg on #") { $reorgSeen = $true }
  $forkImport = ([regex]::Matches($txt, "import: fork block")).Count
  Log ("t+ observed: verdict=$verdictSeen reorg=$reorgSeen forkImports=$forkImport refused='$refused'")
  if ($Expect -eq "accept" -and $reorgSeen -and $forkImport -gt 0) { break }
  if ($Expect -eq "refuse" -and $refused -and $forkImport -gt 0) { break }
}

$after = WorkOf $oursCopy
$marker = Join-Path $oursCopy "gpn-forked"
$markerText = ""
if (Test-Path $marker) { $markerText = (Get-Content $marker -Raw) }

$oTxt = ""
if (Test-Path $o.err) { $oTxt = Get-Content $o.err -Raw }
$keys = @(
  "fork-choice:", "reorg=", "Reorg on", "import: fork block",
  "block sealing is PAUSED", "not submitting this CPU seal", "canonical/anchor-match"
)
$lines = @()
if ($oTxt) {
  $lines = $oTxt -split "`r?`n" | Where-Object {
    $l = $_
    ($keys | Where-Object { $l -like ("*" + $_ + "*") }).Count -gt 0
  } | Select-Object -Last 60
}

function Passed() {
  if ($Expect -eq "accept") {
    if ($reorgSeen -and $forkImport -gt 0 -and $after.height -gt 0) { return $true }
    return $false
  }
  if ($refused -eq "deep-fork" -and $forkImport -eq 0 -and (Test-Path $marker)) { return $true }
  return $false
}
$pass = Passed

$md = @()
$md += "# row 194 fork test: " + $OursLabel + " (ours) vs " + $TheirsLabel + " (theirs) - " + $(if ($pass) { "PASS" } else { "FAIL" })
$md += ""
$md += "Run " + (Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + " by scripts/r194-fork-test.ps1 on box 3."
$md += ""
$md += "- binary: " + $bin + " -> " + $ver.Trim()
$md += "- spec: " + $Spec
$md += "- expect: " + $Expect + " | observed: refused='" + $refused + "' reorg=" + $reorgSeen + " forkImports=" + $forkImport
$md += "- before: ours #" + $before.height + " work=" + $before.work + " ; theirs #" + $theirsBefore.height + " work=" + $theirsBefore.work
$md += "- after:  ours #" + $after.height + " work=" + $after.work + " hash=" + $after.hash
$md += "- gpn-forked marker present: " + (Test-Path $marker)
if ($markerText) { $md += "- marker contents: " + ($markerText -replace "`r?`n", " / ") }
$md += ""
$md += "## Ours node log (filtered)"
$md += ""
$md += '```'
foreach ($l in $lines) { $md += $l }
$md += '```'
$md += ""
$md += "Verdict: " + $(if ($pass) { "PASS - the box's condition is met." } else { "FAIL - condition not met; read the log above for the actual verdict line." })
($md -join "`r`n") | Set-Content -Path $OutFile -Encoding UTF8
Log "wrote $OutFile (PASS=$pass)"

# ------------------------------------------------------------------ teardown
foreach ($p in @($o.proc, $t.proc)) {
  if ($p) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
}
if ($w -and $w.proc) { Stop-Process -Id $w.proc.Id -Force -ErrorAction SilentlyContinue }
Start-Sleep -Seconds 5
Log "=== test end (PASS=$pass)"

Get-Content $OutFile -Raw
if ($pass) { exit 0 } else { exit 1 }
