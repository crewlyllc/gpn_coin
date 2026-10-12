# GPN — Kickstarter Campaign Package

A complete, ready-to-adapt crowdfunding package for **GPN (Project 7)**. It presents GPN as a
**software + AI-compute platform** — "turn your GPU into income by mining **or** serving AI" —
so it can sit on a platform whose rules restrict blockchain/crypto projects and forbid
token/equity/financial-return rewards.

**Status:** DRAFT. Nothing posts until Jeremy clears accounts, timing, and the tokenomics wording.

---

## What's in here

| File | What it is |
|---|---|
| **`README.md`** (this file) | How to use the package, the platform-policy note, and fallback platforms |
| **`campaign.md`** | The full campaign: title/tagline/blurb, the long-form story, funding goal + budget + stretch goals, reward tiers, risks, FAQ, and the updates plan |
| **`video-script.md`** | A 75s campaign video script (hook/problem/demo/solution/ask), shot list, B-roll ideas, and 30s/15s/6s cut-downs |
| **`media-assets.md`** | The asset checklist (hero, logo, screenshots/GIFs, diagrams, social cards, swag art) with size/aspect specs and usage, plus social/launch post copy |
| **`launch-checklist.md`** | Pre-launch, launch-day, and post-launch tasks (profile, bank/tax, KS review, embargo/press, day-1 social, backer comms) |

**Content sources** (read these first; the package is derived from them):
`GPN-WHITEPAPER.md`, `GPN-ONE-PAGER.md`, `GPN-TECHNICAL-OVERVIEW.md`, `GPN-MASTER-REFERENCE.md`,
`marketing/mission-and-origin-narrative.md`, and `marketing/brand-voice-and-messaging-guide.md`.

---

## How to use it

1. **Read the policy note first** (below) and confirm Kickstarter's current rules before submitting.
2. **Review `campaign.md`** — pick the title, confirm the $75,000 goal and budget, and lock the
   reward tiers (prices, limits, delivery estimates are placeholders for Jeremy).
3. **Produce the assets** from `media-assets.md` (several screenshots can be captured from the live
   testnet today).
4. **Shoot the video** from `video-script.md` (the honesty beat is not optional).
5. **Run `launch-checklist.md`** top to bottom.

### Who owns what
- **JEREMY:** policy/eligibility decisions, the Kickstarter account + bank/tax, the funding goal,
  final reward pricing, legal review, and sign-off on the tokenomics wording.
- **PM07 (engineering/ops):** testnet screenshots, credit provisioning, fulfillment of technical
  rewards, milestone updates.
- **MKT (marketing):** copy, assets, video, social, press, community, and the honesty QA pass.

### Voice & honesty rules (from the brand guide — non-negotiable)
- Plain-spoken, honest-to-a-fault, concrete. No hype, no urgency, no financial promises.
- **"Could / can / estimate"** — never "will / guaranteed."
- **Disclose the Phase-1 centralized oracle** in anything touching decentralization or the switch.
- **Never claim a pre-mainnet price, listing, or settlement.** GPN has no live price pre-mainnet.
- **No invented numbers.** Verified figures only (1B cap, ~2yr halving, 50 GPN @ 15s, ≈520M
  effective supply, RTX 4090 AI rental $0.14–0.34/hr). Everything else is marked "est."
- The canonical tokenomics line: *"Fair-launch GPU-PoW, no ICO, disclosed 10% governance treasury,
  no single admin key."*

---

## Platform-policy note

> **Kickstarter restricts blockchain/crypto projects and prohibits offering tokens, equity, or
> any financial return as a reward.** Confirm the current policy on Kickstarter's own rules page
> before submitting — the policy can change.

This package handles that by:

1. **Framing GPN as software + an AI-compute marketplace**, not "a coin." The product is "turn your
   GPU into income by mining **or** serving AI, auto-switched." The token exists, but it is a
   **fair-launch mining coin**, not something sold here.
