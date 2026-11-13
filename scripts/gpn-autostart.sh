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
#   gpn-autostart.sh install-updater [--prefix DIR] [--bin-dir DIR]
#       only gpn-update.service + gpn-update.timer — for a host whose node is
#       started some other way (gpn_fleet.service, a shell, a container)
#   gpn-autostart.sh remove
#   gpn-autostart.sh remove-updater
#   gpn-autostart.sh status
#   gpn-autostart.sh verify
#       prove the boot path end-to-end: the unit is enabled, linger is on,
#       and a stop/start cycle (a simulated reboot) brings the node back with
#       no user action. Exits non-zero if any check fails.
#
# Env overrides:
#   GPN_PREFIX         install dir      (default ~/.gpn)
#   GPN_BIN            bin dir          (default ~/.local/bin)
#   GPN_AP_URL         access point     (default: canonical testnet AP)
#   GPN_BOOTNODES      bootstrap peers  (default: canonical 3)
#   GPN_AUTOSTART_DIR  unit dir         (default ~/.config/systemd/user)
#   GPN_UPDATE_RESTART_CMD  command the hourly updater runs after a swap
#                      (default: restart the supervisor, then `try-restart` every
#                      other gpn-* unit whose ExecStart is under the bin dir)
#   GPN_NO_LINGER=1    do not enable-linger (containers / CI; node then only
#                      starts at login — say so out loud)
#
# Units written: gpn-node.service, gpn-ollama.service, gpn-update.service and
# gpn-update.timer (a signed-manifest check every hour, ±10 min — temporary
# review cadence, 2026-11-12).
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
NOTIFY_UNIT="gpn-notify@.service"
UPDATE_UNIT="gpn-update.service"
UPDATE_TIMER="gpn-update.timer"
MARKER="$PREFIX/autostart"

GPN_AP_URL="${GPN_AP_URL-http://gpn-testnet.duckdns.org:8092}"
GPN_BOOTNODES="${GPN_BOOTNODES:-/dns4/gpn-testnet.duckdns.org/tcp/30333/ws/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/142.129.54.88/tcp/30333/ws/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/23.241.109.255/tcp/30333/ws/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz}"

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

# Write gpn-update.service + gpn-update.timer. Shared by `install` (which also
# manages node/ollama) and `install-updater` (which must NOT touch them: on a
# host whose node is started by gpn_fleet.service, enabling a second
# gpn-node.service would run two supervisors against one base path).
write_update_units() {
  local default_restart="$1"
  mkdir -p "$UNIT_DIR" "$PREFIX"

  # Auto-update lives in gpn_node for installer deployments, but a host that
  # runs `gpn_fleet`, a hand-started node, or anything else would never check
  # the signed manifest and would freeze on its tarball. gpn_update is the
  # deployment-mode-independent check; the timer runs it every hour.
  #
  # Restart hook: never guessed by the binary. Here the default is the unit the
  # caller owns; GPN_UPDATE_RESTART_CMD overrides it, and an empty default
  # leaves it unset (the updater then prints the command instead).
  if [ ! -x "$BIN_DIR/gpn_update" ]; then
    warn "no gpn_update at $BIN_DIR — NOT writing $UPDATE_UNIT (nothing to run)"
    warn "  it ships in every release bundle; copy it next to gpn_node first."
    return 1
  fi

  local restart_cmd="${GPN_UPDATE_RESTART_CMD-$default_restart}"
  local restart_line=""
  if [ -n "$restart_cmd" ]; then
    restart_line="Environment=\"GPN_UPDATE_RESTART_CMD=$restart_cmd\""
  fi

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
$restart_line
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
Description=Check for GPN releases every hour (temporary review cadence, randomized across the fleet)

[Timer]
OnBootSec=10min
# TEMPORARY hourly cadence (review window, 2026-11-12); reverts together with
# DEFAULT_CHECK_HOURS in gpn-agent/src/update.rs.
OnUnitActiveSec=1h
# A fleet that installs together must not download 300 MB together.
RandomizedDelaySec=30min
AccuracySec=1min
Persistent=true
Unit=$UPDATE_UNIT

[Install]
WantedBy=timers.target
EOF
  chmod 644 "$UNIT_DIR/$UPDATE_TIMER"
  return 0
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
OnFailure=gpn-notify@%n.service

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
# A fatal start (exit 69 no-ollama, 78 not-dialable, 3 bad key) is explained
# by gpn_node itself in ~/.gpn/last-exit.json; this hook makes sure a human
# sees it even when the journal is the only other place it landed.
OnFailure=gpn-notify@%n.service

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

  # The failure notifier. It is deliberately best-effort: a headless box has
  # no desktop, and a missing notify-send must never fail anything. XDG_SESSION_ID
  # is inherited from the user manager when a login session exists; DISPLAY /
  # WAYLAND_DISPLAY is checked too, because a `systemctl --user` unit does not
  # always inherit the graphical session environment.
  cat > "$UNIT_DIR/$NOTIFY_UNIT" <<'EOF'
# Written by gpn-autostart.sh — do not edit; re-run install to regenerate.
[Unit]
Description=GPN: report why %i stopped

[Service]
Type=oneshot
# Keep this line readable in journalctl and syslog. Nothing here is fatal.
ExecStart=/bin/sh -c 'msg="GPN unit %i stopped: run gpn_node --diagnose (journal: journalctl --user -u %i)"; logger -t gpn-node -- "$msg" || true; if { [ -n "$XDG_SESSION_ID" ] || [ -n "$DISPLAY" ] || [ -n "$WAYLAND_DISPLAY" ]; } && command -v notify-send >/dev/null 2>&1; then notify-send -u critical "GPN node stopped" "$msg" || true; fi; exit 0'
EOF
  chmod 644 "$UNIT_DIR/$NOTIFY_UNIT"

  write_update_units "$NODE_UNIT"

  # Marker: what we created, so `remove` (and the tests) know exactly what is
  # ours and nothing else is touched.
  {
    echo "mechanism=systemd-user"
    echo "unit_dir=$UNIT_DIR"
    echo "units=$NOTIFY_UNIT $OLLAMA_UNIT $NODE_UNIT $UPDATE_UNIT $UPDATE_TIMER"
    echo "linger=loginctl enable-linger $(id -un)"
  } > "$MARKER"
}

