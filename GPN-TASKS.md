# GPN-TASKS.md — PM07 engineering queue

Numbered engineering queue for the GPN testnet (repo: gpn_coin / projectmanager07).
Maintained by the PM07 agent; each item is smallest-correct-change, committed + pushed.

> Mirrors the shared-workspace queue (`shared/artifacts/project7/GPN-TASKS.md`).

---

## #24 — Simplified node behavior: full-stop mining during a job; one GPU = one job — DONE (2026-10-13)

**Spec (Jeremy):** a node mines by default; when it **accepts** an AI job it **fully stops**
mining for the job's duration, then **resumes** when the job completes. **No profitability
calculation.** And **one GPU = one job** (a machine accepts at most ONE concurrent job).

This is the "auto-yield the one GPU" premise: a node **mines and serves AI**, and the GPU is
handed to whichever job is running — one job at a time.

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
     return path. Wired into `gpn_llm_provider.rs` and `gpn_diffusion_provider.rs`.
3. **One GPU = one job**
   - Both providers' claim loops now `break` after accepting **one** job per pass.
   - Provider heartbeat advertises `--capacity 1`.

**Verification (live, Linux node + local provider)** — flag ON 35 s → **zero** mined blocks;
resume on clear; e2e job holds the flag for the whole job, mining stops during, resumes after;
heartbeat `capacity=1`.

---

## #25 — Single-node supervisor: one command runs the whole node — DONE (2026-10-13)

**Spec (Jeremy):** an operator runs **one command** and gets **one node** — the chain
node/miner, the provider heartbeat, and the provider loop(s) — started, logged (one prefixed
stream), monitored (one status line) and stopped cleanly, together.

**Design correction (Jeremy, 2026-10-13):** **a GPN node is ALWAYS both a miner and an AI
provider — there is NO role option.** The first cut shipped `--role both|mine|provider`
(+ `GPN_ROLE`); that was wrong. Every start brings up the miner **and** the AI serving stack.

**Changes**
1. **New binary** `gpn-agent/src/bin/gpn_node.rs` — supervises the whole node as **separate
   child processes** (fault isolation kept: an AI-stack crash never takes the chain node down):
   - `[node]` — the chain node/miner (`gpn-node … --force-authoring …`);
   - `[heartbeat]` — `gpn_provider_status` (advertises capability + load, `:8093`);
   - `[provider]` — `gpn_llm_provider` (claim + run + settle);
   - `[diffuse]` — `gpn_diffusion_provider` (only when `diffusion = true`).
   It reads one small TOML config (`--config` / `$GPN_CONFIG` / `$GPN_PREFIX/node.toml` /
   `./gpn-node.toml`) plus `GPN_*` env overrides, restarts a crashed child with bounded backoff
   (1s→30s), prints one aggregated status line, and shuts all children down cleanly on
   Ctrl-C / SIGTERM (SIGTERM → wait → SIGKILL). Secrets stay in env / the config file.
   - **ALWAYS mine + serve**: no `--role` flag, no `GPN_ROLE`, no role branching. All children
     are started on every run.
2. **Docs / scripts** — every "role" removed; a node **is** mine + serve:
   - `docs/run-a-node.md` (Linux + Windows one-command flow; "What it starts (always)" table),
   - `gpn-node.example.toml` (role field dropped),
   - `install.sh` (start hint → `gpn_node`, no `--role`),
   - `run-windows-provider.ps1` — now runs `gpn_node.exe` (mine + serve together),
   - `.github/workflows/windows-provider-start.yml` — the scheduled-task supervisor runs
     `gpn_node.exe` (mine + serve together),
   - `run-windows.bat` — starts `gpn_node.exe` (mine + serve together),
   - `website/deploy.html` + `website/download.html` — reframed: a node is miner + provider.

**Verification (isolated: dev chain + throwaway ports/base-path; live fleet untouched)**
- `cargo build --release --bin gpn_node` clean.
- Run with no role flag → **always** brings up node + heartbeat + provider.
- kill -9 a child → restart; SIGINT → all children stopped cleanly.

---

## #26 — Fold the children into a single binary — DEFERRED

Optional later: run node + heartbeat + provider in-process (one binary) instead of the
supervisor's separate child processes. **Unchanged by #25's correction** — the supervisor's
"always mine + serve" behaviour is independent of how many processes implement it.
