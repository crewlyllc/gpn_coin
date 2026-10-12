# GPN — Media & Asset Checklist

Everything the campaign page, video, and social launch need. Specs are given as
**recommended** — always confirm against Kickstarter's current asset guide and each social
platform's latest spec before final export. Keep the visual voice aligned to
`marketing/brand-visual-identity-guide.md` and the copy aligned to the brand voice guide.

Legend: **✅ have / can capture now** · **🎨 needs design** · **📸 needs capture** · **⬜ to confirm**

---

## 1. Asset checklist

### Campaign page

| Asset | Spec | Used where | Status |
|---|---|---|---|
| **Hero / main project image** | 16:9, **1024×576 min** (export 1600×900); JPG/PNG, <5MB | KS project card + top of page | 🎨 |
| **Project thumbnail** | 16:9, 1024×576; keep logo legible small | KS browse/thumbnail crops | 🎨 |
| **Project logo (transparent)** | **SVG** + PNG @ 512/256/128; on light & dark variants | Page header, end cards, watermark | ⬜ |
| **Video thumbnail** | 16:9, 1280×720+ | Video player poster frame | 📸 |
| **Reward-tier images (7)** | 16:9 or 1:1, ≥640×360 | Each reward card | 🎨 |
| **Section images / dividers** | 16:9, ≥1600×900 | Between story sections | 🎨 |

### Screenshots & GIFs (from the live testnet)

| Asset | Spec | Used where | Status |
|---|---|---|---|
| **Dashboard view** (mode + USD/hr comparison) | 1920×1080 PNG; crop 16:9 | Story "how the switch works" + video shot 6 | 📸 |
| **Live AI job** (job created → routed → tokens streaming) | 1920×1080 PNG + **GIF/MP4** (GIF <5MB, 800×600, 8–12s loop) | Story "marketplace" + video shot 7 | 📸 |
| **The switch in action** (MINING → SERVING AI, miner stops, model loads) | **MP4** 1280×720, 10–15s | Hero of the video demo beat | 📸 |
| **Explorer / block feed** | 1920×1080 PNG | "It's real, it's live on testnet" | 📸 |
| **Provider discovery / on-chain registry** | 1920×1080 PNG | Story "routing" | 📸 |
| **Shielded spend** (note in / nullifier out) | 1920×1080 PNG | Privacy section | 📸 |
| **Earnings calculator** | 1920×1080 PNG | FAQ "how much can I earn" | ✅ (exists as `gpu-earnings-calculator.html`) |

> **Blur/replace rule:** any screenshot that shows a *token price* must be blurred or replaced with
> a neutral mock — we never show a price pre-mainnet.

### Diagrams

| Asset | Spec | Used where | Status |
|---|---|---|---|
| **Mine ⇄ Serve auto-switch diagram** | **SVG** + PNG 1600×900; one slide explains the whole product | Story hero section + video | 🎨 |
| **Job lifecycle diagram** (Pending → … → Completed) | SVG + PNG 1600×900 | Story "marketplace" | 🎨 |
| **Routing diagram** (buyer → access point → nearest verified GPU) | SVG + PNG 1600×900 | Story "routing" | 🎨 |
| **Escrow / payment-channel diagram** (lock once on-chain, settle off-chain) | SVG + PNG 1600×900 | Story "trust-minimal escrow" | 🎨 |
| **Eligibility diagram** (PoW + attestation + capacity challenge; CPU greyed out) | SVG + PNG 1600×900 | Story "GPU-only" | 🎨 |

### Social & launch

| Asset | Spec | Used where | Status |
|---|---|---|---|
| **Open Graph / X card** | 1200×630 | Link previews (X, Discord, LinkedIn, FB) | 🎨 |
| **Square social card** | 1080×1080 | Instagram, X, LinkedIn | 🎨 |
| **Vertical / Stories card** | 1080×1920 | IG/FB Stories, TikTok, Shorts thumbnail | 🎨 |
| **Banner (X/YT)** | X 1500×500 · YT 2560×1440 | Profile headers | 🎨 |
| **Launch-day announcement card** | 1200×630 + 1080×1080 | "We're live" post | 🎨 |
| **Stretch-goal unlocked cards (3)** | 1200×630 | Milestone posts | 🎨 |
| **Backer thank-you card** | 1200×630 | Post-funding update | 🎨 |
| **Discord/Telegram server icon** | 512×512 | Community | 🎨 |

### Physical swag artwork (for reward fulfillment)

| Asset | Spec | Used where | Status |
|---|---|---|---|
| **Sticker sheet** | die-cut, 100×100mm, CMYK, 300dpi, cut lines | Tinkerer / Starter Kit tiers | 🎨 |
| **T-shirt print** | front + sleeve, 300dpi, CMYK, separated | Starter Kit+ tiers | 🎨 |
| **Founder's pack** (art card / patch / enamel-pin art) | print-ready, 300dpi | Founding Circle | 🎨 |

---

## 2. Design principles (quick guardrails)

