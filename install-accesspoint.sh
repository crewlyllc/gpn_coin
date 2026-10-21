#!/usr/bin/env bash
# install-accesspoint.sh — run a GPN **access point** on your own machine, in one command.
#
# An access point is a public job API (gpn_scheduler): a caller hits ONE URL and it
# routes each job to the best provider (least-ms idle node). Anyone can run one —
# it's permissionless and needs no special trust, so running several is the
# decentralized front door to the network.
#
# Usage:
#   ./install-accesspoint.sh                                   # build from a local clone
#   GPN_REPO_URL=<git-url> ./install-accesspoint.sh            # build from a fresh clone
#   GPN_PROVIDERS="http://a:8093,http://b:8093" ./install-accesspoint.sh
#   GPN_SIGNER_URI="//Alice" ./install-accesspoint.sh          # testnet free-AI (hot wallet)
#
# Env:
#   GPN_PREFIX      install dir            (default ~/.gpn)
#   GPN_BIN         bin dir                (default ~/.local/bin)
#   GPN_ENDPOINT    chain RPC endpoint     (default ws://127.0.0.1:9944)
#   GPN_LISTEN      listen address         (default 0.0.0.0:8092)
#   GPN_PROVIDERS   provider /status URLs  (comma-separated; default http://127.0.0.1:8093)
#   GPN_SIGNER_URI  hot-wallet seed for testnet free-AI mode (omit for mainnet client-signed)
#   GPN_REPO_URL    git URL if there's no local clone
#   GPN_INSTALL_SERVICE 1 to also install+start a systemd --user unit (default 0)
#   GPN_AP_TOKEN    bearer token for non-loopback callers; omit and the first
#                   boot generates one (48 hex chars) into $PREFIX/accesspoint.env
#                   and prints it once — copy it wherever your callers live
#                   (e.g. the Cloudflare Worker's AP_TOKEN secret).
set -euo pipefail

PREFIX="${GPN_PREFIX:-$HOME/.gpn}"
BIN_DIR="${GPN_BIN:-$HOME/.local/bin}"
ENDPOINT="${GPN_ENDPOINT:-ws://127.0.0.1:9944}"
LISTEN="${GPN_LISTEN:-0.0.0.0:8092}"
PROVIDERS="${GPN_PROVIDERS:-http://127.0.0.1:8093}"
SIGNER_URI="${GPN_SIGNER_URI:-}"
REPO_URL="${GPN_REPO_URL:-}"
SRC="$PREFIX/src"

log() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33m !\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

# 1 ── Prerequisites ──────────────────────────────────────────────────────────
command -v cargo >/dev/null 2>&1 || die "Rust/cargo not found — install: https://rustup.rs"
command -v git   >/dev/null 2>&1 || die "git not found"

# 2 ── Source ────────────────────────────────────────────────────────────────
mkdir -p "$PREFIX"
if [ ! -d "$SRC/.git" ]; then
  if [ -z "$REPO_URL" ]; then
    REPO_URL="$(git -C "$(cd "$(dirname "$0")" && pwd)" remote get-url origin 2>/dev/null || true)"
    [ -n "$REPO_URL" ] || die "no source and no GPN_REPO_URL — clone the repo and run ./install-accesspoint.sh, or set GPN_REPO_URL"
  fi
  log "cloning GPN source…"; git clone --depth 1 "$REPO_URL" "$SRC"
else
  log "updating source…"; git -C "$SRC" pull --ff-only >/dev/null 2>&1 || warn "couldn't pull (continuing)"
fi

# 3 ── Build the access-point binary ─────────────────────────────────────────
log "building gpn_scheduler (access point)…"
cargo build --release --manifest-path "$SRC/gpn-agent/Cargo.toml" --bin gpn_scheduler

# 4 ── Install ───────────────────────────────────────────────────────────────
mkdir -p "$BIN_DIR"
install -m755 "$SRC/gpn-agent/target/release/gpn_scheduler" "$BIN_DIR/gpn_scheduler"
ln -sf "$BIN_DIR/gpn_scheduler" "$BIN_DIR/gpn-accesspoint"

# 5 ── Config ────────────────────────────────────────────────────────────────
cat > "$PREFIX/accesspoint.env" <<EOF
# GPN access-point config
GPN_ENDPOINT=$ENDPOINT
GPN_LISTEN=$LISTEN
GPN_PROVIDERS=$PROVIDERS
$( [ -n "$SIGNER_URI" ] && echo "GPN_SIGNER_URI=$SIGNER_URI" || echo "# GPN_SIGNER_URI=   # set for testnet free-AI (hot wallet); omit for mainnet client-signed" )
# Optional: a public base URL to advertise (so clients can find this AP). 
# GPN_PUBLIC_URL=http://your-host:8092
# GPN_AP_TOKEN is generated on the first boot (and printed once). Leave it
# unset here unless you want to supply your own; retrieve it later with:
#   grep GPN_AP_TOKEN ~/.gpn/accesspoint.env
EOF
chmod 600 "$PREFIX/accesspoint.env"

# 6 ── Run helper ────────────────────────────────────────────────────────────
cat > "$BIN_DIR/gpn-accesspoint-run" <<EOF
#!/usr/bin/env bash
set -a; . "$PREFIX/accesspoint.env"; set +a
ARGS=(--listen "\$GPN_LISTEN" --endpoint "\$GPN_ENDPOINT" --providers "\$GPN_PROVIDERS")
[ -n "\${GPN_SIGNER_URI:-}" ] && ARGS+=(--signer-uri "\$GPN_SIGNER_URI")
exec "$BIN_DIR/gpn_scheduler" "\${ARGS[@]}"
EOF
chmod 755 "$BIN_DIR/gpn-accesspoint-run"

# 7 ── Optional systemd --user unit ──────────────────────────────────────────
if [ "${GPN_INSTALL_SERVICE:-0}" = "1" ] && command -v systemctl >/dev/null 2>&1; then
  mkdir -p "$HOME/.config/systemd/user"
  cat > "$HOME/.config/systemd/user/gpn-accesspoint.service" <<EOF
[Unit]
Description=GPN access point (public job API + scheduler)
After=network-online.target

[Service]
ExecStart=$BIN_DIR/gpn-accesspoint-run
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
EOF
  systemctl --user daemon-reload
  systemctl --user enable --now gpn-accesspoint.service
  log "systemd --user unit installed + started (gpn-accesspoint.service)"
fi

# 8 ── Done ──────────────────────────────────────────────────────────────────
log "access point installed."
echo
echo "Run it now:      gpn-accesspoint-run"
echo "  (or with systemd: GPN_INSTALL_SERVICE=1 ./install-accesspoint.sh)"
echo
echo "Config:  $PREFIX/accesspoint.env"
echo "Test:    curl http://127.0.0.1:${LISTEN##*:}/health"
echo
echo "Auth:    the first boot generates GPN_AP_TOKEN (48 hex chars) into"
echo "         $PREFIX/accesspoint.env and prints it ONCE. Copy it to every"
echo "         non-loopback caller (the gpncoin.ai Worker uses it as AP_TOKEN):"
echo "           ./scripts/set-ap-token.sh          # sets the Worker's AP_TOKEN secret"
echo "         Retrieve it any time with:"
echo "           grep GPN_AP_TOKEN $PREFIX/accesspoint.env"
echo
echo "⚠️  To be reachable by others, expose the listen port (default 8092) and share"
echo "    http://<your-host>:8092 as an access point. The scheduler routes jobs to the"
echo "    providers in GPN_PROVIDERS (add more as the network grows)."
