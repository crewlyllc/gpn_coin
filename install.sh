#!/usr/bin/env bash
# install.sh — GPN one-command installer
#
# Installs the GPN node + miner/agent (GPU mining + AI compute) and generates a
# wallet, so a GPU owner can start earning with a single command.
#
# Usage:
#   curl -fsSL <REPO>/install.sh | sh        # once the repo is public
#   ./install.sh                             # from a local clone
#
# Env overrides:
#   GPN_PREFIX   install dir (default ~/.gpn)
#   GPN_BIN      bin dir    (default ~/.local/bin)
#   GPN_NETWORK  testnet | mainnet (default testnet)
#   GPN_REPO_URL git URL    (default: current repo's origin)
#   GPN_FAST_SYNC      1 (default) bootstraps from the weekly snapshot; 0 = full sync
#   GPN_SNAPSHOT_URL   override the snapshot archive URL
#   GPN_SNAPSHOT_META_URL  override the signed snapshot.json URL
#   GPN_REF      referral code (same as --ref); data collection only
#
# Flags:
#   --no-fast-sync   skip the snapshot and sync every block from peers
#   --fast-sync      force it on (the default), overriding GPN_FAST_SYNC=0
#   --ref CODE       referral code of whoever referred you (4-32 chars,
#                    [A-Za-z0-9_-]). Stored ONCE in $PREFIX/referral and
#                    published on the provider heartbeat. An invalid code only
#                    warns — it never fails the install.
#
# Referral codes are recorded for the testnet-participation allocation at
# mainnet launch (amounts TBD). They cannot be reconstructed later, so they are
# captured at install time and never rewritten (see scripts/gpn-referral.sh).

set -euo pipefail

GPN_REPO_URL="${GPN_REPO_URL:-}"
PREFIX="${GPN_PREFIX:-$HOME/.gpn}"
BIN_DIR="${GPN_BIN:-$HOME/.local/bin}"
NETWORK="${GPN_NETWORK:-testnet}"
SRC="$PREFIX/src"
GPN_BASE_PATH="$PREFIX/node"

log() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33m !\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

# ── Fast sync (weekly snapshot) ────────────────────────────────────────────────
# A fresh node otherwise verifies ~200k blocks from peers (hours). The default is
# therefore to import the published weekly snapshot, verified against a *signed*
# metadata document before a single byte is trusted. It is opt-OUT, and every
# failure — no metadata, bad signature, 404, short disk, hash mismatch, import
# error — is a fallback to the normal full sync, never an install failure.
SNAPSHOT_URL="${GPN_SNAPSHOT_URL:-https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/gpn-testnet-snapshot.tar.gz}"
SNAPSHOT_META_URL="${GPN_SNAPSHOT_META_URL:-https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json}"

# `--no-fast-sync` / `--fast-sync`; env GPN_FAST_SYNC=0 disables.
FAST_SYNC=1
[ "${GPN_FAST_SYNC:-1}" = 0 ] && FAST_SYNC=0
CLI_FAST_SYNC=""
# Referral code: `--ref CODE` / `--ref=CODE` / `GPN_REF=CODE`. Optional; a bad
# value is a warning, never an install failure (see the write step below).
REF_CODE="${GPN_REF:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --no-fast-sync) FAST_SYNC=0; CLI_FAST_SYNC=0; shift ;;
    --fast-sync)    FAST_SYNC=1; CLI_FAST_SYNC=1; shift ;;
    --ref)          [ $# -ge 2 ] || die "--ref needs a code (4-32 chars, [A-Za-z0-9_-])"; REF_CODE="$2"; shift 2 ;;
    --ref=*)        REF_CODE="${1#--ref=}"; shift ;;
    -h|--help) sed -n '2,/^set -euo pipefail/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

# One clear line, then normal sync. Never fatal.
fast_sync_fallback() {
  warn "fast sync unavailable ($1) — falling back to a normal full peer sync (slower, still correct)."
}

