#!/usr/bin/env bash
# gpn-autostart.sh — make a GPN node (and its ollama) survive a reboot.
#
# WHY THIS EXISTS (2026-11-01)
#   The release tarball ships binaries. Extracting it and running `gpn_node`
#   starts one node — for as long as that shell lives. After a reboot nothing
#   starts: there is no unit, no timer, no lingering user session. `gpn_node`
#   also REFUSES TO START without ollama (exit 69, `node=down(no ollama)`), so
#   ollama has to come back too, or the pair never comes up.
#
#   User units alone are NOT enough: a `systemctl --user` service only starts
#   when the user logs in, and on a headless mining box nobody ever does.
#   `loginctl enable-linger $USER` is what makes the user manager start at
#   boot. That is the whole point of this script.
#
# Usage:
#   gpn-autostart.sh install [--prefix DIR] [--bin-dir DIR]
#   gpn-autostart.sh remove
#   gpn-autostart.sh status
#
# Env overrides:
#   GPN_PREFIX         install dir      (default ~/.gpn)
#   GPN_BIN            bin dir          (default ~/.local/bin)
#   GPN_AP_URL         access point     (default: canonical testnet AP)
#   GPN_BOOTNODES      bootstrap peers  (default: canonical 3)
#   GPN_AUTOSTART_DIR  unit dir         (default ~/.config/systemd/user)
#   GPN_NO_LINGER=1    do not enable-linger (containers / CI; node then only
#                      starts at login — say so out loud)
#
# Safety:
#   * idempotent — install twice == install once (unit files are rewritten,
#     never duplicated; there is exactly one gpn-node.service and one
#     gpn-ollama.service)
#   * remove deletes ONLY the two units this script owns plus the linger bit,
#     and never touches $PREFIX/node (chain DB + keystore) or $PREFIX/models
#   * every systemctl/loginctl call is non-fatal with an explicit warning, so a
#     broken/absent systemd never fails an install that otherwise worked
set -euo pipefail

PREFIX="${GPN_PREFIX:-$HOME/.gpn}"
BIN_DIR="${GPN_BIN:-$HOME/.local/bin}"
UNIT_DIR="${GPN_AUTOSTART_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}"
NODE_UNIT="gpn-node.service"
OLLAMA_UNIT="gpn-ollama.service"
MARKER="$PREFIX/autostart"

