#!/usr/bin/env bash
# gpn-snapshot.sh — thin wrapper around the `gpn_snapshot` binary (GPN-TASKS #23).
#
# Fast node onboarding: export an already-synced node's RocksDB + a manifest,
# then import it into a fresh node so it starts near the tip instead of
# full-syncing every block (with its Ethash seal) from genesis.
#
#   ./scripts/gpn-snapshot.sh info   [--rpc URL] [--out meta.json]
#   ./scripts/gpn-snapshot.sh export --base-path <node dir> --out <file.tar.gz|.tar.zst> [--freeze]
#   ./scripts/gpn-snapshot.sh import --snapshot <file> --base-path <dir> [--force]
#
# `--freeze` is the safe way to snapshot a RUNNING node: it SIGSTOPs the node,
# copies, then SIGCONTs (files can't change while frozen; survives gpn_fleet
# restarting the node). Otherwise STOP the node first.
#
# Env:
#   GPN_SNAPSHOT_BIN  path to the binary   (default: ~/.local/bin/gpn_snapshot,
#                                            else <repo>/gpn-agent/target/release/gpn_snapshot)
#   GPN_NODE_DATA     default --base-path  (passed through as the node dir)
#   GPN_RPC           default RPC url for `info`/`export` (default http://127.0.0.1:9944)
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"

bin="${GPN_SNAPSHOT_BIN:-}"
if [ -z "$bin" ]; then
  if [ -x "$HOME/.local/bin/gpn_snapshot" ]; then
    bin="$HOME/.local/bin/gpn_snapshot"
  elif [ -x "$here/gpn-agent/target/release/gpn_snapshot" ]; then
    bin="$here/gpn-agent/target/release/gpn_snapshot"
  elif [ -x "$here/gpn-agent/target/debug/gpn_snapshot" ]; then
    bin="$here/gpn-agent/target/debug/gpn_snapshot"
  else
    echo "gpn_snapshot not found. Build it:  cargo build --release --bin gpn_snapshot" >&2
    echo "  (in $here/gpn-agent) or set GPN_SNAPSHOT_BIN." >&2
    exit 127
  fi
fi

args=("$@")
# Default the base-path / RPC from the environment when the caller didn't pass them.
has_base=0; has_rpc=0
for a in "${args[@]}"; do
  case "$a" in
    --base-path|--base-path=*) has_base=1 ;;
    --rpc|--rpc=*) has_rpc=1 ;;
  esac
done
if [ "$has_base" -eq 0 ] && [ -n "${GPN_NODE_DATA:-}" ]; then
  args+=(--base-path "$GPN_NODE_DATA")
fi
if [ "$has_rpc" -eq 0 ] && [ -n "${GPN_RPC:-}" ]; then
  args+=(--rpc "$GPN_RPC")
fi

exec "$bin" "${args[@]}"
