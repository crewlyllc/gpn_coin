# GPN Tokenomics

GPN is the native token of the **GPN network** — a network where a GPU does one of two
things, automatically switching to whichever pays more:

- **Mine** — GPU proof-of-work secures the chain and mints GPN (the *coin*).
- **Serve AI compute** — the same GPU earns by running AI/ML workloads on the on-chain
  compute marketplace (the *product*).

That dual nature is the core thesis: GPN has **utility-backed demand** (people paying
for GPU compute), not just speculative demand.

---

## Supply

| Parameter | Value |
|---|---|
| Hard cap | **1,000,000,000 GPN** (1B) |
| Mining allocation | **800,000,000 GPN** (80%) via GPU PoW |
| Treasury allocation | **200,000,000 GPN** (20%), vested (see below) |
| ICO / liquid premine | **None** — no sale, no hidden allocation |

## Emission

- Block reward: **50 GPN/block**, ~15s target → **~288,000 GPN/day** (~105M in year 1).
- Reward **halves every ~4,204,800 blocks (~2 years)**.
- All newly issued GPN comes from mining block rewards; there is no other mint.

## Treasury (20% — a growth fund, not a payout)

200,000,000 GPN (20% of the cap) is allocated at genesis to a **keyless treasury
account** (`gpn/trsy`) — it has no private key and can only spend via on-chain
governance (**13-of-25** signers).

**Purpose — promote GPN on two fronts:**
1. **As a coin** — marketing, advertising, exchange listings, community and liquidity
   programs.
2. **As an AI compute tool** — promoting the GPU marketplace to AI/ML developers and
   companies, developer relations, onboarding compute buyers *and* sellers, and
   partnerships that bring real AI workloads onto the network.

Supporting uses: ongoing development, security audits, bug bounties, and a multi-year
operating runway.

**Vesting:** the 200M unlocks in **28 six-month tranches over ~14 years** — **0 unlocked
at launch**. Phase 1: 100M over 48 months (8 × 12.5M). Phase 2: 100M over 120 months
(20 × 5M). Amount, address, and schedule are all verifiable on-chain.

## Fees

A **5%** marketplace fee on AI-compute job payouts, split:

| Destination | Share |
|---|---|
| Staking reward pool | 40% |
| Treasury | 30% |
| Burn | 20% |
| Oracle operators | 10% |

## Governance

- **Upgrades / parameter changes** — a **committee (2-of-3)** can propose a change;
  it executes as `Root` only after a **3-day timelock**. No single key and no admin
  backdoor (mainnet `sudo` is inert).
- **Treasury spending** — **13-of-25** governance.
- **Oracle (price feed)** — a closed set of operators (committee-admitted) plus
  permissionless bonded operators; the on-chain median is authoritative.

## No admin backdoor

Mainnet has **no live `sudo` key** and **no endowed accounts** — supply is issued only
through mining, and changes happen only through the timelocked governance above.

---

*This document is the public disclosure of GPN's token allocation and emission.
Detailed design: `docs/` and the project's design artifacts.*