GPN_AP_URL="${GPN_AP_URL-http://gpn-testnet.duckdns.org:8092}"
GPN_BOOTNODES="${GPN_BOOTNODES:-/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/23.241.109.255/tcp/30333/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz}"

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m !\033[0m %s\n' "$*" >&2; }

have() { command -v "$1" >/dev/null 2>&1; }

# Never let a missing/blocked systemd turn a good install into a failed one.
sc() {
  if have systemctl; then
    systemctl --user "$@" || warn "systemctl --user $* failed (rc=$?) — the node may not autostart"
  else
    warn "systemctl not found — cannot $* (unit written; enable it when systemd is available)"
  fi
}

# `loginctl enable-linger` is the difference between "starts at login" and
# "starts at boot". Without it a headless box stays down after a reboot.
enable_linger() {
  if [ "${GPN_NO_LINGER:-0}" = 1 ]; then
    warn "GPN_NO_LINGER=1 — autostart will only happen at LOGIN, not at boot."
    return 0
  fi
  if have loginctl; then
    if loginctl enable-linger "$(id -un)" 2>/dev/null; then
      log "linger enabled for $(id -un) — user services start at boot, no login needed"
    else
      warn "could not enable linger (need polkit/admin). Run: sudo loginctl enable-linger $(id -un)"
      warn "  without it your node only starts when you log in."
    fi
  else
    warn "loginctl not found — user services will only start at LOGIN, not at boot."
  fi
}

write_units() {
  mkdir -p "$UNIT_DIR" "$PREFIX"

  # Never write a unit that points at a binary that is not there: systemd would
  # fail at boot (and the failure is invisible on a headless box).
  if [ ! -x "$BIN_DIR/gpn_node" ]; then
    warn "no gpn_node at $BIN_DIR — NOT writing $NODE_UNIT (nothing to autostart)"
    return 1
  fi
  # ollama first: gpn_node refuses to start without it (exit 69).
  if [ -x "$PREFIX/ollama/bin/ollama" ]; then
    cat > "$UNIT_DIR/$OLLAMA_UNIT" <<EOF
# Written by gpn-autostart.sh — do not edit; re-run install to regenerate.
[Unit]
Description=GPN ollama (local LLM server for the GPN node)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=OLLAMA_MODELS=$PREFIX/models
Environment=OLLAMA_HOST=127.0.0.1:11434
ExecStart=$PREFIX/ollama/bin/ollama serve
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
EOF
    chmod 644 "$UNIT_DIR/$OLLAMA_UNIT"
  else
    warn "no ollama at $PREFIX/ollama/bin/ollama — skipping $OLLAMA_UNIT"
    warn "  gpn_node REFUSES TO START without ollama; run scripts/install-ollama.sh first."
  fi

  cat > "$UNIT_DIR/$NODE_UNIT" <<EOF
# Written by gpn-autostart.sh — do not edit; re-run install to regenerate.
[Unit]
Description=GPN node (miner + AI provider supervisor)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
WorkingDirectory=$PREFIX
Environment=GPN_BASE_PATH=$PREFIX/node
Environment=GPN_AP_URL=$GPN_AP_URL
Environment=GPN_BOOTNODES=$GPN_BOOTNODES
Environment=GPN_OLLAMA=http://127.0.0.1:11434
Environment=OLLAMA_MODELS=$PREFIX/models
Environment=GPN_GUI=1
ExecStart=$BIN_DIR/gpn_node
Restart=always
RestartSec=10
KillMode=mixed
TimeoutStopSec=30

[Install]
WantedBy=default.target
EOF
  chmod 644 "$UNIT_DIR/$NODE_UNIT"

  # Marker: what we created, so `remove` (and the tests) know exactly what is
  # ours and nothing else is touched.
  {
    echo "mechanism=systemd-user"
    echo "unit_dir=$UNIT_DIR"
    echo "units=$OLLAMA_UNIT $NODE_UNIT"
    echo "linger=loginctl enable-linger $(id -un)"
  } > "$MARKER"
}

cmd_install() {
  write_units || return 1
  sc daemon-reload
  if [ -f "$UNIT_DIR/$OLLAMA_UNIT" ]; then
    sc enable "$OLLAMA_UNIT"
  fi
  sc enable "$NODE_UNIT"
  enable_linger
  log "autostart installed: $UNIT_DIR/{$OLLAMA_UNIT,$NODE_UNIT}"
  log "  start now:  systemctl --user start $NODE_UNIT"
  log "  logs:       journalctl --user -u $NODE_UNIT -f"
  log "  undo:       ./uninstall.sh --remove-autostart"
}

cmd_remove() {
  local unit
  for unit in "$NODE_UNIT" "$OLLAMA_UNIT"; do
    if [ -f "$UNIT_DIR/$unit" ]; then
      sc disable --now "$unit" || true
    fi
  done
  sc daemon-reload
  for unit in "$NODE_UNIT" "$OLLAMA_UNIT"; do
    rm -f "$UNIT_DIR/$unit"
  done
  if have loginctl; then
    loginctl disable-linger "$(id -un)" 2>/dev/null \
      && log "linger disabled for $(id -un)" \
      || warn "could not disable linger — run: sudo loginctl disable-linger $(id -un)"
  fi
  rm -f "$MARKER"
  log "autostart removed (units + linger). Nothing else was touched."
  log "  kept: $PREFIX/node (chain DB + keystore), $PREFIX/models, $BIN_DIR"
}

cmd_status() {
  local unit
  for unit in "$NODE_UNIT" "$OLLAMA_UNIT"; do
    if [ -f "$UNIT_DIR/$unit" ]; then
      printf '%s: installed\n' "$unit"
      if have systemctl; then
        printf '  enabled: %s\n' "$(systemctl --user is-enabled "$unit" 2>/dev/null || echo unknown)"
        printf '  active:  %s\n' "$(systemctl --user is-active  "$unit" 2>/dev/null || echo unknown)"
      fi
    else
      printf '%s: NOT installed\n' "$unit"
    fi
  done
}

ACTION="${1:-}"
shift || true
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="$2"; MARKER="$PREFIX/autostart"; shift 2 ;;
    --prefix=*) PREFIX="${1#--prefix=}"; MARKER="$PREFIX/autostart"; shift ;;
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --bin-dir=*) BIN_DIR="${1#--bin-dir=}"; shift ;;
    -h|--help) sed -n '2,/^set -euo pipefail/p' "$0" | sed '$d'; exit 0 ;;
    *) warn "ignoring unknown argument: $1"; shift ;;
  esac
done

case "$ACTION" in
  install) cmd_install ;;
  remove)  cmd_remove ;;
  status)  cmd_status ;;
  *) sed -n '2,/^set -euo pipefail/p' "$0" | sed '$d'; exit 2 ;;
esac
