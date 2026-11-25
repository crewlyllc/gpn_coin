#!/usr/bin/env bash
# soak-record-daily.sh — the CALL SITE for scripts/soak-record.py (row 273).
#
# Why a wrapper at all: the recorder writes into the repo's docs/, so it has to run from
# the one working clone (~/gpn-loop, owner decision 2026-11-24) and it must not fail the
# timer noisily when the node is briefly down — a missed reading is data, not an outage.
#
# Behaviour:
#   * cd to the clone, run the recorder, append its output to ~/.gpn/soak-record.log
#   * exit 0 on "recorded day N", exit 2 (systemd: failed) on "node unreachable"
#     so `systemctl --user show gpn-soak-record.service -p Result` is truthful
#   * never commits — the loop commits the doc with its own batch
#
# Installed as gpn-soak-record.timer (daily, user unit) on the fleet host.
set -uo pipefail

repo="${GPN_SOAK_REPO:-$HOME/gpn-loop}"
log="${GPN_SOAK_LOG:-$HOME/.gpn/soak-record.log}"

cd "$repo" || { echo "no clone at $repo" >&2; exit 2; }

out=$(/usr/bin/python3 "$repo/scripts/soak-record.py" 2>&1)
rc=$?
printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$out" >> "$log"
echo "$out"
exit "$rc"
