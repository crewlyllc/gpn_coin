# GPU Attestation Protocol (Proof of Staked GPU) — v1 (Aligned to Implemented Code)

**Status:** ✅ Matches `pallet_gpu_attestation` (Cycle 32 audit)
**Date:** 2026-08-01
**Author:** PM07

> Staking that requires **proof of real GPU compute**, not just locked tokens. Source of
> truth = `gpn-node/pallets/gpu-attestation/src/lib.rs`.

---

## 1. Why GPU Attestation

Plain proof-of-stake can be gamed by wealthy holders with no hardware. GPU attestation ties
staking weight to **demonstrated GPU compute**, providing:
- **Sybil resistance**: each staker must run a real GPU and prove measurable GFLOPS.
- **Hardware alignment**: the network is staked by the same GPUs that mine and serve AI.
- **Reputation**: attested GFLOPS feeds the marketplace `min_gflops` gate, so only
  provably-capable GPUs win jobs.

## 2. Protocol Flow

```
deposit_stake(≥ 1,000 GPN)   ──▶ ProviderStake[who] locked
        │
        ▼
submit_attestation(record)   ──▶ Attestations[who] = AttestationRecord
         (gpu_count, compute_gflops, tier)      (measured by agent's GPU monitor)
        │
        ▼
attested_gflops / attested_tier   ──▶  consumed by compute-marketplace (min_gflops gate)
        │
        ▼
withdraw_stake()   (after check_expired clears attestation)
```

## 3. Key Parameters

| Parameter | Value | Notes |
|-----------|-------|-------|
| `MinAttestationStake` | **1,000 GPN** | Bond required before attesting |
| `min_gpus` | configurable | Minimum GPU count for a valid record |
| `min_gflops` | configurable | Minimum compute for a valid record |
| Tiering | from measured GFLOPS | e.g. Bronze @ ~188–294 GFLOPS on RTX 4060 |

## 4. GFLOPS Measurement (Cycle 24)

- The agent measures **real GFLOPS** via a GPU matmul benchmark (`gpu/matmul.rs`).
- Units fixed in Cycle 24: raw FLOP/s leaked `×1e9` and silently broke the `min_gflops`
  gate; now reported as true GFLOPS and the gate is verified both positive and negative
  live (Cycle 28).
- Measured GFLOPS is the on-chain attestable compute metric.

## 5. Cross-Pallet Contract: `GPUAttestationProvider`

`gpu-attestation` exposes a public read-only trait:

```rust
trait GPUAttestationProvider<AccountId> {
    fn attested_gflops(who: &AccountId) -> u64;
    fn attested_tier(who: &AccountId) -> Tier;
}
```

`compute-marketplace` consumes it via `Config::GPUAttestation`. `accept_job` rejects with
`Error::ProviderComputeInsufficient` when `attested_gflops < job.gpu_requirements.min_gflops`.
`min_gflops = 0` skips the gate (open market). 0 GFLOPS = never attested = blocked from
gated jobs. (Cycle 26; +4 gate tests.)

## 6. Attestation Record

`Attestations: Map<AccountId, AttestationRecord>` where the record carries:
`gpu_count`, `compute_gflops`, `tier`, plus stake linkage via `ProviderStake`.

## 7. Staking Weight / Rewards (open items)

- `TotalStaked` tracks aggregate bonded GPN.
- `effective_stake(who)` = attested staker's bonded balance.
- **Open (Phase 4):** per-staker staking reward payout keyed by GPU tier multiplier
  (see tokenomics Open Questions) — `distribute_staking_rewards` currently drains the pool
  and emits an event.

## 8. Verification

- gpu-attestation **20 tests pass**.
- Live on RTX 4060 (Cycle 27): `staked=true attested=true`, `gpus=1 gflops=188667299223
  tier=Bronze`, balance `996,999,999,999.9994 GPN` (= 1T − 1,000 stake).

## 9. Related

- Marketplace gate: `design/protocols/03-compute-marketplace.md`
- Tokenomics / stake params: `design/tokenomics/01-gpn-tokenomics.md`
- Implementation: `gpn-node/pallets/gpu-attestation/src/lib.rs`
