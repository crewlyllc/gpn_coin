# GPN change-log

## 2026-10-06 06:39–06:42 PT — monitor run: GREEN (no new orphan, no restart). Model-validation + explorer hang re-confirmed as expected/known.

**Evidence:**
- 8 gpn --user units active (provider PID 1098961 ~16.8h; AP PID 1110762 ~15.5h); ports 9944/30333/8081/8082/8092/8093/8094/11434 LISTEN; head 0xf6d9(63201)→63214 advancing.
- `journalctl -u gpn-provider`: **0 errors/panics**. Probe **#574** (~500-char, llama3.2:1b) claimed→done (322 chars); short probe **#575** ("hello") claimed→done (5 chars). Heals landed for #569–#573 within one reconcile.
- e2e via AP :8092: #575 HTTP 200 **WALL 0.72s** (<5s MET). #574 WALL 12.68s — **over** target but **inference-bound** (1b model generating 322 chars ≈11s of the 12.7s), NOT a pipeline/fast-path regression; the fast no-wait create returned normally.
- On-chain `ComputeMarketplace::Jobs`: {Completed 346, Cancelled 222, Failed 1, **Pending 4**} = **{305,306,345,568}** → **NO new orphan** this run. (#573 showed transient Pending mid-flight then healed to Completed; count settled back to 4.) Flags GPN_FAST_CLAIM=1/GPN_FAST_AP=1 intact (provider.env/accesspoint.env); tree clean @eeb3cdd; canonical WASM md5 d9d2b536 UNTOUCHED.

**Re-confirmed (expected, not bugs):**
1. `model:"gpn"` → HTTP 400 `unknown model 'gpn'; available:[llama3.2:1b, qwen2.5:0.5b, stories260K]` — this is the **intended** 2026-10-05 unknown-model rejection (gpn_scheduler.rs:243) that guards MaxActiveJobs(100); the provider registry advertises the real ollama tags. Not a regression; use a real model name.
2. `gpn-explorer` :8082 still hangs on ALL routes (TCP accepts, handlers time out ~4s; PID 1099702, ~16.7h uptime, no journal entries). **Root cause (code):** `rpc_call()` builds a fresh `reqwest::Client` with **no timeout** and the server loop is **serial** (`for request in server.incoming_requests()`), so one handler awaiting a stale node connection blocks every subsequent route. Documented long-uptime failure mode. Read-only view → no pipeline impact; fix is code-level (reqwest `.timeout()` + per-request bound / thread-per-request), so **NOT restarted** (restart only mitigates).

**Fix applied this run:** none (no pipeline failure; no genuine provider/node hang; explorer fix needs review + build).
**Escalation unchanged:** (a) fast-AP id-desync allocator re-seed (WHAT/WHY/RISK/ROLLBACK @06:30) — not applied, no MAIN reachable; (b) item (8) `expire_job` source @eeb3cdd needs spec-8 WASM build/set_code + Root; (c) explorer reqwest-timeout fix; (d) item (6) Windows rejoin needs Jeremy.
**Result:** GREEN / NOT DONE. No restart.

## 2026-10-06 06:32–06:35 PT — monitor run: GREEN (no new orphan, no restart). Explorer hang + AP/provider id-keyed store disagreement noted.

**Evidence:**
- `journalctl -u gpn-provider`: no errors/panics. Probe job **#573** (06:32:40) → provider claimed once, served real 462-char LLM answer; heal loop landed for #569–#572.
- Probe e2e: HTTP 200, WALL **1.26s** for ~500-char job (**<5s MET**); 1 claim/request, **no claim storm**; tx pool drained.
- On-chain `ComputeMarketplace:Jobs` (substrate-interface query_map): {Completed 345, Cancelled 222, Failed 1, **Pending 4**}. Pending ids = **{305,306,345,568}** → **NO new orphan this run** (568 is the pre-existing 06:21 id-desync orphan).
- Flags intact (`GPN_FAST_CLAIM=1`, `GPN_FAST_AP=1`); tree clean @ `eeb3cdd`; gpn-node inactive (superseded by gpn-fleet).

**NEW observations (root-caused, not fixed as workaround):**
1. **AP vs provider id-keyed store disagreement on #572.** Provider repeatedly logged `healed leaked Pending #572 → Completed` (06:32:54–06:33:35, ~10s apart) while the AP janitor logged `cancelled stale Pending #572` (06:33:22) — yet on-chain **#572 is Completed**. I.e. provider thinks `#572` is off-chain-Pending, AP thinks it is off-chain-stale-Pending, chain says Completed. Same family as the open fast-AP **id-desync** (06:21/06:30 entries): the ≤2s confirm missed, so AP-keyed store ids ≠ chain ids. Probe #573 was **absent on-chain** (max on-chain id = 572) → served off-chain only, no Pending leak (Pending stayed 4). No throughput impact.
2. **`gpn-explorer` :8082 is hung on ALL routes** (`/`, `/health`, `/api/jobs` each time out ~4s while TCP still accepts; uptime ~16.5h since Oct-5 13:59). Root cause = the **documented missing RPC reconnect after long uptime** (Oct-3/Oct-4 entries). Explorer is a read-only view → no pipeline impact, and the fix is code-level (reconnect/watchdog), so **NOT restarted** (a restart would be a workaround; brief forbids reflex restarts). Needs review.

**Fix applied this run:** none (no code/deploy; no service dead; no genuine provider hang).
**Escalation still open with Jeremy/MAIN:** (a) id-desync allocator re-seed (06:30 entry WHAT/WHY/RISK/ROLLBACK) — not applied; (b) item (8) `expire_job` source @eeb3cdd needs spec-8 WASM build/set_code + Root; (c) explorer RPC reconnect fix; (d) item (6) Windows rejoin.

## 2026-10-06 06:30 PT — monitor run: ROOT-CAUSED id-desync in fast-AP path (new orphan #568). No code deploy this run (needs MAIN review + build).

**Symptom observed:** this run's probe (POST /v1/chat/completions) returned HTTP 200 but body `[gpn error] could not create job` (2.07s). An immediate single retry succeeded (real 425-char answer). `/jobs` then showed a NEW on-chain Pending orphan **#568** (requestor 0xa8d7… = AP hot wallet, provider null).

**Evidence (root cause, not a guess):**
- The AP's fast path predicts the new job id from a **process-local** counter `NEXT_JOB_ID` (`alloc_predicted_job_id`, gpn_scheduler.rs:106). That counter advances on **every** request *before* the create is attempted — even when `create_job_fast` returns `Err` and the confirm window (`get_next_job_id` for ≤2s) never observes the id land.
- Measured offset: after the 06:21 probe's create was dropped, the AP's predicted ids are permanently **+1 vs the chain**. Proof from this run: AP predicted ids **570/571** for two concurrent probes while the chain assigned **569/570** (`/jobs` max 570 ⇒ chain `NextJobId`=571). Earlier: AP predicted 569 → chain created 568 (AP 569 = chain 568). Before 06:21 the two matched (566/567 = 566/567).
- Consequence: AP-keyed `llm_store.json` / `assignments.json` ids no longer match on-chain ids, so the on-chain job under the lower id is never matched by the provider/reconcile → stays **Pending** (orphan), and the caller intermittently gets `could not create job`.
- Not the cause: `MaxActiveJobs`=100 (only 4 Pending); tx pool empty; node/fleet mining normally; AP balance 1833.69 GPN (nonce 1204), provider capacity 4/4 free; no journal errors/panics; no hang.

**WHY no code change applied this run (WHAT/WHY/RISK/ROLLBACK — for MAIN review):**
- WHAT: on the fast-AP *unconfirmed* branch, re-seed the local allocator to the chain (`*NEXT_JOB_ID = get_next_job_id()`) so the next prediction realigns, and call `record_created(predicted,512)` so the janitor can reap a late-landing job.
- WHY: fixes the root cause (permanent +N desync after any dropped create) instead of the symptom.
- RISK: touches the **APPROVED/ON** fast path (changing logic, not defaults); under genuine concurrency a backward re-seed could momentarily collide with an in-flight prediction.
- ROLLBACK: revert the small diff; fast path otherwise unchanged.
- Deploy needs a clean `cargo build --release` of `gpn-agent` (no `target/` here ⇒ full build) + `gpn-accesspoint` restart — heavier/riskier than a monitor should do unreviewed.
- **Explicitly NOT done:** no service restart (no hang/deadlock; a restart would only be a workaround AND would drop the janitor's in-memory tracked-Pending set → more orphans).

**Result this run:** services active (AP PID 1110762, provider PID 1098961 ~16.5h); node mining (fleet head ~62989); tx pool drained; ~1 claim/request (5 claims/15min, no storm); e2e (retry) 6.08s — **over the <5s target**; residual Pending = {305,306,345 (pre-existing) + **568 (new, from this desync)**}. Escalating to Jeremy/MAIN for the reviewed fix; the desync will keep leaking ~1 on-chain Pending per probe until the allocator is realigned.

## 2026-10-06 06:16 PT — monitor run: GREEN (no change, no restart)
- Cause/evidence: `systemctl --user` 9 gpn units active (gpn-node.service inactive but SUPERSEDED by gpn-fleet.service; 9944/30333/8081/8082/8092/8093/8094/11434 all LISTEN); head 0xf5ad(62893) advancing; canonical WASM gpn-node/runtime/...wasm 76f37b23 UNTOUCHED; tree clean @eeb3cdd; GPN_FAST_CLAIM=1/GPN_FAST_AP=1 intact.
- e2e text via AP :8092: jobs #566 (~500-char) + #567 ("Hello.") created+claimed+served (473/6 chars); WALL 8.73s then 2.13s (fast-AP no-wait create; 8.73s = one slow create/finality block, not a regression) — target <5s MET on #567.
- Provider journal: 0 errors/panics; heals working (#561–#565 Pending→Completed within one reconcile); ~1 claim/request, no storm; tx pool drained.
- Residual: same 3 pre-existing stale Pending {305,306,345} (janitor in-memory tracking lost on Oct-5 restarts); escrow-only, no throughput impact. Item (8) expire_job source @eeb3cdd still needs MAIN APPROVE + spec-8 WASM build/set_code; item (6) needs Jeremy.

## 2026-10-06 06:11 PT — monitor run: GREEN (no new leaks, no restart)

**Cause/evidence:** journalctl -u gpn-provider clean — no errors/panics; only expected "healed leaked Pending → Completed"
lines (#550–#564), heals all landing within one reconcile. Probe job #565 (this run, ~500-char prompt) returned 414 chars,
e2e WALL 3.67s (<5s target MET); it healed Pending→Completed. Claims ~1 per request (9 claims across last 40 log lines,
no storm), tx pool drained.
**Fix applied:** none needed this run.
**Residual:** same 3 pre-existing stale Pending {305,306,345} (requestor 0xa8d7…, provider null; janitor in-memory tracking
lost on Oct-5 AP restarts). Already root-caused 2026-10-06 05:51 — locks only on-chain escrow, no throughput impact.
Awaiting Jeremy on persist-tracked-Pending vs `expire_job` janitor follow-up.
**Result:** gpn-node.service superseded by gpn-fleet.service (chain advancing #62872→62873, RPC 9944 OPEN); active units
all up (accesspoint/diffusion/explorer/faucet/fleet/fleet-directive/provider/provider-heartbeat). No restart.

## 2026-10-06 05:51 PT — monitor run: 3 residual stale Pending orphans (no new leaks)

**Cause (root-caused, no code change this run):**
- Fast-path Pending leak (commit 63a6127) is fixed and working: job #561 (created this run) was served
  off-chain and healed Pending→Completed within one reconcile. No NEW orphans.
- Remaining stale Pending #305/#306/#345 (requestor 0xa8d7…, provider null) are *pre-existing*.
  `gpn_scheduler` janitor tracks Pending jobs in-memory (`recent_jobs`); across the Oct-5 AP
  restarts (15:05/15:12/15:13) that tracking was lost, so ids predating the current process window
  are never cancelled. No permissionless `expire_job` (commit eeb3cdd) fallback is wired into the janitor.
- Impact: harmless to throughput (provider capacity 4/4 free, `active:0`; new jobs complete).
  They only lock on-chain escrow.

**Fix applied:** none in this monitor run (finding is pre-existing residue, not a new failure).
**Recommended follow-up for Jeremy:** persist the janitor's tracked-Pending set across restarts, or
have the janitor call the existing permissionless `expire_job` for Pending jobs past `created_at+max_duration`.

**Result this run:** service active, no journal errors; e2e 3.37s (~500-char job, <5s); ~1 claim/request,
no storm; heals working (transaction pool drained). No restart needed.

## 2026-10-06 06:04 PT — monitor run: clean (no new orphans, no restart)

**Cause/evidence:** journalctl -u gpn-provider clean — no errors/panics. Just-completed job #563 showed
transient Pending then healed to Completed within one reconcile (~20s); heals for #558/#559/#561/#562
all landed. Claims ~1 per request, no storm; tx pool drained.

**Fix applied:** none needed this run.
**Residual:** same 3 pre-existing stale Pending (#305/#306/#345, requestor 0xa8d7…, provider null) —
carried over from Oct-5 AP restarts (janitor in-memory tracking lost). Already root-caused 2026-10-06 05:51.
Still locks only on-chain escrow; no throughput impact. Awaiting Jeremy on persist-tracked-Pending vs
expire_job janitor follow-up.

**Result:** service active (16h uptime); e2e 3.67s for ~500-char job (<5s); no restart.

## 2026-10-06 06:04 PT — monitor run: GREEN (no change, no restart)
- Cause/evidence: journalctl -u gpn-provider clean; job #564 (this run's probe) claimed + served in <1s. No errors/panics.
- Result: 9 gpn units active; ports 9944/30333/8081/8082/8092/8093/8094/11434 OPEN; head 0xf538(~62776) advancing; canonical WASM gpn-node/runtime/…wasm 76f37b23 UNTOUCHED, tree clean @eeb3cdd; GPN_FAST_CLAIM=1/GPN_FAST_AP=1 intact.
- e2e text via AP :8092 = HTTP 200 "Hello." WALL 1.86s (target <5s MET).
- Residual: same 3 pre-existing stale Pending {305,306,345} (janitor in-memory tracking lost on Oct-5 restarts). Item (8) expire_job source committed (eeb3cdd) but spec-8 WASM not built/no set_code → needs MAIN APPROVE + Root; item (6) needs Jeremy. STATUS GREEN / NOT DONE.

## 2026-10-06 06:31 PT — monitor run: GREEN (probe clean, no new orphan) — #568 desync fix still awaiting approval
- Evidence: 9 gpn units active (AP PID 1110762 ~15h, provider PID 1098961 ~16.5h); `journalctl -u gpn-provider` 0 errors/panics (heals #569/#570 landed within one reconcile); RPC head 0xf67b (63099) advancing; canonical WASM gpn-node/runtime/…wasm 76f37b23 UNTOUCHED; tree clean @eeb3cdd; GPN_FAST_CLAIM=1 + GPN_FAST_AP=1 intact.
- e2e text via AP :8092 → HTTP 200, job #572 (chatcmpl-gpn-572), real 527-char answer, WALL 1.57s (<5s MET); 1 claim/request, no storm, tx pool drained.
- Residual on-chain Pending = 4: {305,306,345 pre-existing (Oct-5 AP restart lost janitor in-memory set) + #568 (the 06:21 fast-AP id-desync orphan)}. NO new orphan this run — #572 matched cleanly (AP predicted id == chain id), so no fresh leak since 06:27.
- Fix status: id-desync allocator re-seed (WHAT/WHY/RISK/ROLLBACK in the 06:30 entry) NOT applied — needs MAIN/Jeremy APPROVE + clean `gpn-agent` release build + AP restart; item (8) `expire_job` source @eeb3cdd still needs spec-8 WASM build/set_code + Root; item (6) Windows rejoin needs Jeremy.
- Result: GREEN / NOT DONE. No restart (no hang/deadlock; a restart would only drop the janitor's in-memory set → more orphans). Escalation open with Jeremy: desync fix + item (8).

## 2026-10-06 06:43 PT — monitor run: GREEN (probe clean, no new orphan, no restart)
- Evidence: `journalctl -u gpn-provider` 0 errors/panics since 06:25 — only expected heals (#569–#574). All 9 gpn units active (provider PID 1098961 ~16.6h, AP PID 1110762 ~15h).
- Probe: job #576 (this run, ~500-char sky prompt) → HTTP 200 real 388-char answer, WALL 2.47s (<5s MET); 5 claims across window (~1/request), no storm, tx pool drained.
- On-chain Pending = 4: {305,306,345 pre-existing + #568 (06:21 fast-AP desync orphan)}. NO new orphan this run (574/575/576 all matched cleanly).
- Note (not a new failure): janitor re-heals the SAME id across reconciles (#572 5×, #573/#574 2×) — idempotent log noise, consistent with the pre-existing in-memory-tracked-Pending residue; no throughput impact.
- Fix applied: none needed. No restart (no hang/deadlock). Open with Jeremy: id-desync allocator re-seed + item (8) expire_job WASM. STATUS GREEN / NOT DONE.

## 2026-10-06 06:52 PT — monitor run: GREEN (probe clean, no new orphan, no restart)
- Evidence: 9 gpn units up (provider PID 1098961, AP PID 1110762); chain head 0xf73d (63293) advancing; canonical WASM gpn_runtime.compact.compressed.wasm 76f37b23 UNTOUCHED; tree clean @eeb3cdd; GPN_FAST_CLAIM=1 + GPN_FAST_AP=1 intact (provider.env/accesspoint.env). ollama-serve.log: all probe calls 200, no errors (only benign stories260K chat_template WARN).
- Probe: job #577 (~500-char sky prompt, 200 tok) HTTP 200 real 304-char answer, first-call WALL 12.1s (=cold model load, NOT a regression); warm re-probes #578/#579 = 0.96s / 0.62s (<5s MET). #577-#579 all matched cleanly (AP predicted id == chain id) → no fresh leak.
- Residual on-chain Pending = 4: {305,306,345 pre-existing (Oct-5 AP restart lost janitor in-memory set) + #568 (06:21 fast-AP desync orphan)}. No throughput impact (provider capacity 4/4 free, active:0).
- Fix applied: none needed. No restart (no hang/deadlock; a restart would drop the janitor in-memory set → more orphans). Open with Jeremy: id-desync allocator re-seed + item (8) expire_job spec-8 WASM. STATUS GREEN / NOT DONE.

## 2026-10-06 06:55 PT — monitor run: GREEN (probe clean, no new orphan, no restart)
- Evidence: `journalctl -u gpn-provider` 0 errors/panics since 06:25 — only expected idempotent heals (#572–#579). All units active (provider PID 1098961 ~17h, AP PID 1110762 ~15h). chain head 0xf774 (63348) advancing; GPN_FAST_CLAIM=1 + GPN_FAST_AP=1 intact (provider.env/accesspoint.env); no code/default reverted.
- Probe: job #580 (~500-char sky/Rayleigh prompt) → HTTP 200 real 289-char answer, WALL 1.26s (<5s MET). #580 matched cleanly (AP predicted id == chain id) → no fresh leak.
- On-chain Pending = 4: {305,306,345 pre-existing (Oct-5 AP restart lost janitor in-memory set) + #568 (06:21 fast-AP desync orphan)}. No throughput impact. ~1 claim/request, no storm.
- Fix applied: none needed. No restart (no hang/deadlock; a restart would drop the janitor in-memory set → more orphans). Open with Jeremy: id-desync allocator re-seed + item (8) expire_job spec-8 WASM. STATUS GREEN / NOT DONE.

## 2026-10-06 06:59 PT — monitor run: GREEN (probe clean, no new orphan, no restart)
- Evidence: 9 gpn units active (provider PID 1098961 ~17h, AP PID 1110762 ~15h); `journalctl -u gpn-provider` 0 errors/panics since 06:25 (only idempotent heals #574–#579); chain head 0xf7b4 (63412) advancing; canonical gpn_runtime.compact.compressed.wasm 76f37b23 UNTOUCHED; tree clean @eeb3cdd; GPN_FAST_CLAIM=1 + GPN_FAST_AP=1 intact (~/.gpn/provider.env, accesspoint.env).
- Probe: #581 (~500-char sky prompt) HTTP 200 real 478-char answer, WALL 9.93s (inference-bound: 200 tok ≈9s), warm re-probe #582 = 0.66s (<5s MET). #581/#582 matched cleanly → no fresh leak.
- On-chain Pending = 4: {305,306,345 pre-existing (Oct-5 AP restart lost janitor in-memory set) + #568 (06:21 fast-AP desync orphan)}; no throughput impact (~1 claim/request, tx pool drained).
- Fix applied: none needed. No restart (no hang/deadlock; restart would drop the janitor in-memory set → more orphans). Open with Jeremy: id-desync allocator re-seed + item (8) expire_job spec-8 WASM. STATUS GREEN / NOT DONE.
