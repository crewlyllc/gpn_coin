# Compute Marketplace Protocol — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches `pallet_compute_marketplace` (Cycle 32 audit)
**Date:** 2026-08-01
**Author:** PM07

> The decentralized GPU job marketplace: requestors pay, providers serve AI compute, escrow
> guarantees payout. Source of truth = `gpn-node/pallets/compute-marketplace/src/lib.rs`.

---

## 1. Model

**Hybrid auction + queue** (per README key design decision), implemented as an
**order-based job registry** with **escrow** and a **reputation gate** (attested GFLOPS).
Requestor posts a job with a price; any qualified provider claims it.

## 2. Two-Party Flow (real escrow)

```
REQUESTOR (Bob)                         PROVIDER (Alice)
     │                                        │
     │ create_job(price, min_gflops, ...)     │
     │   escrow debited → JobEscrow           │
     ▼                                        │
  [Pending]                                   │
     │◄─────────────── accept_job (gate: attested_gflops ≥ min_gflops)
     │                 ProviderComputeInsufficient if not qualified
     ▼                                        │
  [Accepted]                                  │
     │◄─────────────── start_job (work begins)
     │                 runs real GPU GEMM (matmul)
     ▼                                        │
  [Running]                                   │
     │◄─────────────── submit_result(result_hash)  (deterministic hash)
     │                 complete_job
     ▼                                        │
  [Completed]  ─── payout to provider ────────┤
                  price − 5% MarketplaceFeeBps
                  fee → gpn-token::distribute_fees
```

Job states: `Pending → Accepted → Running → Completed` (+ `Cancelled` via `cancel_job`).

## 3. Extrinsics

| Call | Actor | Effect |
|------|-------|--------|
| `register_provider(requirements)` | provider | Declare `GPURequirements`; idempotent |
| `unregister_provider()` | provider | Leave the market |
| `create_job(...)` | requestor | Post job; escrow price debited |
| `accept_job(job_id)` | provider | Claim job (min_gflops gate enforced) |
| `start_job(job_id)` | provider | Begin work (bounds state machine) |
| `submit_result(job_id, result_hash)` | provider | Commit deterministic result |
| `complete_job(job_id)` | provider | Finalize → escrow payout + fee |
| `cancel_job(job_id)` | requestor | Abort (escrow refund path) |

## 4. Pricing & Fees

- Requestor sets the job price (in GPN) at `create_job`; full price is escrowed up front.
- On completion, provider receives `price − 5%` (`MarketplaceFeeBps = 500`).
- The 5% fee is split by `gpn-token::distribute_fees` (staking pool / treasury / burn /
  oracle — see tokenomics spec §3).

## 5. Reputation / Qualification Gate

- `GPURequirements.min_gflops` at job creation.
- At `accept_job`, `compute-marketplace` queries `GPUAttestationProvider::attested_gflops`
  (from `gpu-attestation`). Below requirement → `ProviderComputeInsufficient`.
- `min_gflops = 0` → open market, any registered provider can claim.
- Result integrity: providers submit a **deterministic result hash**; the prototype uses a
  real GPU GEMM and commits its hash (malicious-result slashing is a Phase-4 item).

## 6. Storage (source of truth)

`NextJobId`, `Jobs`, `JobRequestor`, `JobProvider`, `ProviderJobs`, `RequestorJobs`,
`JobEscrow`, `RegisteredProviders`, `ActiveJobCount`.

## 7. Agent Integration

- In `Compute` mode, `ChainClient::list_claimable_jobs(attested_gflops)` enumerates
  `0..next_job_id`, keeps only `Pending` jobs the provider can satisfy (open market or
  `attested ≥ min_gflops`).
- `claim_and_serve_jobs()` runs the full accept→start→real GPU GEMM→submit→complete loop
  and is wired into the auto-switch tick (Cycle 29).
- Escrow/payout verified live two-party (Cycle 30): Bob escrows 50 GPN → Alice earns 47.5.

## 8. Verification

- compute-marketplace **21 tests pass** (incl. 4 min_gflops gate tests).
- Live on-chain: job #1/#3/#4 served and paid; two-account demo green (Cycles 28–31).

## 9. Related

- Attestation gate: `design/protocols/02-gpu-attestation.md`
- Auto-switch: `design/node-agent/01-agent-architecture.md`
- Fees: `design/tokenomics/01-gpn-tokenomics.md`
- Implementation: `gpn-node/pallets/compute-marketplace/src/lib.rs`
