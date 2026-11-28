#!/usr/bin/env bash
# Second-coin miner wrapper (row 294) — the contract `gpn-agent::secondary` spawns.
#
#   second-miner-lolminer.sh --coin ergo --pool <url> --address <addr> \
#                            --device <n> --intensity-percent <p> --status-file <path>
#
# Why a wrapper and not "just call lolMiner": the supervisor owns *policy*
# (which coin, how much, pause for a job) and must not learn every miner's flag
# grammar. This script is the one place that knows lolMiner's.
#
# The miner binary is $LOLMINER, else `lolMiner` on PATH. If it is absent this
# exits 3 after writing `miner-not-installed` to the status file — the
# dashboard then says exactly that, instead of a miner that silently does
# nothing.
#
# INTENSITY is enforced as a **duty cycle**, not by trusting a vendor flag:
# each 60 s window the miner runs for `intensity% × 60 s` and is SIGSTOPped for
# the rest. That works with any miner, needs no per-algorithm table, and is the
# only meaning of "30 % of the card" we can actually measure. `--selftest`
# exercises the cycle in 2 s windows so a test can check it.

set -u

COIN=""; POOL=""; ADDRESS=""; DEVICE=""; INTENSITY=""; STATUS=""
SELFTEST=0
WINDOW_MS=$(( ${GPN_SECOND_MINER_WINDOW_MS:-60000} ))
LOG_MAX_BYTES=$(( ${GPN_SECOND_MINER_LOG_BYTES:-16384} ))

while [ $# -gt 0 ]; do
  case "$1" in
    --coin) COIN=${2:-}; shift 2;;
    --pool) POOL=${2:-}; shift 2;;
    --address) ADDRESS=${2:-}; shift 2;;
    --device) DEVICE=${2:-}; shift 2;;
    --intensity-percent) INTENSITY=${2:-}; shift 2;;
    --status-file) STATUS=${2:-}; shift 2;;
    --selftest) SELFTEST=1; shift;;
    -h|--help) sed -n '2,25p' "$0"; exit 0;;
    *) echo "second-miner: unknown argument \`$1\`" >&2; exit 2;;
  esac
done

say() { # last line the dashboard shows for this card
  [ -n "$STATUS" ] || return 0
  printf '%s %s\n' "$(date -u +%H:%M:%SZ)" "$*" >> "$STATUS"
  if [ -f "$STATUS" ] && [ "$(wc -c < "$STATUS")" -gt "$LOG_MAX_BYTES" ]; then
    tail -n 200 "$STATUS" > "$STATUS.tmp" && mv "$STATUS.tmp" "$STATUS"
  fi
}

die() { local msg=$1 code=${2:-1}; say "$msg"; echo "second-miner: $msg" >&2; exit "$code"; }

# ── selftest: measure the duty cycle on a sleeper, no GPU and no pool ─────────
if [ "$SELFTEST" = 1 ]; then
  INTENSITY=${INTENSITY:-30}
  WINDOW_MS=${GPN_SECOND_MINER_SELFTEST_WINDOW_MS:-2000}
  on_ms=$(( WINDOW_MS * INTENSITY / 100 ))
  off_ms=$(( WINDOW_MS - on_ms ))
  sleep 1000 & child=$!
  trap 'kill "$child" 2>/dev/null' EXIT TERM INT
  running=1
  t0=$(date +%s%N)
  sleep_on=0; sleep_off=0
  cycles=${GPN_SECOND_MINER_SELFTEST_CYCLES:-3}
  for _ in $(seq "$cycles"); do
    kill -CONT "$child" 2>/dev/null; running=1
    sleep "$(awk "BEGIN{printf \"%.3f\", $on_ms/1000}")"; sleep_on=$((sleep_on + on_ms))
    kill -STOP "$child" 2>/dev/null; running=0
    sleep "$(awk "BEGIN{printf \"%.3f\", $off_ms/1000}")"; sleep_off=$((sleep_off + off_ms))
  done
  t1=$(date +%s%N)
  kill "$child" 2>/dev/null
  total=$(( (t1 - t0) / 1000000 ))
  echo "selftest: window ${WINDOW_MS}ms intensity ${INTENSITY}% on=${sleep_on}ms off=${sleep_off}ms wall=${total}ms"
  exit 0
fi

[ -n "$COIN" ] || die "missing --coin" 2
[ -n "$POOL" ] || die "missing --pool" 2
[ -n "$ADDRESS" ] || die "missing --address" 2
[ -n "$DEVICE" ] || die "missing --device" 2
INTENSITY=${INTENSITY:-30}
case "$INTENSITY" in ''|*[!0-9]*) die "intensity \`$INTENSITY\` is not a number" 2;; esac
[ "$INTENSITY" -ge 1 ] && [ "$INTENSITY" -le 100 ] || die "intensity $INTENSITY outside 1..100" 2

# Only algorithms this miner really has (row 294, measured 2026-11-28: lolMiner
# 1.98a answers `Error: --algo option "ALEPHIUM" is not supported`, so Alephium
# gets its own refusal instead of a miner that exits the moment it starts).
case "$COIN" in
  ergo)       ALGO=AUTOLYKOS2 ;;
  alephium)   die "miner-not-supported: lolMiner has no Alephium algorithm (it refuses --algo ALEPHIUM); Alephium needs a different miner binary" 4 ;;
  ravencoin)  ALGO=KAWPOW ;;
  *)          die "unsupported coin \`$COIN\`" 2 ;;
esac

BIN=${LOLMINER:-$(command -v lolMiner || true)}
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
  die "miner-not-installed: lolMiner not found (set LOLMINER=/path/to/lolMiner)" 3
fi

say "starting $COIN via $BIN (algo $ALGO, device $DEVICE, intensity ${INTENSITY}%)"
# Coin-B rewards go to the operator's own address: --user is that address and
# nothing in this path ever touches GPN's account or the airdrop export.
"$BIN" --algo "$ALGO" --pool "$POOL" --user "$ADDRESS" --devices "$DEVICE" \
       ${GPN_SECOND_MINER_EXTRA_ARGS:-} > >(while IFS= read -r l; do say "$l"; done) 2>&1 &
child=$!
trap 'kill -CONT "$child" 2>/dev/null; kill "$child" 2>/dev/null; wait "$child" 2>/dev/null; say "miner stopped"' EXIT TERM INT

on_ms=$(( WINDOW_MS * INTENSITY / 100 ))
off_ms=$(( WINDOW_MS - on_ms ))
while kill -0 "$child" 2>/dev/null; do
  kill -CONT "$child" 2>/dev/null
  sleep "$(awk "BEGIN{printf \"%.3f\", $on_ms/1000}")"
  kill -STOP "$child" 2>/dev/null
  sleep "$(awk "BEGIN{printf \"%.3f\", $off_ms/1000}")"
done
wait "$child"; rc=$?
if [ "$rc" -ne 0 ]; then
  die "pool-disconnected: miner exited $rc" "$rc"
fi
say "pool-disconnected: miner exited 0"
exit 0
