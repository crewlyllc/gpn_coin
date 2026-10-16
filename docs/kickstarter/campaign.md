# GPN — Kickstarter Campaign (full draft)

**Project:** GPN (Project 7) · **Package version:** v1 draft · **Drafted:** 2026-10-09
**Status:** DRAFT for Jeremy's review. Nothing posts until Jeremy clears accounts, timing,
and the tokenomics wording. Aligned to `marketing/brand-voice-and-messaging-guide.md`.

> **Positioning note (read before publishing).** Kickstarter's rules restrict projects that
> promote or involve cryptocurrency/blockchain, and its reward rules forbid offering tokens,
> equity, or any financial return as a reward. This campaign therefore presents **GPN as a
> software + AI-compute platform** — "turn your GPU into income by mining **or** serving AI" —
> and offers **only non-financial rewards** (early access, compute credits, swag, tier
> benefits, a supporter wall). **No tokens are sold or offered here**, and no price, listing,
> or return is promised. See `README.md` for the full policy note and fallback platforms.

---

## 1. Title, tagline, blurb

**Title (≤60 chars, 34):**
> **GPN — Mine or Serve AI With One GPU**

**Subtitle / tagline:**
> *A GPU should not have to choose.*

**Short blurb (project card, ~131 chars):**
> GPN turns one GPU into two income paths: it mines GPN or serves AI compute, auto-switching
> to whichever pays more. Live on testnet.

**One-paragraph summary:**
> Most GPU owners have two ways to earn and are forced to pick one: mine a coin, or rent the
> card for AI work. Point it at mining and a price dip means mining at a loss; list it for AI
> and an empty queue means zero. GPN is open-source software that ends the guesswork — the
> same GPU mines GPN **or** serves AI compute (LLM inference, image/video generation,
> fine-tuning), **auto-switching** to whichever pays more, with jobs coordinated and settled
> by a trust-minimal chain. We're funding the last mile to mainnet: finishing the auto-switch,
> independent security audits, the shielded-pool ceremony, and the seed full nodes that keep
> the network reachable. The network is already live on a public testnet.

---

## 2. The story

### The problem: two ways to earn, and you have to pick one

Owning a GPU presents two ways to earn, and they have always been **different products**:

1. **Mine** a proof-of-work coin — you're paid whatever the coin is worth today.
2. **Rent** the GPU for AI work — you're paid whatever compute is worth today.

The catch is that **you have to pick one.** Point the card at mining, and if the coin dips you
mine at a loss. List it on a compute marketplace, and if nobody books it, it earns $0. The card
can't see that "the other side is paying more right now" and move.

Meanwhile AI compute is expensive and concentrated: buyers pay hyperscaler prices, while
consumer GPUs — some of the cheapest compute on earth, verified RTX 4090 AI rentals at
**$0.14–0.34/hr** — sit idle roughly 20 hours a day. That silicon is already paid for. It just
earns nothing while it sleeps.

And mining became a one-way bet. Post-Merge, GPU miners were pushed toward coins whose only
story is "number goes up." A coin with no job behind it is a bet, not a product — and when the
block reward thins, there's nothing left but exit.

### The idea: don't make the owner pick

**GPN is a network where every GPU mines *or* serves AI — and auto-switches to whichever earns
more.** Same card, one install, no manual re-listing. The chain does coordination, escrow, and
settlement; the heavy compute runs off-chain.

That single idea — **the auto-switch** — is the network's whole thesis. It's the one thing no
GPU-compute competitor does, and it's what turns a coin into a *product*.

### How it works

**One node, one mode at a time.** A GPN node is one entity — one GPU, one identity, one wallet —
running in one of three modes: **Mining** (hash blocks, mint new GPN), **Compute** (serve AI
jobs, paid from job escrow), or *(later)* **Staking**. At launch the practical switch is
**Mining ⇄ Compute**.

**The switch is a decision, not a guess.** Every cycle the node estimates each mode's revenue in
**USD/hr**, using an on-chain oracle feed of token price, GPU-hour rental price, utilization, and
staking APR, and switches **only when the other mode beats the current one by ~5%** (a hysteresis
margin) — so the GPU doesn't thrash on tiny price crossings. A switch is a real **stop/start of
GPU work**: entering Compute stops the miner and frees VRAM before the model loads.

**The chain is thin and trust-minimal.** It holds the job lifecycle, escrow, and payouts, enforces
capability and eligibility gates, and settles rewards. Inference, generation, and training
**never touch the chain**.

