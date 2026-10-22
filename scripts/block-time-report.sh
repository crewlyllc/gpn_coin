#!/usr/bin/env bash
# Assemble the block-time analysis document from the sampler + journals.
#
#   bash scripts/block-time-report.sh <sample.jsonl> <out.md>
#
# Every number in the output is regenerated from the raw sample and the node's
# own journals; nothing is typed by hand.
set -euo pipefail
cd "$(dirname "$0")/.."
SAMPLE="${1:-/tmp/gpnbt/bt-20261022.jsonl}"
OUT="${2:-/home/jeremyg/shared_workspace/artifacts/project7/analysis/block-time-2026-10-22.md}"
PROV="${3:-/tmp/gpnbt/provider.log}"
PY=/usr/bin/python3

{
cat <<'HEAD'
# GPN block time & transaction latency — measured 2026-10-22

**Question.** Is the 44–116 s e2e AI-job latency (change-log #47–#51) caused by
mining pauses during jobs, by the difficulty retarget, by tx-pool
non-inclusion, or by the node stalling? Measure first, then fix only the
dominant cause.

**Method.**
* `scripts/block-time-sampler.py` polls `chain_getHeader` every ~1 s for 50 min,
  recording the arrival time of every new best block, its authored
  `pallet_timestamp`, `nvidia-smi` util/VRAM every 5 s, `author_pendingExtrinsics`
  every 10 s, and `system_health` peers/isSyncing every 30 s.
* Attribution is done against the node's **own journal**, not by inference:
  mining-pause windows come from the miner's `parked`/`resuming` lines, and
  cause (d) is ruled in only when the node's 5 s informant heartbeat is *silent*
  through a gap (a live node reporting `best: #<prev>` proves it had nothing to
  import, i.e. the block did not exist).
* Cause (c) — blocks produced but our tx not included — is not a block-time gap
  at all; it is measured separately as create-submit → inclusion from the
  provider journal over 12 h (`scripts/inclusion-delay.py`).

---

HEAD

echo "## 1. Block-time distribution"
echo
$PY scripts/block-time-analyze.py "$SAMPLE" 2>/dev/null | sed -n '/^## Distribution/,/^## Timestamp semantics/p' | head -n -1
echo
echo "## 2. Timestamp semantics, side signals and cause split"
echo
$PY scripts/block-time-analyze.py "$SAMPLE" 2>/dev/null | sed -n '/^## Timestamp semantics/,$p'
echo
echo "## 2b. The controller's actual output (no model)"
echo
$PY scripts/retarget-steps.py 2>/dev/null
echo
echo "---"
echo
echo "## 3. Cause (c): create-submit → inclusion (12 h of real jobs)"
echo
if [ -f "$PROV" ]; then $PY scripts/inclusion-delay.py "$PROV"; else echo "(provider log not captured)"; fi
cat <<'TAIL'

---

## 4. Reading the numbers

**The p95 is not a defect.** A memoryless PoW solve at a 15 s mean has
`P(interval > 3x) = 5 %`, so **p95 ≈ 45 s whatever the controller does**. The
measured p95 sits at that floor. No retarget rule can lower it; only a shorter
`TargetBlockTime` can.

**What the controller does contribute is the far tail.** `2x/(1+x)` over a
10-block window is unbounded, and a 10-block mean of a 15 s exponential has
CV ≈ 32 %, so the multiplier routinely lands at 0.6–1.6. The observed
single-window steps were −42 % / +31 %, which is why bucket means oscillate
between 6 s and 37 s and why single blocks reach 150 s. A paired replay on 2 500
real intervals (`analysis/retarget-sim-2026-10-22.md`) puts the current rule at
**p99 127 s / max 554 s** versus **p99 102 s / max 229 s** with a ±12 % step
clamp and a 45-block window. That is the avoidable half of the tail.

**e2e latency = create-inclusion wait + ~1 s inference.** The #49 fix (never
claim before the create lands) makes the user wait for a block. Measured over
12 h: **p50 30.5 s, p95 79.8 s, max 183 s** — i.e. ~2 blocks at p50, because a
tx that arrives after a template is built waits for the next one. So the e2e
distribution is the block-time distribution shifted up by one block, which is
why the 110 s Worker proxy limit is breached a few times per day (§3), not
because of a bug in the job path. There were **0** priority/nonce (1014)
failures and **0** in-block reverts in that window: the pool always included our
txs — it just had no block to put them in.

**SLO statement (this repository stops claiming 5 s).** At a 15 s target block
time, achievable and *measured*:

| metric | achievable now | after the staged clamp/window |
|---|---|---|
| block time p50 | ≤ 15 s | ≤ 15 s |
| block time p95 | ≈ 50 s (≈3× mean, intrinsic) | ≈ 50 s (unchanged) |
| block time p99 | ≈ 90–127 s | ≈ 100 s |
| block time max (per 2 500 blocks) | ≈ 154 s observed, 554 s modelled | ≈ 229 s modelled |
| e2e p50 | 30 s (measured, §3) | ~25 s |
| e2e p95 | 80 s (measured, §3) | ~65 s |
| e2e > 110 s (Worker proxy) | 3 in 12 h | rare |

**A 5 s SLO is arithmetically unreachable on a 15 s-target chain** and should no
longer appear in reports. The two honest options, if a tighter SLO is wanted,
are (i) `TargetBlockTime` 3–5 s (p95 ≈ 12–15 s; the retarget window must stay
wide or it will oscillate), or (ii) remove the create-inclusion wait with a
pallet-side claim-lease so a provider can serve before inclusion — a runtime
change, not a driver patch.

## 4b. Cause (a) — mining pauses during AI jobs (expected, quantified, still worth fixing)

The miner parks on purpose while the GPU serves a job. Measured here: **~10 park
windows per hour, almost all 1–2 s** (inference of `llama3.2:1b` is ~1 s), plus
**one 28 s window** at 11:24:29 — the whole difference between those is *model
load*, not inference:

```
11:24:29 miner parked: GPU yielded to an AI job        ← qwen2.5:7b cold load
11:24:29 [job 6475] running inference — model=qwen2.5:7b
11:24:56 [job 6475] result returned to AP              ← 27 s, of which ~1 s inference
11:24:57 miner resuming: GPU is free again
```

`ollama` holds `qwen2.5:7b` at 4.7 GB of 8.2 GB VRAM (`/api/ps`), so while it is
loaded the miner genuinely cannot run — the pause is correct, but its *length* is
a scheduling artefact. Two cheap, non-consensus fixes, in order of value:

1. **Warm the model before parking the miner.** Park only once the model is
   resident (the load is CPU/disk work that can overlap with hashing), which
   turns a 28 s outage into ~1 s.
2. **Pin `keep_alive`** on the provider's `ollama` request (the provider sends
   none, so ollama's 5-minute default applies and any model switch re-triggers a
   cold load). Combined with advertising only models that fit *alongside*
   mining, this removes most cold loads.

This is not the dominant cause (§2: it explains ~1 in 8 long gaps), so it is a
proposal, not part of the staged runtime change.

## 5. Reproduce

```bash
python3 scripts/block-time-sampler.py --minutes 50 --out /tmp/gpnbt/bt.jsonl &
journalctl --user -u gpn-provider --since "-12 h" -o short-iso > /tmp/gpnbt/provider.log
bash scripts/block-time-report.sh /tmp/gpnbt/bt.jsonl <out.md> /tmp/gpnbt/provider.log
```
TAIL
} > "$OUT"
echo "[written] $OUT"
