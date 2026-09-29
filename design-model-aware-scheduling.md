# GPN Model-Aware Scheduling + On-Demand Model Pulls

## Goal
1. Providers **advertise which models they have** (Ollama inventory) in their heartbeat.
2. Client-side model selection:
   - **No model requested** → run on *any* provider that has *any* usable model.
   - **Specific model requested** → pick the **fastest** provider that HAS it.
   - **No provider has it** (or one could *download it and finish faster*) → pick a provider that
     **can** run it, and have that provider **auto-download the model** (Ollama pull / HuggingFace).

## Piece 1 — Provider model inventory (heartbeat)
`gpn_provider_status /status` reports `models: [...]` **dynamically** from the local Ollama
(`GET http://127.0.0.1:11434/api/tags`) instead of a static `--models` list. Falls back to the
`--models` flag if Ollama is unreachable. So the registry always reflects reality.

## Piece 2 — Model-aware scheduler (`pick`)
Inputs per job: `model` (optional), `job_type` (llm/image/…), `min_gflops`.
1. **Compatible** = attested_gflops ≥ min_gflops (and has free capacity).
2. **Model match**:
   - model specified → providers whose `models` contains it.
   - model unspecified (`"auto"`/empty) → any compatible provider that has ≥1 model for that kind.
3. **Score** = latency + load (fastest + least-busy), as today.
4. If **no provider has the model**, fall to the **pull-eligible** set: providers whose heartbeat
   advertises `can_pull: true` (Ollama present + disk/VRAM headroom) — score those by
   latency + load + a **pull-cost penalty** (`model_size / link_speed`).
   → If a *ready* provider exists but a *pull-eligible* one is cheaper end-to-end
     (`pull_penalty < ready_wait`), the puller wins (Jeremy's "download + be faster" rule).

## Piece 3 — On-demand pull (provider side)
When a provider receives an assignment for a model it doesn't have:
1. It runs `gpn_pull_model pull <model>` (Ollama) — or `gpn_pull_model gguf <hf-repo> <file>` for a
   HuggingFace GGUF that isn't an Ollama-library model.
2. It then proceeds with the existing run loop.
Providers declare `can_pull` in their heartbeat only if Ollama is installed + there's disk room.

## Dispatch contract
`/route/{id}` (and the hot-wallet `/jobs`) already write the assignment `{job_id → provider_addr}`.
Extend the assignment value to carry the **model + whether a pull is needed**, so the provider knows:
```json
{ "7": { "provider": "5ENq…", "model": "llama3.2:1b", "pull": true } }
```
(Backwards-compatible: a bare string still parses as `{provider, model:null, pull:false}`.)

## Why this matters
- Clients don't need to know which machine has what — they ask for a **model** (or "any").
- The network **self-heals**: a requested model absent everywhere gets **pulled on demand** by the
  best-suited machine, so the catalog grows organically toward demand.
- Same rails as today: chain = escrow + coordination; scheduler = routing; provider = compute.

## Build order
1. Dynamic model inventory in `gpn_provider_status` (query Ollama).
2. `pick` model-aware (specified vs auto vs pull-eligible) in `gpn_scheduler`.
3. Assignment carries `{model, pull}`; provider pulls on demand before running.
4. Score pull-cost vs ready-wait (the "download + be faster" tie-break).