fast_sync() {
  local base="$GPN_BASE_PATH"
  local snap_bin="$BIN_DIR/gpn_snapshot"

  if [ "$FAST_SYNC" = 0 ]; then
    if [ "$CLI_FAST_SYNC" = 0 ]; then
      log "fast sync disabled (--no-fast-sync) — the node will sync from peers."
    else
      log "fast sync disabled (GPN_FAST_SYNC=0) — the node will sync from peers."
    fi
    return 0
  fi

  # Never touch an existing chain DB. A DB created with a different pruning mode
  # is exactly what makes `gpn-node` fail with `Incompatible pruning modes` /
  # `Column families not opened` — importing over it would corrupt the install.
  if [ -d "$base/chains" ] && [ -n "$(ls -A "$base/chains" 2>/dev/null || true)" ]; then
    if [ -f "$base/.gpn-pruning-archive" ]; then
      log "found an existing chain DB at $base — fast sync not needed (leaving it untouched)."
    else
      warn "found an existing chain DB at $base with no archive-pruning marker — NOT touching it; fast sync skipped."
    fi
    return 0
  fi

  if [ ! -x "$snap_bin" ]; then fast_sync_fallback "gpn_snapshot not installed"; return 0; fi
  command -v curl >/dev/null 2>&1 || { fast_sync_fallback "curl not found"; return 0; }

  # Everything below happens inside a scratch dir that is deleted on the way
  # out (success or failure). A failed fast sync leaves the real base path with
  # nothing but what was already there.
  FS_WORK="$(mktemp -d "$PREFIX/.fast-sync.XXXXXX" 2>/dev/null)" \
    || { fast_sync_fallback "cannot create a temp dir under $PREFIX"; return 0; }
  _fast_sync_try || true
  rm -rf "$FS_WORK"
  return 0
}

_fast_sync_try() {
  local base="$GPN_BASE_PATH"
  local snap_bin="$BIN_DIR/gpn_snapshot"
  local work="$FS_WORK"
  local meta="$work/snapshot.json" vout="$work/verify.txt"
  log "fetching signed snapshot metadata…"
  if ! curl -fsSL --connect-timeout 15 --max-time 60 -o "$meta" "$SNAPSHOT_META_URL" 2>/dev/null; then
    fast_sync_fallback "snapshot metadata not available (404/timeout) at $SNAPSHOT_META_URL"; return 0
  fi
  # Signature first: an unsigned or edited document is never trusted, so the
  # size/hash it advertises is never used.
  if ! "$snap_bin" verify --meta "$meta" > "$vout" 2>&1; then
    fast_sync_fallback "snapshot metadata failed verification: $(tail -n1 "$vout" 2>/dev/null)"; return 0
  fi

  local snap_size snap_block
  snap_size="$(sed -n 's/^size=//p' "$vout" | head -n1)"
  snap_block="$(sed -n 's/^block=//p' "$vout" | head -n1)"
  case "$snap_size" in ''|*[!0-9]*) fast_sync_fallback "signed metadata has no usable size"; return 0 ;; esac

  log "weekly snapshot: block ${snap_block:-?}, $((snap_size/1024/1024)) MiB — downloading…"

  # Disk guard: the archive plus the unpacked DB needs room; require 3x.
  local avail_kb need_kb
  avail_kb="$(df -Pk "$PREFIX" 2>/dev/null | awk 'NR==2{print $4}')"
  need_kb=$(( snap_size * 3 / 1024 ))
  if [ -n "$avail_kb" ] && [ "$avail_kb" -lt "$need_kb" ] 2>/dev/null; then
    fast_sync_fallback "only $((avail_kb/1024)) MiB free, need ~$((need_kb/1024)) MiB (3x snapshot)"; return 0
  fi

  local arch="$work/snap.tar.gz"
  if ! curl -fL --retry 2 --retry-delay 2 --connect-timeout 15 --max-time 3600 \
        --progress-bar -o "$arch" "$SNAPSHOT_URL"; then
    fast_sync_fallback "snapshot download failed"; return 0
  fi
  # Verify the *bytes on disk* against the signed size + sha256.
  if ! "$snap_bin" verify --meta "$meta" --snapshot "$arch" > "$vout" 2>&1; then
    fast_sync_fallback "snapshot failed verification: $(tail -n1 "$vout" 2>/dev/null)"; return 0
  fi

  # Import into a STAGING dir, never over the real base path: a failure then has
  # nothing to undo except our own temp dir.
  local stage="$work/stage"
  log "importing snapshot (archive pruning) — a few minutes…"
  if ! "$snap_bin" import --snapshot "$arch" --base-path "$stage" --pruning archive >>"$work/import.log" 2>&1; then
    fast_sync_fallback "snapshot import failed: $(tail -n1 "$work/import.log" 2>/dev/null)"; return 0
  fi
  if [ ! -d "$stage/chains" ]; then fast_sync_fallback "import produced no chains/ dir"; return 0; fi

  mkdir -p "$base"
  if [ -e "$base/chains" ]; then fast_sync_fallback "$base/chains appeared during import — not overwriting it"; return 0; fi
  mv "$stage/chains" "$base/chains" \
    || { fast_sync_fallback "could not move the imported DB into $base"; return 0; }
  if [ -f "$stage/.gpn-pruning-archive" ]; then
    mv "$stage/.gpn-pruning-archive" "$base/.gpn-pruning-archive" 2>/dev/null || true
  fi

  SNAPSHOT_BOOTSTRAPPED="block ${snap_block:-?}"
  log "bootstrapped from the weekly snapshot at block ${snap_block:-?} — skipping the full peer sync."
  return 0
}

