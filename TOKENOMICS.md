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
| Mining allocation | **900,000,000 GPN** (90%) via GPU PoW |
| Treasury allocation | **100,000,000 GPN** (10%) — see below |
| ICO / token sale | **None** — no sale, no hidden allocation |

## Emission

- Block reward: **50 GPN/block**. Runtime target block time is **4 s** (shared testnet/mainnet
  cadence) → **~1,080,000 GPN/day** at target. (Older docs said "~15 s → ~288,000/day"; the
  15 s cadence is **not** implemented — see the cadence note below.)
- Reward **halves every 4,204,800 blocks** — that is ~2 years at 15 s but **~194 days at the
  current 4 s cadence**; the block-time/halving parity is an **open mainnet decision**
  (`docs/mainnet-launch-runbook.md` §10).
- All newly issued GPN comes from mining block rewards; there is no other mint.

## Treasury (10% — a growth fund, not a payout)

100,000,000 GPN (10% of the cap) is allocated at genesis to a **keyless treasury
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

**Unlock schedule:** **70M unlocks at genesis** (immediately spendable via 13-of-25
governance) and the remaining **30M vests over 10 years** (20 × 1.5M six-month
tranches). Amount, address, and schedule are all verifiable on-chain.

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

Mainnet has **no live `sudo` key** and **no endowed accounts beyond the disclosed
treasury** — supply is issued only through mining, and changes happen only through the
timelocked governance above.

---

*This document is the public disclosure of GPN's token allocation and emission.
Detailed design: `docs/` and the project's design artifacts.*