2. **Offering only non-financial rewards:** early access, testnet AI-compute credits, swag,
   recognition (a supporter wall, named roles), and onboarding priority. No tokens, no equity, no
   revenue share, no financial return.
3. **Stating the KYC/tax reality:** Kickstarter requires creator identity verification and pays to a
   verified bank account (US: W-9 / 1099-K; campaign income is taxable). These are the *creator's*
   obligations, not the backer's — but backing is a normal fiat purchase, and mining/credits can
   have tax implications for users (not tax advice).
4. **Saying, on the page, that no tokens are offered and no price/listing/return is promised.**

**Even framed this way, Kickstarter may decline it.** If so, move to the fallbacks below.

---

## Fallback platforms & enablers (if Kickstarter rejects it)

Ordered from "closest fit" to "different framing":

1. **Self-hosted / indie fundraising (no crypto restriction).**
   Run your own campaign page with a community-favorite processor: e.g. **Open Collective** or
   **GitHub Sponsors** for the open-source work, plus a simple **pre-order/credit** store and a
   public "founders wall." Pros: no platform crypto ban, full control of copy and rewards. Cons:
   you own trust and payment friction. *(Best for: the OSS + operator community.)*

2. **Public-goods / OSS grant ecosystems (web3-native, rewards impact not tokens).**
   Apply for grants rather than crowdfunding rewards: **Gitcoin Grants**, **Optimism Retro
   Funding (RetroPGF)**, the **Ethereum Foundation Ecosystem Support Program (ESP)**, and
   **Filecoin/IPFS-style** compute/privacy grants. Frame the **privacy (shielded pool)** and
   **decentralized AI compute** angles. Pros: mission-aligned money, no consumer crowdfunding risk.
   Cons: competitive, milestone-gated, slower. *(Best for: audits, the CRS ceremony, public-goods
   tooling.)*

3. **Hardware / AI-device framing (turns the pitch into a tangible product).**
   Package a **"GPN Operator Appliance"** — a pre-configured node (mini-PC + GPU, or a
   bring-your-own-GPU kit with a flashed starter USB) — and take it to a **hardware-friendly
   crowdfunding platform** (e.g. **Crowd Supply** or **GroupGets**). The reward is a physical
   device, not a token. Pros: sidesteps the software/crypto restriction; tangible backer value.
   Cons: BOM, logistics, and margin risk; only offer if you can actually source and ship it.
   *(Best for: the operator/enthusiast audience who want a turnkey box.)*

4. **Indiegogo (rules can differ from Kickstarter).**
   Historically more permissive on crypto-adjacent campaigns, but **policies vary and change** —
   verify before assuming. Keep the same non-financial reward model. *(Best as: a parallel listing
   if KS declines and the hardware framing is too heavy.)*

> **Whichever route:** keep the same honest framing and non-financial rewards, and confirm each
> platform's current policy in writing before you commit spend.

---

## Placeholders left for Jeremy (nothing is invented here)

- **Founder story / bio** — explicitly Jeremy's to write (see `mission-and-origin-narrative.md` §5).
- **Reward tier limits** (1,000 / 500 / 300 / 150 / 50 / 20) — proposed defaults; confirm.
- **Delivery estimates** on each tier — proposed; confirm against real fulfillment timelines.
- **Funding goal ($75,000) and budget lines** — proposed; confirm the split.
- **Campaign duration / dates** — not set here.
- **Whether to add a "Team"/"Advisor" tier** — left open (never a token/financial tier).
- **Canonical tokenomics line (C8)** — appears verbatim; pending Jeremy's sign-off per the brand guide.
- **Screenshots/GIFs** — to be captured from the live testnet; any visible token price must be
  blurred or replaced.

---

## Hard rules respected

- No genesis/canonical-WASM changes were made; no `set_code`.
- No secrets, keys, PATs, or credentials are included anywhere in this package.
- No token, equity, or financial-return reward is offered at any tier.