# Install ollama user-local into $PREFIX/ollama (no sudo) and prepare the model
# store at $PREFIX/models. Idempotent: no-op if ollama is already present. This
# keeps the entire AI-compute stack (binaries + models + ollama data) sandboxed
# under ~/.gpn. On unsupported platforms (or a failed download) it only warns —
# mining still works without it, and models can be pulled later with
# `gpn_pull_model`.
install_ollama() {
  if [ -x "$PREFIX/ollama/bin/ollama" ]; then
    log "ollama already installed at $PREFIX/ollama/bin/ollama"
    mkdir -p "$PREFIX/models"
    return 0
  fi

  local os arch
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    *) warn "unsupported OS for ollama auto-install; install ollama manually"; return 0 ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch=amd64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) warn "unsupported arch for ollama auto-install; install ollama manually"; return 0 ;;
  esac

  log "installing ollama (user-local) → $PREFIX/ollama …"
  local url="https://github.com/ollama/ollama/releases/latest/download/ollama-${os}-${arch}.tgz"
  local tgz="$PREFIX/ollama.tgz"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$tgz" || { warn "ollama download failed (skipping ollama)"; rm -f "$tgz"; return 0; }
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$tgz" || { warn "ollama download failed (skipping ollama)"; rm -f "$tgz"; return 0; }
  else
    warn "no curl/wget — cannot download ollama (skipping)"; return 0
  fi

  mkdir -p "$PREFIX/ollama" "$PREFIX/models"
  tar -xzf "$tgz" -C "$PREFIX/ollama"
  rm -f "$tgz"
  log "ollama installed: $PREFIX/ollama/bin/ollama (models → $PREFIX/models)"

  # Guaranteed baseline: pull a starter model set so every node offers a usable
  # LLM from day one (the network always has at least one model available).
  local baseline="${GPN_BASELINE_MODELS:-llama3.2:1b,qwen2.5:0.5b}"
  log "pulling baseline models: $baseline"
  OLLAMA_MODELS="$PREFIX/models" "$PREFIX/ollama/bin/ollama" serve >/dev/null 2>&1 &
  local opid=$!
  sleep 3
  local m
  for m in $(echo "$baseline" | tr ',' ' '); do
    OLLAMA_MODELS="$PREFIX/models" "$PREFIX/ollama/bin/ollama" pull "$m" >/dev/null 2>&1 \
      || warn "baseline pull failed (will retry on provider start): $m"
  done
  kill "$opid" 2>/dev/null || true
  log "baseline models ready"
}

# 1 ── Prerequisites ──────────────────────────────────────────────────────────
log "checking prerequisites…"
command -v cargo >/dev/null 2>&1 || die "Rust/cargo not found — install: https://rustup.rs"
command -v git  >/dev/null 2>&1 || die "git not found"
if [ ! -d /etc/OpenCL/vendors ] && [ ! -d /etc/OpenCL ]; then
  warn "no OpenCL ICD detected — GPU mining will fall back to CPU (slower)."
  warn "  NVIDIA: install nvidia-opencl-icd / reboot. AMD: install rocm-opencl-runtime."
fi

