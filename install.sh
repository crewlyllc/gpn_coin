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

set -euo pipefail

GPN_REPO_URL="${GPN_REPO_URL:-}"
PREFIX="${GPN_PREFIX:-$HOME/.gpn}"
BIN_DIR="${GPN_BIN:-$HOME/.local/bin}"
NETWORK="${GPN_NETWORK:-testnet}"
SRC="$PREFIX/src"

log() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33m !\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

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

# 3 ── Build ─────────────────────────────────────────────────────────────────
log "building gpn-node + gpn-agent (a few minutes, first run)…"
cargo build --release --manifest-path "$SRC/gpn-node/Cargo.toml" -p gpn-node
cargo build --release --manifest-path "$SRC/gpn-agent/Cargo.toml" --bins

# 4 ── Install binaries ──────────────────────────────────────────────────────
log "installing binaries → $BIN_DIR"
mkdir -p "$BIN_DIR"
ln -sf "$SRC/gpn-node/target/release/gpn-node" "$BIN_DIR/gpn-node"
for b in gpn_node gpn_provider_status gpn_llm_provider gpn_diffusion_provider gpn_gpu_mine gpn_external_miner gpn_gpu_compute gpn_simulate_autoswitch gpn_pull_model gpn_snapshot; do
  [ -f "$SRC/gpn-agent/target/release/$b" ] && ln -sf "$SRC/gpn-agent/target/release/$b" "$BIN_DIR/$b"
done

# 4b ── ollama (AI-compute models, user-local) ────────────────────────────────
install_ollama

# 5 ── Wallet ────────────────────────────────────────────────────────────────
if [ ! -f "$PREFIX/wallet" ]; then
  log "generating wallet…"
  "$BIN_DIR/gpn-node" key generate > "$PREFIX/wallet"
fi
WALLET="$(grep -oE 'SS58 Address:[[:space:]]*[A-Za-z0-9]+' "$PREFIX/wallet" | awk '{print $3}')"
chmod 600 "$PREFIX/wallet"

# 6 ── Config ────────────────────────────────────────────────────────────────
log "writing config → $PREFIX/gpn.env"
cat > "$PREFIX/gpn.env" <<EOF
# GPN miner config
GPN_NETWORK=$NETWORK
GPN_ENDPOINT=wss://testnet.gpn.example:9944   # TODO: replace with the real public endpoint
GPN_WALLET=$WALLET
GPN_DATA=$PREFIX/data

# ollama (AI-compute) — user-local, sandboxed under $PREFIX
OLLAMA_BIN=$PREFIX/ollama/bin/ollama
OLLAMA_MODELS=$PREFIX/models
EOF

# 7 ── Done ──────────────────────────────────────────────────────────────────
log "installed. wallet: $WALLET"
echo
echo "Start earning (ONE command — supervised node: miner + AI provider together):"
echo "  GPN_SIGNER=<your-key> GPN_AUTHOR=alice gpn_node"
echo "  (a GPN node always mines AND serves AI — no role option; config + env reference: docs/run-a-node.md)"
echo
echo "Or run just the chain node/miner directly:"
echo "  gpn-node --chain $NETWORK --force-authoring --base-path \"$PREFIX/data\""
echo
echo "Lighter option — mine against a remote node (positional args):"
echo "  gpn_external_miner <rpc_url> [device_index] [batch] [poll_ms]"
echo
echo "AI-compute models (requires ollama; see above):"
echo "  gpn_pull_model pull llama3.2:1b"
echo "  gpn_pull_model gguf TheBloke/Llama-2-7B-GGUF llama-2-7b.Q4_K_M.gguf"
echo
echo "Config/env: $PREFIX/gpn.env   ·   wallet (secret): $PREFIX/wallet"
