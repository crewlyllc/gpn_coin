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
for b in gpn_gpu_mine gpn_external_miner gpn_gpu_compute gpn_simulate_autoswitch; do
  [ -f "$SRC/gpn-agent/target/release/$b" ] && ln -sf "$SRC/gpn-agent/target/release/$b" "$BIN_DIR/$b"
done

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
EOF

# 7 ── Done ──────────────────────────────────────────────────────────────────
log "installed. wallet: $WALLET"
echo
echo "Start earning (full node — self-mines on GPU, serves AI compute):"
echo "  gpn-node --chain $NETWORK --force-authoring --base-path \"$PREFIX/data\""
echo
echo "Lighter option — mine against a remote node (positional args):"
echo "  gpn_external_miner <rpc_url> [device_index] [batch] [poll_ms]"
echo
echo "Config/env: $PREFIX/gpn.env   ·   wallet (secret): $PREFIX/wallet"