**Mining, Bitcoin-style.** GPN uses a memory-hard, Ethash-derived GPU proof-of-work; anyone can
mine and there's no validator set. The node that produces a block **keeps that block's reward**,
paid to its own account (per-node wallets). 15-second blocks, self-tuning difficulty, and a
halving every ~2 years.

**GPU-only, verifiably.** Earning requires proving you're a real GPU, not spoofable and not a
CPU/iGPU. Eligibility layers proof-of-work, hardware attestation (GPU count + measured GFLOPS →
Bronze/Silver/Gold tiers), and a random **capacity challenge** that makes throughput *proven*
rather than self-reported. Staking is **optional** — a lever that raises your tier and reward
share, not a gate.

### The AI compute marketplace

A requestor submits a job and the chain runs it through a lifecycle: `Pending → Accepted →
Running → AwaitingVerification → Completed` (plus cancelled/failed states).

- **Per-token escrow.** The requestor escrows `rate × max_output_tokens` as a cap; the provider
  meters **actual** usage; the charge is `rate × actual`; the **surplus auto-refunds**. Priced in
  dollars, settled in GPN, so a buyer's *dollar* cost is stable.
- **Cost-derived pricing.** Rates come from a roofline estimate of the provider's true throughput,
  divided by realized tokens/sec, times a margin — targeting roughly **half of comparable market
  GPU-rental**, and clamped at or above the provider's cost so a GPU **at least covers its cost**.
- **Least-latency routing.** A buyer hits a single access point and never needs a provider's
  address. The scheduler filters by capability (GFLOPS, VRAM/RAM, model availability), requires
  the node to be idle (one GPU, one job — i.e. one job *at a time*, mutual exclusion, **not** a limit
  on job types: every node serves every job type by design), then picks the **lowest-latency** eligible provider.
  Providers **register and advertise their endpoint on-chain**, so access points discover them
  from the chain — not a hand-maintained list.
- **Model on demand.** Providers can pull a model on demand (via Ollama/Hugging Face), so
  requestor demand grows the catalog organically.

**Trust-minimal escrow, and no block delay.** On-chain, a job costs one transaction per step
(create/escrow → accept → settle). GPN also supports **payment channels**: funds are locked
**once** on-chain, and each job is settled **off-chain** by mutually-signed state updates. Either
party can force-settle on-chain, so the chain remains the root of trust — and the AI path can run
with **zero per-job transactions**, i.e. no block delay. Jobs that stall are reclaimable
(permissionless expiry, per-job failure refunds, a bounded expiry sweep, and a per-requestor cap),
and providers are paid only for work actually delivered.

### Privacy

GPN includes a **shielded pool** using Sapling-style note commitments and nullifiers. **Deposit**
converts transparent GPN into a hidden note (value and owner hidden). **Spend** proves in zero
knowledge that you own an unspent note, publishing only an unlinkable nullifier, and supports
private transfers and withdrawals with value conservation. The chain keeps a bounded history of
Merkle roots so earlier notes stay spendable. Production-grade privacy depends on a **multi-party
trusted-setup ceremony** (the "CRS") where the trapdoor is destroyed concurrently by independent
contributors — secure as long as a single participant is honest. That ceremony is one of the
things this campaign funds.

### The token — stated carefully

GPN is a **fair-launch, GPU-mineable proof-of-work coin**. There was **no ICO and no presale**.
One thing is disclosed rather than hidden: a **10% governance treasury** (100M GPN — 70M
genesis-unlocked plus 30M vesting over 10 years), held by a pallet account and spendable only via
a **13-of-25 governance roster** — no single admin key. Mining halving is every ~2 years; the
effective maximum supply is ≈520M GPN including the treasury.

**This campaign does not sell GPN, and no tokens are offered as rewards.** GPN has **no live
price pre-mainnet** and **no listings**; the market will set any price after mainnet. The token's
intended long-run value derives from **real AI-marketplace demand** — not mining inflation.

We also say the quiet part out loud: **in Phase 1 the mining↔compute oracle is centralized**, and
we name it while we engineer the path off it. We'd rather disclose the centralization than bury it,
because the trust that survives a crypto-skeptic audience is trust you built by being honest.

### Why now

- The world is full of idle GPUs: a gaming or workstation card sits dark ~20 hours a day.
- AI compute is expensive and centralized: buyers pay a premium; hardware owners have no clean
  way to sell.
- Mining became a one-way bet: a coin with no job behind it is a bet, not a product.
- The two problems share one answer: **the same GPU, pointed at whatever pays more.**

GPN is already live on a **public testnet**: the PoW chain with per-node rewards, the AI
marketplace (LLM, image, video, ML, fine-tuning), payment channels, shielded spends, on-chain
provider discovery, peer discovery, and committee governance. What's left is the last mile.