# Units that execute something the updater is about to replace.
#
# WHY THIS EXISTS (2026-11-12). The hook used to name ONE unit — the supervisor
# (gpn-fleet, else gpn-node) — on the reasoning that restarting the supervisor
# restarts the stack. That is true on Windows, where the launcher spawns every
# provider as a child, and false on a Linux host whose provider stack is a set
# of SIBLING units: fleet came back on the new build while accesspoint,
# provider, heartbeat, diffusion, finetune, oracle and faucet kept executing the
# deleted 0.3.42 image out of a 0.3.43 install dir — for a day, with nothing in
# any journal saying so. A file on disk and the code a process runs are two
# different facts; only a restart changes the second one.
#
# A unit qualifies when its ExecStart points into the bin dir (a shipped binary)
# or into the prefix (a launcher script that spawns those binaries, e.g.
# gpn-fleet). Aliases are skipped: gpn-scheduler.service is a symlink to
# gpn-accesspoint.service, and naming both would restart the same unit twice.
units_that_run_installed_code() {
  local u name line
  for u in "$UNIT_DIR"/gpn-*.service; do
    [ -f "$u" ] || continue
    [ -L "$u" ] && continue
    name="$(basename "$u")"
    case "$name" in
      "$UPDATE_UNIT"|gpn-update@*|gpn-notify@*) continue ;;
    esac
    # Mutual exclusion: gpn-fleet.service and gpn-node.service both open the
    # SAME chain DB ($PREFIX/node/chains/.../db/full). When the fleet supervisor
    # is installed, gpn-node.service is the duplicate and must never be touched
    # — and `try-restart` is not the safety net its man page implies: on
    # 2026-11-12 19:47:18 it started a unit that was disabled AND inactive, that
    # unit took the RocksDB LOCK, the fleet's own node then died with "lock
    # file: Resource temporarily unavailable", and the chain went down for
    # ~7 minutes until the duplicate was stopped by hand.
    if [ -f "$UNIT_DIR/gpn-fleet.service" ] && [ "$name" = "$NODE_UNIT" ]; then
      continue
    fi
    line="$(grep -m1 '^ExecStart=' "$u" || true)"
    [ -n "$line" ] || continue
    # Expand %h (the user's home) so a shell-wrapped ExecStart is recognised:
    # gpn-oracle runs `/bin/sh -c 'exec %h/.local/bin/gpn_operate_oracle …'`, and
    # a strict `ExecStart=$BIN_DIR/` prefix test never saw it. A unit was
    # "covered" only if its ExecStart *began* with the bin dir, which is the
    # narrower question, not the right one.
    line="${line//%h/$HOME}"
    case "$line" in
      *"$BIN_DIR/"*|*"$PREFIX/"*) printf '%s\n' "$name" ;;
    esac
  done
}

# Build the restart hook. `try-restart`, never `restart`: a unit the operator
# deliberately stopped must stay stopped, and on this kind of host
# `gpn-node.service` can be an INACTIVE standalone authoring node — `restart`
# would start a second supervisor against the same chain data.
restart_cmd_for_host() {
  local u list=""
  for u in $(units_that_run_installed_code); do
    list="$list $u"
  done
  [ -n "$list" ] || return 0
  printf 'systemctl --user try-restart%s' "$list"
}

