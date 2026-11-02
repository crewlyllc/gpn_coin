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
#   GPN_UPDATE_RESTART_CMD  command the 6-hourly updater runs after a swap
#                      (default `systemctl --user restart gpn-node.service`)
#   GPN_NO_LINGER=1    do not enable-linger (containers / CI; node then only
#                      starts at login — say so out loud)
#
# Units written: gpn-node.service, gpn-ollama.service, gpn-update.service and
# gpn-update.timer (a signed-manifest check every 6 h, ±30 min).
#
# Safety:
#   * idempotent — install twice == install once (unit files are rewritten,
#     never duplicated; there is exactly one gpn-node.service and one
#     gpn-ollama.service)
#   * remove deletes ONLY the four units this script owns plus the linger bit,
#     and never touches $PREFIX/node (chain DB + keystore) or $PREFIX/models
#   * every systemctl/loginctl call is non-fatal with an explicit warning, so a
#     broken/absent systemd never fails an install that otherwise worked
set -euo pipefail

PREFIX="${GPN_PREFIX:-$HOME/.gpn}"
BIN_DIR="${GPN_BIN:-$HOME/.local/bin}"
UNIT_DIR="${GPN_AUTOSTART_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}"
NODE_UNIT="gpn-node.service"
OLLAMA_UNIT="gpn-ollama.service"
UPDATE_UNIT="gpn-update.service"
UPDATE_TIMER="gpn-update.timer"
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

  # ── updater ──────────────────────────────────────────────────────────────
  # Auto-update lives in gpn_node for installer deployments, but a host that
  # runs `gpn_fleet`, a hand-started node, or anything else would never check
  # the signed manifest and would freeze on its tarball. gpn_update is the
  # deployment-mode-independent check; the timer runs it every 6 h.
  #
  # Restart hook: this script wrote $NODE_UNIT, so restarting it is not a guess.
  # It can be overridden (e.g. `systemctl --user restart gpn-fleet`) without
  # editing the unit: GPN_UPDATE_RESTART_CMD=... ./scripts/gpn-autostart.sh install
  if [ ! -x "$BIN_DIR/gpn_update" ]; then
    warn "no gpn_update at $BIN_DIR — NOT writing $UPDATE_UNIT (nothing to run)"
  else
    local restart_cmd="${GPN_UPDATE_RESTART_CMD:-systemctl --user restart $NODE_UNIT}"
    cat > "$UNIT_DIR/$UPDATE_UNIT" <<EOF
# Written by gpn-autostart.sh — do not edit; re-run install to regenerate.
[Unit]
Description=GPN signed-release updater (one check + apply)
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
Environment=GPN_UPDATE_DIR=$PREFIX/update
Environment=GPN_UPDATE_TARGET_DIR=$BIN_DIR
Environment="GPN_UPDATE_RESTART_CMD=$restart_cmd"
Nice=10
# 0 = applied/up to date, 2 = manifest unreachable (offline), 3 = newer release
# exists but GPN_AUTO_UPDATE=0 refused it. Only a real error (1) fails the unit.
SuccessExitStatus=0 2 3
ExecStart=$BIN_DIR/gpn_update --apply --quiet

[Install]
WantedBy=default.target
EOF
    chmod 644 "$UNIT_DIR/$UPDATE_UNIT"

    cat > "$UNIT_DIR/$UPDATE_TIMER" <<EOF
# Written by gpn-autostart.sh — do not edit; re-run install to regenerate.
[Unit]
Description=Check for GPN releases every 6 hours (randomized across the fleet)

[Timer]
OnBootSec=10min
OnUnitActiveSec=6h
# A fleet that installs together must not download 300 MB together.
RandomizedDelaySec=30min
AccuracySec=1min
Persistent=true
Unit=$UPDATE_UNIT

[Install]
WantedBy=timers.target
EOF
    chmod 644 "$UNIT_DIR/$UPDATE_TIMER"
  fi

  # Marker: what we created, so `remove` (and the tests) know exactly what is
  # ours and nothing else is touched.
  {
    echo "mechanism=systemd-user"
    echo "unit_dir=$UNIT_DIR"
    echo "units=$OLLAMA_UNIT $NODE_UNIT $UPDATE_UNIT $UPDATE_TIMER"
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
  # The timer is what keeps an out-of-date box from staying out of date. It does
  # not download during install: the first run is OnBootSec=10min away.
  if [ -f "$UNIT_DIR/$UPDATE_TIMER" ]; then
    sc enable --now "$UPDATE_TIMER"
  fi
  enable_linger
  log "autostart installed: $UNIT_DIR/{$OLLAMA_UNIT,$NODE_UNIT,$UPDATE_UNIT,$UPDATE_TIMER}"
  log "  start now:  systemctl --user start $NODE_UNIT"
  log "  logs:       journalctl --user -u $NODE_UNIT -f"
  log "  updater:    systemctl --user list-timers $UPDATE_TIMER   (first check in 10 min)"
  log "  undo:       ./uninstall.sh --remove-autostart"
}

cmd_remove() {
  local unit
  for unit in "$UPDATE_TIMER" "$UPDATE_UNIT" "$NODE_UNIT" "$OLLAMA_UNIT"; do
    if [ -f "$UNIT_DIR/$unit" ]; then
      sc disable --now "$unit" || true
    fi
  done
  sc daemon-reload
  for unit in "$UPDATE_TIMER" "$UPDATE_UNIT" "$NODE_UNIT" "$OLLAMA_UNIT"; do
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
  for unit in "$NODE_UNIT" "$OLLAMA_UNIT" "$UPDATE_UNIT" "$UPDATE_TIMER"; do
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
