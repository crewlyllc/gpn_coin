# GPN — Gitcoin Grant Application

*Copy for a Gitcoin grant (and reusable for DoraHacks / Optimism RetroPGF / EF ESP). Gitcoin
funds **public goods / open-source**; frame GPN as open-source infrastructure, not a token sale.*

---

## Project name
**GPN — a GPU-native network for mining and AI compute**

## One-liner
An open-source, trust-minimal network that lets any GPU **mine _or_ serve AI compute — auto-switching**
to whichever pays more, so idle GPUs become income and AI compute gets cheaper.

## What is the project?
GPN is an open-source **Coordination layer + compute marketplace** built as a Substrate blockchain:

- **Chain = coordination + escrow + settlement; compute = off-chain.** The heavy AI work never touches
  the chain.
- **One GPU, two modes.** A node **mines** (GPU proof-of-work, Bitcoin-style per-node rewards) **or
  serves AI** (LLM inference, image/video generation, ML inference, fine-tuning) and **auto-switches**
  between them based on which pays more.
- **Trust-minimal money.** Per-token escrow, optionally via **payment channels** (no per-job block
  delay); either side can force-settle on-chain.
- **GPU-only, verifiably.** Proof-of-work + hardware attestation + a compute challenge.
- **Privacy.** A shielded pool for private transfers/withdrawals.

## Why does it matter? (the public-goods angle)
- **Energy/asset efficiency:** the same hardware serves the most profitable real demand instead of
  idling or mining at a loss.
- **Cheaper, more distributed AI compute:** consumer GPUs are the cheapest compute on earth and sit
  idle; GPN routes real AI work to them, broadening access beyond hyperscalers.
- **Open, reusable infrastructure:** a reference implementation of **blockchain-coordinated,
  trust-minimized compute markets** — escrow, capability gates, routing, privacy — that any
  decentralized-compute project can learn from or build on. **All open-source.**
- **Decentralization:** permissionless access points, Bitcoin-style peer discovery, and a
  self-organizing full-node layer.

## How is it open-source / what is the deliverable?
- Fully open **Rust/Substrate** codebase (node, pallets, agents), and **open design docs**.
- Concrete, verifiable deliverables: the chain + runtime, the job marketplace, the provider/AP agents,
  the **ceremony tooling**, and the **docs/onboarding**.

## Funding needs & milestones
| Amount | Milestone | Outcome |
|---|---|---|
| **$15k** | Independent **security audit** (pallets + agents) | publicly published audit report + fixes |
| **$10k** | **Trusted-setup ceremony** (CRS) + tooling hardening | trustless privacy layer; ceremony transcript verifiable |
| **$25k** | **Mainnet readiness** (params, genesis, runbooks, monitoring) | mainnet launch candidate |
| **$10k** | **Docs + onboarding + access-point tooling** | one-command install for node/provider/AP |
| **$10k** | **Seed full nodes / CI GPU** (decentralization + testing) | a resilient, testable network |

*(Total ask ≈ $70k. Any grant size is useful — even a partial round directly funds audits + the ceremony.)*

## Impact metrics (what we'll report back)
- Network: nodes/providers/APs live; **GPU-hours served**; **jobs completed**; escrow settled.
- Cost: effective **$/1M tokens** delivered vs a market baseline.
- Decentralization: independent full nodes; geographic spread of access points.
- Security: audits completed + findings fixed; the ceremony transcript.
- OSS: contributors, releases, docs, reuse by other projects.

## Milestone-based honesty
We report **what shipped**, not promises. Current state = **live on a public testnet** with the
marketplace, channels, shielded spends, discovery, and governance all functional; next = audits, the
ceremony, and mainnet.

## Team
*(Add founder/contributors + links.)* GPN has been built and operated end-to-end by a small team; the
network runs on self-hosted hardware today.

## Links
- Website: *(add)* · GitHub: *(add)* · Whitepaper: *(`GPN-WHITEPAPER.md`)* · Testnet status: *(add)* · Docs: *(add)*

## Grant pool fit
- **Gitcoin (OSS rounds):** open-source dev + the ceremony tooling + docs.
- **Optimism RetroPGF / EF ESP / DoraHacks:** open, reusable public-good infrastructure
  (chain-coordinated compute markets) with measurable impact.

## Notes
No tokens are sold; **no financial return** is offered or implied. Grant funds go to development,
audits, and decentralized infrastructure.
