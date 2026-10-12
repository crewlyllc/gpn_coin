# GPN Access Points + Job Scheduler — Design

## The problem (what Jeremy hit)
Today a user must know a specific node's RPC/IP to submit a job (`gpn_api` on :8090), and
whichever provider claims first wins (no routing). That's bad UX and no load balancing.

## Goal
1. **Access points** — one or more well-known public endpoints where anyone can submit a
   job WITHOUT knowing any node/provider IP. Multiple = redundancy + geographic spread.
2. The **network picks which provider** runs the job, by:
   - **Compatibility** — the provider *can* do the job (GFLOPS ≥ job min, model present, GPU type).
   - **Latency** — the fastest / most responsive provider.
   - **Load** — the least-busy provider (spread the work).

## Design

```
  customer ──> [ access point 1 ] ─┐
              [ access point 2 ] ─┤  scheduler: filter (compat) → score (latency+load) → pick
              [ access point N ] ─┘
                                     └──> chosen provider (accept → run → settle)
        ▲ providers register + heartbeat (capabilities + load + latency)
```

### 1. Provider heartbeat (each provider agent)
The provider agent exposes a tiny `GET /provider/status` (HTTP) returning:
`{ peer_id, attested_gflops, models[], gpu, current_jobs, capacity, last_seen }`.
Cheap, unauthenticated, local — the scheduler polls it to learn live load + measure latency.

### 2. Provider registry (the scheduler)
- **Capabilities** come from the **on-chain attestation** (source of truth: GFLOPS, GPU, tier).
- **Load + latency** come from the `/provider/status` heartbeat (off-chain; not on-chain).
- Registry = `{ provider_addr → {capabilities, load, latency_ms, endpoint} }`, refreshed every N s.

### 3. Scheduler (at each access point)
On job submission, the scheduler:
1. **Compatibility filter**: keep providers with `attested_gflops ≥ job.min_gflops` and the
   required model/GPU available.
2. **Score**: e.g. `score = w1·(1/latency) + w2·(free_capacity/total)` (latency + lowest load).
3. **Pick** the best provider; fall back to round-robin if scores tie.
4. **Create the on-chain job** (escrow from the customer) — the chain stays the coordination+escrow layer.
5. **Dispatch** the job off-chain to the chosen provider (a `POST /jobs/{id}/assign` on that
   provider, or the provider polls for "assigned to me"), which then accept → start → run → settle.

### Access point = `gpn_scheduler` (new bin)
- Runs the **same job API** as `gpn_api` (`POST /jobs`, `GET /job/{id}`, …) so a customer points at
  ONE URL and never sees provider IPs.
- Adds the scheduler + registry behind it.
- Stateless-ish: any access point can be hit; they all consult the same on-chain state + their
  own provider heartbeat views.

## On-chain vs off-chain split
- **On-chain (source of truth):** jobs, escrow, provider attestations/capabilities, payouts.
- **Off-chain (scheduler-only):** latency, live load, endpoint discovery — things the chain can't see.
- The chain *enforces* capability gates (`accept_job` min_gflops); the scheduler *optimises* latency/load.

## Why multiple access points
- **Redundancy** (one down ≠ network down), **geo-latency** (hit the nearest), and **DDoS spread**.
- Anyone can run one — it's just a `gpn_scheduler` pointed at the chain; it needs no special trust.

## Build order (incremental)
1. Provider `/provider/status` heartbeat (agent).
2. `gpn_scheduler` with registry + compat filter + naive pick, exposing the job API.
3. Latency + load scoring.
4. Dispatch-to-chosen-provider (vs the current first-claim-wins).
5. (later) Multi-access-point discovery (a published list of access points).
