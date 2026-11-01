#!/usr/bin/env bash
# uninstall.sh — undo what `install.sh` set up.
#
# The one command that MUST exist: a process that starts itself at boot has to
# be removable in one step, without hunting for unit files.
#
# Usage:
#   ./uninstall.sh --remove-autostart   stop/disable the units, drop linger,
#                                       keep chain data + wallet + models
#   ./uninstall.sh --status             show what autostart is currently set up
#   ./uninstall.sh                      print this help (does nothing)
#
# Nothing here deletes your money identity: $PREFIX/node/keystore.json and the
# chain DB at $PREFIX/node are always left untouched.
set -euo pipefail

SELF_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="$SELF_DIR/scripts/gpn-autostart.sh"
[ -f "$HELPER" ] || HELPER="$SELF_DIR/gpn-autostart.sh"
PREFIX="${GPN_PREFIX:-$HOME/.gpn}"

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m !\033[0m %s\n' "$*" >&2; }

usage() { sed -n '2,/^set -euo pipefail/p' "$0" | sed '$d'; }

[ $# -gt 0 ] || { usage; exit 0; }

ACTION=""
while [ $# -gt 0 ]; do
  case "$1" in
    --remove-autostart|--remove-autostart=*) ACTION=remove; shift ;;
    --status) ACTION=status; shift ;;
    --prefix) PREFIX="$2"; shift 2 ;;
    --prefix=*) PREFIX="${1#--prefix=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die_arg="$1"; warn "unknown argument: $die_arg"; usage; exit 2 ;;
  esac
done

[ -f "$HELPER" ] || { warn "autostart helper not found next to this script ($HELPER)"; exit 1; }

case "$ACTION" in
  remove)
    bash "$HELPER" remove --prefix "$PREFIX"
    echo
    echo "Autostart is gone. Your node data and wallet are untouched:"
    echo "  chain DB + keystore: $PREFIX/node   (keystore.json is your money — back it up)"
    echo "  models:              $PREFIX/models"
    echo "  binaries:            ${GPN_BIN:-$HOME/.local/bin}"
    echo "Start the node by hand any time:  gpn_node"
    ;;
  status) bash "$HELPER" status --prefix "$PREFIX" ;;
  *) usage ;;
esac
