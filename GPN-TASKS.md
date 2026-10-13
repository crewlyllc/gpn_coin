# GPN-TASKS.md — PM07 engineering queue

Numbered engineering queue for the GPN testnet (repo: gpn_coin / projectmanager07).
Maintained by the PM07 agent; each item is smallest-correct-change, committed + pushed.

---

## #24 — Simplified node behavior: full-stop mining during a job; one GPU = one job — DONE (2026-10-13)

**Spec (Jeremy):** a node mines by default; when it **accepts** an AI job it **fully stops**
mining for the job's duration, then **resumes** when the job completes. **No profitability
calculation.** And **one GPU = one job** (a machine accepts at most ONE concurrent job).

**Changes**
1. **Node miner — full stop while a job runs** (`gpn-node/node/src/service.rs`):
   - `spawn_gpu_miner` (and the CPU fallback `spawn_cpu_miner`) now `continue` the loop while
     `gpu_yield_requested()` is true — **no nonce search at all** — instead of the old
     "sleep then still mine once per cycle" duty cycle. The miner thread stays alive, so mining
     resumes on the very next loop once the flag clears. Stale comment removed.
   - `GPU_YIELD_SLEEP_MS` 2500 → 500 (it is now purely the poll interval, so resume is snappy).
2. **Providers hold the flag for the WHOLE job duration** (accept → complete/fail/error):
   - New RAII helper `gpn_agent::gpu_yield::GpuYieldGuard` (`gpn-agent/src/gpu_yield.rs`) writes
     `~/.gpn/gpu-yield` on construction and removes it on `Drop` — so the flag is held on every
     return path. Wired into `gpn_llm_provider.rs` and `gpn_diffusion_provider.rs` (replaces the
     old write-immediately-before-inference / remove-right-after pattern, which left settlement
     un-covered).
3. **One GPU = one job**
   - Both providers' claim loops now `break` after accepting **one** job per pass (a second is
     not accepted while one is accepted/running; `run_job` is awaited to completion).
   - Provider heartbeat advertises `--capacity 1`:
     `~/.config/systemd/user/gpn-provider-heartbeat.service` (was `4`),
     `run-windows-provider.ps1`, and `.github/workflows/windows-provider-start.yml`.
     (`gpn_provider_status` already defaults to 1; the Windows box inherits it.)

**Verification (live, Linux node + local provider)**
- Controlled: `touch ~/.gpn/gpu-yield` for 35 s → **zero** `✅ Successfully mined` lines; block
  production resumed within ~3 s of `rm` (head #128552 → #128553).
- Before (old node): identical test → blocks still mined while the flag was ON
  (#128547 area, 13:32:30 + 13:32:41 during the ON window) — confirmed the old duty cycle.
- End-to-end job via AP `POST /jobs`: flag held for the whole job (inference → complete),
  no mining during the job, mining resumed immediately after. Heartbeat `capacity=1`, `load 0/1`.

**Remaining manual step:** the Windows box picks up the new bins + `--capacity 1` via the
`release-windows.yml` workflow (deploys to `C:\gpn\bin`) + `windows-provider-start.yml`; Jeremy
restarts the provider afterwards.