# Enable ONLY the updater. Refuses to touch node/ollama units: on a fleet host
# enabling a second gpn-node.service would run two supervisors against one base
# path.
cmd_install_updater() {
  local default_restart
  default_restart="$(restart_cmd_for_host)"
  write_update_units "$default_restart" || return 1
  sc daemon-reload
  sc enable --now "$UPDATE_TIMER"
  log "updater installed: $UNIT_DIR/{$UPDATE_UNIT,$UPDATE_TIMER}"
  if [ -n "$default_restart" ]; then
    log "  restart hook: $default_restart"
  else
    log "  restart hook: none (the updater will print the command to run)"
  fi
  log "  first check in 10 min · logs: journalctl --user -u $UPDATE_UNIT -f"
}

cmd_remove_updater() {
  local unit
  for unit in "$UPDATE_TIMER" "$UPDATE_UNIT"; do
    if [ -f "$UNIT_DIR/$unit" ]; then
      sc disable --now "$unit" || true
    fi
  done
  sc daemon-reload
  rm -f "$UNIT_DIR/$UPDATE_TIMER" "$UNIT_DIR/$UPDATE_UNIT"
  log "updater removed (only $UPDATE_TIMER and $UPDATE_UNIT). Nothing else was touched."
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
  log "autostart installed: $UNIT_DIR/{$NOTIFY_UNIT,$OLLAMA_UNIT,$NODE_UNIT,$UPDATE_UNIT,$UPDATE_TIMER}"
  log "  start now:  systemctl --user start $NODE_UNIT"
  log "  logs:       journalctl --user -u $NODE_UNIT -f"
  log "  verify:     $0 verify   (enabled + linger + simulated reboot)"
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
  for unit in "$UPDATE_TIMER" "$UPDATE_UNIT" "$NODE_UNIT" "$OLLAMA_UNIT" "$NOTIFY_UNIT"; do
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
  for unit in "$NOTIFY_UNIT" "$NODE_UNIT" "$OLLAMA_UNIT" "$UPDATE_UNIT" "$UPDATE_TIMER"; do
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

# Prove the property the whole script exists for: after a reboot the node
# comes back with no user action. Failure is loud and returns non-zero.
cmd_verify() {
  local ok=1
  pass() { printf '  PASS %s\n' "$*"; }
  fail() { printf '  FAIL %s\n' "$*"; ok=0; }

  if [ ! -f "$UNIT_DIR/$NODE_UNIT" ]; then
    fail "$NODE_UNIT is not installed (run: gpn-autostart.sh install)"
    return 1
  fi
  pass "$NODE_UNIT exists"

  if ! have systemctl; then
    fail "systemctl not found - cannot verify the boot path on this host"
    return 1
  fi

  local en
  en=$(systemctl --user is-enabled "$NODE_UNIT" 2>/dev/null || true)
  if [ "$en" = enabled ]; then pass "$NODE_UNIT is enabled"; else fail "$NODE_UNIT is '$en', expected 'enabled'"; fi

  if have loginctl; then
    if loginctl show-user "$(id -un)" -p Linger 2>/dev/null | grep -qx 'Linger=yes'; then
      pass "linger is enabled (starts at boot, not only at login)"
    else
      fail "linger is OFF - the node starts only when you log in (sudo loginctl enable-linger $(id -un))"
    fi
  else
    fail "loginctl not found - cannot prove the node starts at boot"
  fi

  # Simulated reboot: stop the unit, then start it again with no other action.
  systemctl --user stop "$NODE_UNIT" 2>/dev/null || true
  sleep 1
  if ! systemctl --user start "$NODE_UNIT" 2>/dev/null; then
    fail "systemctl --user start $NODE_UNIT failed"
    return "$ok"
  fi
  local i state=""
  for i in 1 2 3 4 5 6 7 8 9 10; do
    state=$(systemctl --user is-active "$NODE_UNIT" 2>/dev/null || true)
    [ "$state" = active ] && break
    sleep 1
  done
  if [ "$state" = active ]; then
    pass "the node came back after stop/start (simulated reboot, no user action)"
  else
    fail "unit state after stop/start is '$state', expected 'active'"
  fi

  if [ "$ok" = 1 ]; then echo "autostart verification PASSED"; else echo "autostart verification FAILED"; fi
  [ "$ok" = 1 ]
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
  install-updater) cmd_install_updater ;;
  remove-updater)  cmd_remove_updater ;;
  remove)  cmd_remove ;;
  status)  cmd_status ;;
  verify)  cmd_verify ;;
  *) sed -n '2,/^set -euo pipefail/p' "$0" | sed '$d'; exit 2 ;;
esac
