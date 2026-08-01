# Cycle 32 — Tokenomics Spec Aligned to Implemented Code

**Date:** 2026-08-01 ~06:00 UTC (2026-07-31 ~23:00 PT)
**Caretaker:** PM07

## Context / Brief correction
The cron brief for this cycle described the project as **Phase 1 Research, greenfield,
"no code exists yet"**. That is stale — the project is at **Cycle 31, Phase 3 (Prototype)**
with a live, verified end-to-end system on the RTX 4060 (real Ethash GPU mining + chain-feed
auto-switch + GPU attestation + marketplace job serving + two-account escrow payout). All
Phase 1 research items in the brief (GPU coins, compute networks, hybrid consensus) were
already completed in Cycles 1–6 and are documented in `shared/artifacts/project7/`.

Per the SOUL.md directive ("find the next task, manage it, complete it; momentum beats
perfection"), I did **not** re-run finished research. I audited the actual state and found a
real gap to close.

## What was done
Audited the repo and identified that the **`design/tokenomics/` directory is empty** despite
being referenced in README's project structure, and that the "Preliminary" tokenomics table
in the design-synthesis doc **does not match the implemented `pallet-gpn-token` code**.

Wrote **`design/tokenomics/01-gpn-tokenomics.md`** — a source-of-truth tokenomics spec that
tracks the actual implementation:

- **Supply:** fair-launch, 0 genesis, all GPN minted via `mint_block_reward` (root-only, PoW)
- **Inflation:** Initial 10% → Terminal 1%, decays 0.5%/yr via `update_inflation()`
- **Block reward:** 50 GPN/block @ 15s → ~288,000 GPN/day at launch
- **Fee split:** 5% marketplace fee → 200/150/100/50 bps (staking pool / treasury / burn / oracle)
- **Stake params:** `MinAttestationStake = 1,000 GPN`, `MarketplaceFeeBps = 500 (5%)`
  (both verified from runtime `lib.rs`)
- **Reconciliation table** mapping preliminary (paper) vs implemented (code) — documents the
  deliberate deviation (fee-driven + inflation-decay model preferred over fixed allocations)
- **Open questions for Phase 4** (supply cap, real burn, staking payout, oracle payout, dev
  faucet removal)

## Verification
- Confirmed constants from source: `MinAttestationStake = 1_000_000_000_000_000_000_000`
  (1,000 GPN), `MarketplaceFeeBps = 500` (5%), `InitialBlockReward = 50 GPN`, inflation
  10%→1%, fee default 200/150/100/50.
- No code changed; spec only. Git tree was clean before and after apart from the new doc.

## Next actions (priority)
1. **Fill remaining empty design dirs** — `design/pallets/`, `design/protocols/`,
   `design/node-agent/` are scaffolded but empty (README references them).
2. **Phase 4 prep items** from the spec's Open Questions: wire real burn, per-staker
   staking payout keyed by GPU tier, oracle reward distribution, remove dev faucet on mainnet.
3. Re-run `cargo test` + a live smoke test next hardware cycle to confirm no drift.