# 2 ── Source ────────────────────────────────────────────────────────────────
mkdir -p "$PREFIX"
if [ ! -d "$SRC/.git" ]; then
  if [ -z "$GPN_REPO_URL" ]; then
    GPN_REPO_URL="$(git -C "$(cd "$(dirname "$0")" && pwd)" remote get-url origin 2>/dev/null || true)"
    [ -n "$GPN_REPO_URL" ] || die "no source and no GPN_REPO_URL — clone the repo and run ./install.sh, or set GPN_REPO_URL"
  fi
  log "cloning GPN source…"
  git clone --depth 1 "$GPN_REPO_URL" "$SRC"
else
  log "updating source…"
  git -C "$SRC" pull --ff-only >/dev/null 2>&1 || warn "couldn't pull (continuing with existing source)"
fi

# 2b ── Referral code (data collection only) ─────────────────────────────────
# Optional, and it must NEVER fail an install: an invalid or absent code is a
# warning and the install proceeds. The file is write-once — a second install
# with a different --ref keeps the first value (no retroactive re-referring).
REF_HELPER="$SRC/scripts/gpn-referral.sh"
[ -f "$REF_HELPER" ] || REF_HELPER="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/scripts/gpn-referral.sh"
if [ -n "$REF_CODE" ]; then
  if [ -f "$REF_HELPER" ] && . "$REF_HELPER" 2>/dev/null; then
    if referral_write_once "$PREFIX" "$REF_CODE"; then
      log "referral recorded: '$(referral_read "$PREFIX" 2>/dev/null)' → $PREFIX/referral (write-once; used for the mainnet testnet-participation allocation)"
    else
      rc=$?
      if [ "$rc" = 2 ]; then
        warn "[referral] already set to '$(referral_read "$PREFIX" 2>/dev/null)'; ignoring new value '$REF_CODE'"
      else
        warn "[referral] invalid code '$REF_CODE': $(referral_validate "$REF_CODE" 2>/dev/null) — continuing WITHOUT a referral"
      fi
    fi
  else
    warn "[referral] helper scripts/gpn-referral.sh not found — skipping referral '$REF_CODE' (install continues)"
  fi
fi

# 3 ── Build ─────────────────────────────────────────────────────────────────
log "building gpn-node + gpn-agent (a few minutes, first run)…"
cargo build --release --manifest-path "$SRC/gpn-node/Cargo.toml" -p gpn-node
cargo build --release --manifest-path "$SRC/gpn-agent/Cargo.toml" --bins

# 4 ── Install binaries ──────────────────────────────────────────────────────
log "installing binaries → $BIN_DIR"
mkdir -p "$BIN_DIR"
ln -sf "$SRC/gpn-node/target/release/gpn-node" "$BIN_DIR/gpn-node"
for b in gpn_node gpn_provider_status gpn_llm_provider gpn_diffusion_provider gpn_gpu_mine gpn_external_miner gpn_gpu_compute gpn_simulate_autoswitch gpn_pull_model gpn_snapshot gpn_seedlist; do
  [ -f "$SRC/gpn-agent/target/release/$b" ] && ln -sf "$SRC/gpn-agent/target/release/$b" "$BIN_DIR/$b"
done

# 4b ── ollama (AI-compute models, REQUIRED to serve LLMs, user-local) ─────────
# A node mines without ollama, but cannot serve LLM jobs until ollama runs (the
# provider calls http://127.0.0.1:11434). This installs it user-local + pulls
# the baseline model(s). Equivalent helpers: scripts/install-ollama.sh (Linux),
# scripts/install-ollama.ps1 (Windows).
install_ollama

# 4c ── Fast sync: import the weekly snapshot (default; opt-out) ───────────────
# Runs BEFORE any node starts, into $GPN_BASE_PATH (what `gpn_node` uses).
# Every failure falls back to a normal full sync — see fast_sync().
SNAPSHOT_BOOTSTRAPPED=""
fast_sync

# 5 ── Wallet (the MONEY identity) ──────────────────────────────────────────
# One wallet per machine, in the node's own keystore (`<base>/keystore.json`,
# 0600). `gpn_node --ensure-wallet` creates it once and reuses it forever; it
# never ships a key. Two nodes with the same account split/steal each other's
# rewards, so there is deliberately NO pinned signer here (a fleet script may
# pass `--signer-uri` / GPN_SIGNER on purpose — that is an explicit override).
GPN_BASE_PATH="$PREFIX/node"
WALLET=""
if GPN_BASE_PATH="$GPN_BASE_PATH" "$BIN_DIR/gpn_node" --ensure-wallet > "$PREFIX/wallet.setup" 2>&1; then
  WALLET="$(sed -n 's/.*payout account: \([1-9A-HJ-NP-Za-km-z]\{47,48\}\).*/\1/p' "$PREFIX/wallet.setup" | head -1)"
