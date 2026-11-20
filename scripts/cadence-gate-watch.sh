#!/usr/bin/env bash
# cadence-gate-watch.sh — the CALL SITE for scripts/check-block-cadence.py.
#
# Why this exists (0.3.63, row 217/218 call-site box): the gate was written, self-tested
# and run by hand three times, but nothing invoked it on a schedule — so it could only
# report when a human remembered. Same failure mode as check-deploy-drift.sh before
# deploy-drift-watch.sh: a correct checker with no caller.
#
# What it does, no alerting (that is an owner/assistant decision, same as deploy-drift):
#   1. run the gate against the local node, attributing gaps from the node's own journal,
#   2. keep the raw 100-block sample next to the state file as re-derivable evidence,
#   3. write ~/.gpn/cadence-gate.state as one JSON object:
#        {"checked_at":"<ISO8601 Z>","exit":0,"violations":0,"max_gap_s":15.2,"message":"…"}
#   4. exit with the gate's code (0 pass, 1 fail, 2 collection/RPC error) so that
#      `systemctl --user show gpn-cadence-gate.service -p Result` is truthful.
#
# Installed as `gpn-cadence-gate.timer` (every 30 min, user unit) on the fleet host.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
state="${GPN_CADENCE_STATE:-$HOME/.gpn/cadence-gate.state}"
sample="${GPN_CADENCE_SAMPLE:-$HOME/.gpn/cadence-gate-sample.json}"
unit="${GPN_CADENCE_UNIT:-gpn-fleet}"
rpc="${GPN_CADENCE_RPC:-http://127.0.0.1:9944}"

# the fleet supervisor may not be the unit name on every box; fall back to log-less
# attribution rather than failing collection over a unit name.
unit_args=()
if systemctl --user cat "$unit" >/dev/null 2>&1; then
  unit_args=(--journal-unit "$unit")
fi

out="$(python3 "$repo/scripts/check-block-cadence.py" --rpc "$rpc" --json "$sample" "${unit_args[@]}" 2>&1)"
code=$?

mkdir -p "$(dirname "$state")"
tmp="$state.tmp.$$"
# NOTE: the output travels in an env var, not on stdin — the heredoc owns stdin.
GATE_OUT="$out" python3 - "$state" "$tmp" "$code" <<'PY'
import json, os, re, sys, datetime
state, tmp, code = sys.argv[1], sys.argv[2], int(sys.argv[3])
msg = os.environ.get("GATE_OUT", "")
flat = " | ".join(l.strip() for l in msg.splitlines() if l.strip())[:2048]
m = re.search(r"max gap\s*:\s*([0-9.]+)\s*s", msg)
ratio = re.search(r"([0-9]+) suspect gap\(s\), ([0-9]+) unexplained", msg)
if ratio:
    viol = int(ratio.group(2))
else:
    ratio = re.search(r"([0-9]+) violation\(s\)", msg)
    viol = int(ratio.group(1)) if ratio else None
payload = {
    "checked_at": datetime.datetime.now(datetime.timezone.utc)
                  .replace(microsecond=0).isoformat().replace("+00:00", "Z"),
    "exit": code,
    "violations": viol,
    "max_gap_s": float(m.group(1)) if m else None,
    "message": flat,
}
with open(tmp, "w") as f:
    json.dump(payload, f)
    f.write("\n")
PY
rc=$?
if [ $rc -eq 0 ]; then
  mv -f "$tmp" "$state"
else
  rm -f "$tmp"
fi

# stdout only when it is worth a journal line
if [ "$code" -ne 0 ]; then
  echo "cadence gate: exit $code — $out"
fi
exit "$code"