### Who it's for

- **GPU owners** — gaming rigs, workstations, small farms. Turn an idle card into an earner that
  works both markets.
- **AI developers** — a peer-to-peer GPU cloud: rent cheaper than hyperscalers, or sell your own
  idle capacity; payments settle on-chain.
- **Operators** — run a full node or access point and help keep the network reachable.
- **Crypto-native users** — a fair-launch PoW coin that actually has a job behind it.
- **Privacy-minded users** — shielded transfers and withdrawals.

---

## 3. Funding goal and budget

**Goal: $75,000** (all-or-nothing). This funds the last mile from a live testnet to a mainnet
launch, plus the security work that has to happen first.

| Area | Amount | What it pays for |
|---|---|---|
| Engineering & mainnet readiness | $28,000 | Finishing the auto-switch (folding the node's own miner into the switch), client-side RTT, remote-provider end-to-end, video pipeline, mainnet genesis tooling and rehearsal |
| Security: audits & red-team remediation | $14,000 | Independent review of the marketplace escrow, eligibility, and shielded pool; remediation of findings; re-review |
| CRS ceremony (shielded-pool trusted setup) | $7,000 | Coordinating, running, and verifying the multi-party Powers-of-Tau / phase-2 ceremony |
| Seed full nodes & CI/GPU hardware | $9,000 | Always-on, publicly-reachable full nodes (the network's reachability layer) plus CI GPUs for testing across vendors |
| Docs, ops & community support | $5,000 | Installer/onboarding docs, the explorer, monitoring, and backer support during launch |
| Rewards production & fulfillment | $6,000 | Swag production, shipping, and compute-credit operations |
| Platform + payment fees (~8%) | $6,000 | Kickstarter's fee and payment processing |
| **Total** | **$75,000** | |

**Stretch goals**
- **$120,000 — Reach & routing.** Client-side RTT (user → nearest node), model-aware scheduling
  hardening, and a second geographic access-point region for lower latency worldwide.
- **$200,000 — More platforms & a docs home.** Windows and macOS one-click installers, a hosted
  documentation portal, and bridge groundwork (the cross-chain bridge ships post-mainnet).
- **$350,000 — Grow the operator base.** A funded operator-onboarding program (setup bounties for
  independent full nodes), an expanded audit budget, and accelerated mainnet milestones.

> If we exceed the goal, the extra goes first to audits and the ceremony — security before speed.

---

## 4. Reward tiers

**All rewards are non-financial.** No tokens, no equity, no revenue share, and no promise of
financial return is offered at any tier. "Compute credits" here are **testnet AI-compute credits**
— a usage allowance for the GPN marketplace with no cash value, not transferable, and not a token.
Reward estimates are best-effort and depend on network/software milestones; physical rewards ship
after the campaign, and we'll keep backers posted on timing. *(Limits and delivery windows are
placeholders for Jeremy to confirm — see `README.md`.)*

| # | Price | Title | Limit | What's included | Est. delivery |
|---|---|---|---|---|---|
| 1 | **$5** | **Supporter** | Unlimited | Your name on the project **supporter wall**; backer-only update stream | 30 days after campaign |
| 2 | **$25** | **Early Access** | 1,000 | Early **testnet installer** access + a guided setup walkthrough; supporter wall | 30 days |
| 3 | **$60** | **Tinkerer** | 500 | Early access + **25 testnet AI-compute credits** + **sticker pack** | Digital ~30–45 days · stickers 2–3 months |
| 4 | **$150** | **Operator Starter Kit** | 300 | Early access + **100 compute credits** + **sticker pack + tee** + the **operator onboarding guide** + a private **Discord role** | 60 days |
| 5 | **$400** | **Founding Operator** | 150 | Early access + **400 compute credits** + full swag + named **"Founding Operator"** Discord role + **early mainnet operator allow-list** (an onboarding priority spot, not a token) | 90 days |
| 6 | **$1,000** | **Patron** | 50 | Everything above + your name/logo on the **website supporter wall** + a **1:1 onboarding call** + your name in the **launch credits** | 90 days |
| 7 | **$2,500** | **Founding Circle** | 20 | Everything above + a **seat on a design-review call** (observer; no governance rights) + a **custom founder's pack** + your name inscribed on the **project page** | 120 days |

**Notes on the tiers**
- **Compute credits** are applied to your account on the live **testnet** within the delivery
  window, and honored on mainnet when it launches (we'll announce the redemption flow in an
  update). They carry no cash value and are non-transferable.
- The **early-access installer** points at the testnet; you can run it on a supported GPU today
  and start earning testnet rewards while we ship mainnet.
- **"Founding Operator"** and **"Founding Circle"** are recognition, not ownership — no equity, no
  governance, no financial stake.
- Happy to add a **"Team"** or **"Advisor"** tier only if Jeremy wants one; nothing above is a
  token or financial instrument.

---

## 5. Risks and challenges

We lead with the catch. Here's what can go wrong and what we're doing about it.

- **It's software, and the network is young.** GPN is live on a **public testnet**, not mainnet.
  Some features (the node-internal miner folded into the auto-switch, video serving on all
  platforms, remote-provider end-to-end) are still being finished.
  **Mitigation:** the core chain, marketplace, channels, shielded spends, and discovery already
  run on testnet; we ship in small, reviewed, reversible changes and post milestone updates.

- **Testnet → mainnet is a real gate.** Mainnet requires a genesis cut and a launch decision; we
  will not rush it.
  **Mitigation:** mainnet genesis is rehearsed and only cut on an explicit go; the genesis is
  immutable and treated as final.

- **Regulatory and classification risk.** Crypto projects face shifting rules, and a token can be
  scrutinized.
  **Mitigation:** GPN is a **fair-launch mining coin** — **no ICO, no presale, no token sale
  here** — and this campaign offers **no tokens, equity, or financial return**. We disclose the
  10% governance treasury rather than hide it. We'll take legal review before any market-facing
  step. *(Not legal advice; we'll engage counsel.)*

- **Hardware and GPU availability.** Rewards that involve hardware, and the seed full nodes we
  need to run, depend on GPU/machine availability and pricing.
  **Mitigation:** we don't ship GPUs as rewards; the hardware line funds a small number of
  always-on full nodes from ordinary machines, and swag uses commodity suppliers with buffers.

- **Timeline risk.** Software estimates slip.
  **Mitigation:** conservative delivery windows with buffer, weekly-to-monthly updates, and a rule
  that extra funds go to security first.

- **Kickstarter policy risk.** Kickstarter restricts crypto/blockchain projects, and it may decline
  this even framed as software.
  **Mitigation:** we present the product as a **software + AI-compute platform**, keep every reward
  non-financial, and — if declined — move to the fallback platforms in `README.md`.

- **Security of escrow and the shielded pool.** Bad escrow logic can enable theft-of-service; a
  shielded pool is only as strong as its trusted setup.
  **Mitigation:** the marketplace's authorization/escrow rules are designed so no party can
  prematurely finalize a job, failures refund trust-minimally, and we fund independent audits plus
  the **public CRS ceremony** (secure if a single participant is honest).

- **Eligibility spoofing.** Capability is self-reported.
  **Mitigation:** layered with proof-of-work, hardware attestation, and a random capacity challenge
  so eligibility reflects *demonstrated* compute.

- **Demand risk (an empty marketplace).** Renting out compute only pays if buyers show up.
  **Mitigation:** seed demand and early-compute discounts at launch; the network is honest that
  compute income is **demand-dependent and variable** — we never promise a number.

- **Income is variable, not guaranteed.** Mining income floats with price; compute income depends
  on utilization. We say **"could earn," never "will earn."**

---

## 6. FAQ

**1. Is this an investment?**
No. Backing this campaign is a purchase of non-financial rewards (early access, credits, swag,
recognition). It is not equity, not a security, and carries **no financial return**. Nothing here
is financial advice.

**2. Can I buy GPN here?**
No. **No tokens are sold or offered as rewards.** GPN is a fair-launch, GPU-mineable coin: you
obtain it by mining, not by buying it here. It has **no live price pre-mainnet** and no listings;
the market will set any price after mainnet.

**3. Do I need a GPU to use GPN?**
To **earn**, yes — eligibility is GPU-only (no CPU/iGPU). To **buy AI compute** from the network,
no: requestors just need to reach an access point.

**4. Which GPUs work?**
Discrete GPUs that meet a measured-TFLOPS floor, with capability gates on VRAM/RAM for bigger
jobs (a video job only routes to a big-VRAM card). We publish a supported-GPU list and a benchmark
guide with the installer. A general-purpose CPU cannot competitively mine by construction.

**5. When is mainnet?**
Mainnet is the goal of this campaign, and we ship it once the audits, the ceremony, and the
final integration are done — not on a marketing date. The network is **live on a public testnet
today**; you can run it now.

**6. How do I earn?**
Run a node. Your GPU **mines GPN** when mining pays more, and **serves AI jobs** when compute pays
more — the auto-switch decides continuously, with a ~5% margin so it doesn't thrash. Miners keep
the reward of blocks they produce; providers are paid per token for work actually delivered.

**7. How much can I earn?**
We don't promise a number — it depends on your GPU, price, and demand. Verified reference: an
RTX 4090 rents for **$0.14–0.34/hr** serving AI. We publish an earnings calculator and honest
benchmarks rather than projections.

**8. Is my data private?**
The AI compute itself runs off-chain, and GPN includes a **shielded pool** for private transfers
and withdrawals (zero-knowledge spends). Production privacy depends on the **public CRS ceremony**
this campaign funds; until then, testnet proofs rely on on-chain nullifier/root/value checks.

**9. What makes GPN different from NiceHash, Vast, RunPod, Render, Akash, or io.net?**
Those let a GPU **do one thing** — mine, or rent. None **auto-switches a single card between
mining and AI serving** based on which pays more. That switch is the whole product.

**10. Is it decentralized?**
The chain is permissionless and has no validator set; the network finds its own reachable nodes.
But we're honest: **Phase 1 uses a centralized oracle** for the mining↔compute switch. We name it
and we're engineering the path off it. "Decentralizing," not "decentralized."

**11. Do I have to stake?**
No. Staking is **optional** — it raises your tier and reward multiplier, but a miner becomes
pay-eligible by simply producing seal-verified blocks.

**12. What's the governance treasury?**
A **disclosed 10% of supply** (100M GPN — 70M genesis-unlocked + 30M vesting over 10 years) held by
a pallet account, spendable only via a **13-of-25 roster**. There was no ICO. It's disclosed, not
hidden.

**13. What if the campaign doesn't reach the goal?**
Kickstarter is all-or-nothing: if we don't hit $75,000, backers are **not charged** and the
campaign ends. We'd then pursue the fallback funding routes in `README.md` and keep building in
public.

**14. Do I need to complete KYC / will this affect my taxes?**
Backing is a normal fiat purchase on Kickstarter, which requires **creator identity verification**
and pays out to a verified bank account (US: W-9 / 1099-K for the creator). Rewards are not income
to you, but **digital assets and mining rewards can have tax implications** — talk to a qualified
professional. We don't give tax advice.

**15. Can I get a refund?**
Kickstarter's all-or-nothing model means no charge unless we fund; after that, digital and
physical rewards are generally non-refundable, and compute credits carry no cash value. We'll
handle genuine fulfillment failures case-by-case.

---

## 7. Updates plan

**Cadence:** **weekly** during the campaign; **monthly** (plus milestone posts) after it funds.

**What every update covers:** what shipped, what's next, what slipped and why, and one honest
metric. No hype, no price talk.

### Update 1 — "We're live: what we're building and where it stands" (pre-launch / day 1)

> Hi everyone — thank you for backing GPN. Here's the honest starting point.
>
> **What exists today (testnet):** the proof-of-work chain with Bitcoin-style per-node rewards;
> GPU-only, stake-free, work-verified eligibility; the AI marketplace (LLM, image, video, ML,
> fine-tuning) with per-token escrow, cost-derived pricing, and least-latency routing; on-chain
> provider discovery; payment channels; shielded spends; peer discovery; and committee governance.
>
> **What this campaign funds:** finishing the auto-switch, independent audits, the shielded-pool
> **CRS ceremony**, and the seed full nodes that keep the network reachable — then the mainnet
> genesis and launch.
>
> **What we won't do:** promise a price, a listing, or an income number. GPN has no live price
> pre-mainnet, and mining and compute income are variable. We'll report progress, not projections.
>
> First technical update lands next week: the auto-switch integration status.

### Update 2 — "Auto-switch: folding the miner into the switch" (week 2)

> This week: we wired the node's own miner into the auto-switch so a single card cleanly stops
> mining and frees VRAM before a compute job loads — the "one GPU, one mode at a time" promise in
> practice, not just on paper.
>
> Also this week: a remote-provider end-to-end test — a job created at an access point, routed to
> a separate provider machine, completed, and paid. We'll post the outcome (pass or fail) and the
> fix list if it fails.
>
> Reminder of the catch we lead with: Phase 1 uses a **centralized oracle** for the switch. We're
> naming it while we build the path off it.

### Update 3 — "Security first: audits and the ceremony" (week 3)

> Where the extra money goes first: **audits and the ceremony**, before speed.
>
> - We've scoped an independent review of the marketplace escrow, eligibility gates, and the
>   shielded pool.
> - We're planning the **CRS ceremony** — the multi-party trusted setup that makes the shielded
>   pool's privacy production-grade. It's secure as long as one participant is honest, so we'll
>   invite independent contributors and publish the transcript.
>
> If we pass the stretch goals, this budget grows before anything else does. More next week.