else
  cat "$PREFIX/wallet.setup" >&2
  warn "could not set up this machine's wallet — fix it before starting the node"
fi
# `$PREFIX/wallet` used to hold a randomly generated keypair that nothing read
# (gpn_node never used it — it kept a second wallet in the base dir). Keep the
# path as a NON-SECRET pointer so old instructions still lead somewhere.
{
  echo "This node's payout (money) identity, created on this machine."
  echo "payout account: ${WALLET:-unknown}"
  echo "keystore:       $GPN_BASE_PATH/keystore.json   (secret; owner-only 0600)"
  echo "back it up offline. Never copy it to another node."
} > "$PREFIX/wallet"
chmod 600 "$PREFIX/wallet"

# 6 ── Config ────────────────────────────────────────────────────────────────
log "writing config → $PREFIX/gpn.env"
cat > "$PREFIX/gpn.env" <<EOF
# GPN miner config
GPN_NETWORK=$NETWORK
GPN_ENDPOINT=wss://testnet.gpn.example:9944   # TODO: replace with the real public endpoint
GPN_BASE_PATH=$GPN_BASE_PATH
GPN_SIGNER_FILE=$GPN_BASE_PATH/keystore.json
GPN_DATA=$PREFIX/data

# ollama (AI-compute) — user-local, sandboxed under $PREFIX
OLLAMA_BIN=$PREFIX/ollama/bin/ollama
OLLAMA_MODELS=$PREFIX/models

# Referral (data collection only; write-once, so there is deliberately no
# setting here — the value lives in $PREFIX/referral and must not be rewritable).
EOF

# 7 ── Done ──────────────────────────────────────────────────────────────────
log "installed. payout account: ${WALLET:-<see $PREFIX/wallet.setup>}"
# Say plainly where the chain data came from — no ambiguity for the operator.
if [ -n "$SNAPSHOT_BOOTSTRAPPED" ]; then
  log "chain data: bootstrapped from the weekly snapshot ($SNAPSHOT_BOOTSTRAPPED) — the node only has to catch up the last few blocks."
else
  log "chain data: none yet — the node will full-sync from peers on first start (hours)."
fi
echo
echo "Start earning — three steps, one question:"
echo "  gpn_node --setup     # generate a new wallet, or import an existing one"
echo "  gpn_node             # one command: miner + AI providers, supervised"
echo "  (a GPN node always mines AND serves AI — no role option; config + env reference: docs/run-a-node.md)"
echo ""
echo "The chain spec, data folder, payout address, peers and public address are automatic."
echo "Wallet: $GPN_BASE_PATH/keystore.json (secret, 0600) — back it up; never copy it to another node."
echo
echo "Or run just the chain node/miner directly (mines by default):"
echo "  gpn-node --chain $NETWORK --base-path \"$PREFIX/data\""
echo ""
echo "ollama (REQUIRED to serve LLM jobs; mining works without it):"
echo "  installed user-local → $PREFIX/ollama/bin/ollama  (models in $PREFIX/models)"
echo "  keep it running:  OLLAMA_MODELS=$PREFIX/models $PREFIX/ollama/bin/ollama serve &"
echo "  re-run/repair:    ./scripts/install-ollama.sh"
echo "  check:            curl -s http://127.0.0.1:11434/api/tags"
echo
echo "Lighter option — mine against a remote node (positional args):"
echo "  gpn_external_miner <rpc_url> [device_index] [batch] [poll_ms]"
echo
echo "AI-compute models (requires ollama; see above):"
echo "  gpn_pull_model pull llama3.2:1b"
echo "  gpn_pull_model gguf TheBloke/Llama-2-7B-GGUF llama-2-7b.Q4_K_M.gguf"
echo
echo "Config/env: $PREFIX/gpn.env   ·   keystore (secret): $GPN_BASE_PATH/keystore.json"