- **Product, not price.** Show the *switch*, the dashboard, a real job — not charts promising money.
- **One idea per image.** If a graphic needs a paragraph, cut it.
- **Plain-spoken labels.** "Mining / Serving AI," "USD/hr comparison," "no block delay" — the same
  words the story uses.
- **Honest by design.** The Phase-1 centered-oracle note appears on at least the switch diagram's
  caption or an adjacent card; never imply Phase-1 is decentralized.
- **Legible at thumbnail size.** The `Mine ⇄ Serve` mark should read at 200px wide.
- **Accessible contrast.** Meet WCAG AA for on-image text; caption text ≥ 4.5:1.

---

## 3. Social / launch post copy

Aligned to brand voice: confident, honest, no hype, no price/return claims. Adjust per platform.

### X / Twitter — launch thread (short posts, one idea each)

1. **We're live on Kickstarter.** GPN: software that turns one GPU into two income paths — it
   mines, or it serves AI compute, and auto-switches to whichever pays more. *A GPU should not
   have to choose.* 🧵
2. **The problem.** Own a GPU and you must pick one: mine a coin, or rent it for AI work. Mine and
   a dip means mining at a loss. Rent and an empty queue means $0. The card can't move to where
   the money is.
3. **The switch.** Every cycle, the node compares mining vs AI-serving revenue in USD/hr and flips
   when one beats the other by >5% — no thrashing. One GPU, one install, no re-listing.
4. **AI buyers, one endpoint.** Submit a job; the network routes it to the nearest verified GPU.
   Escrowed on-chain; payment channels mean no per-job block delay.
5. **GPU-only, verifiably.** Proof-of-work + hardware attestation + a capacity challenge. A CPU
   can't fake it.
6. **Honest catch.** Our Phase-1 switch oracle is centralized. We say so out loud and we're
   engineering the path off it. Trust is the moat.
7. **It's live.** The chain, marketplace, channels, and shielded pool run on a public testnet
   today. This campaign funds the last mile: auto-switch, audits, the shielded-pool ceremony, seed
   nodes — then mainnet.
8. **What we're *not* doing:** no tokens for sale, no ICO, no price promises. Just a real tool for
   the hardware you already own. Back us → [link]

### LinkedIn / Facebook — single honest post

> Most graphics cards sit idle ~20 hours a day. And most owners face a bad choice: mine a coin, or
> rent the card for AI work — but you have to pick one, and the card can't move when the other
> market pays more.
>
> We built GPN so a single GPU does both, auto-switching to whichever pays more, coordinated and
> settled by a trust-minimal chain. It's open source and live on a public testnet today.
>
> We're funding the last mile to mainnet on Kickstarter: finishing the auto-switch, independent
> security audits, the shielded-pool ceremony, and the seed nodes that keep the network reachable.
> Non-financial rewards only — early access, compute credits, swag. No tokens, no price promises.
>
> A GPU should not have to choose. [link]

### Instagram / short-form caption

> Your GPU has two ways to earn. Until now, you had to pick one. GPN doesn't make it choose — it
> mines, or it serves AI, and switches to whichever pays more. Live on testnet. Back the last mile
> on Kickstarter. 🔗 in bio. **#GPU #AICompute #OpenSource** *(no price/return claims)*

### Community (Reddit / Discord / Telegram) — contributor note

> Hi all — we're the GPN team. Disclosure up front: this is our project.
>
> GPN is open-source software that runs one GPU in two markets — it mines GPN or serves AI compute
> and auto-switches to whichever pays more. The chain does coordination/escrow; compute runs
> off-chain. It's live on a public testnet. We've launched a Kickstarter to fund the last mile
> (auto-switch, audits, ceremony, seed nodes) with **non-financial rewards only — no tokens.**
>
> Honest caveats: Phase-1 oracle is centralized (we're moving off it); compute income is
> demand-dependent and variable; no price/listing promises. Happy to answer anything. [link]

> **Per-platform rule:** read each community's own self-promo rules before posting; disclose
> affiliation (brand guide rule 5).

### Short post templates

- **Milestone:** "Milestone: [X] is live on testnet. Here's what it does and what's still to
  come: [link]."
- **Stretch goal:** "Stretch goal unlocked: [X]. Extra funds go to [audits/ceremony] first — always
  security before speed."
- **Final hours:** "Final hours. If we hit $75k, the last mile ships: auto-switch, audits, the
  ceremony, seed nodes. Non-financial rewards only. [link]"

---

## 4. Where each asset is used (quick map)

| Asset | Campaign page | Video | Social |
|---|---|---|---|
| Hero image | ✅ top | end card | OG card |
| Logo pack | ✅ header | watermark | avatars |
| Mine⇄Serve diagram | ✅ story | ✅ demo | ✅ thread image |
| Dashboard screenshot | ✅ "how it works" | ✅ shot 6/7 | ✅ launch post |
| Live AI job GIF | ✅ marketplace | ✅ shot 7 | ✅ thread |
| Explorer / testnet shots | ✅ "it's live" | ✅ shot 11 | — |
| Social cards | — | — | ✅ all posts |
| Swag artwork | ✅ tiers | — | ✅ teases |
