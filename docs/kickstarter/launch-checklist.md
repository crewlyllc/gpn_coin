# GPN — Kickstarter Launch Checklist

Three phases: **pre-launch**, **launch day**, **post-launch**. Owners are placeholders
(**JEREMY** = needs Jeremy; **PM07** = ops/engineering; **MKT** = marketing). Nothing here
publishes until Jeremy clears it. Keep the honesty guardrails from the brand voice guide live at
every step.

---

## 0. Eligibility & policy gate (do this first)

- [ ] **Confirm Kickstarter's current policy.** KS restricts blockchain/crypto projects and
      forbids token/equity/financial-return rewards. If our software framing still fails their
      review, fall back to the platforms in `README.md`. — **JEREMY**
- [ ] **Confirm the reward model is 100% non-financial.** No tokens, no equity, no revenue share,
      no "investment." Compute credits are testnet usage credits, no cash value. — **JEREMY/MKT**
- [ ] **Reward-credit delivery check.** Confirm what "testnet credits" cost us to honor and that
      mainnet-redemption wording is safe (no fixed price/date for mainnet). — **PM07/JEREMY**
- [ ] **Legal review** of campaign copy for the tokens/marketplace/treasury language. — **JEREMY**

---

## 1. Pre-launch (T-21 → T-1 days)

### Project setup
- [ ] Create the Kickstarter account/profile; enable 2FA. — **JEREMY**
- [ ] Complete **creator identity verification** (government ID). — **JEREMY**
- [ ] Add the campaign's **bank account** and **tax details** (US: W-9; expect 1099-K reporting;
      KS payout fees apply). — **JEREMY**
- [ ] Confirm payout currency and **who is the legal creator entity**. — **JEREMY**
- [ ] Set the **funding goal ($75,000)**, currency, and **all-or-nothing** model. — **JEREMY**
- [ ] Set **campaign duration** (recommend 30–45 days) and **start/end times**. — **JEREMY/MKT**
- [ ] Add **team members** (if any) with proper roles; no fabricated bios. — **JEREMY**

### Content (from this package)
- [ ] Title, tagline, blurb finalized and within limits (`campaign.md` §1). — **MKT**
- [ ] Story sections pasted and QA'd against the brand voice guide (banned phrases, C8 wording,
      "could, not will," Phase-1 oracle disclosed). — **MKT**
- [ ] Budget breakdown and stretch goals finalized. — **JEREMY/MKT**
- [ ] Reward tiers priced, limited, and delivery estimates set (confirm placeholder limits). —
      **JEREMY/MKT**
- [ ] Risks section and FAQ reviewed for honest framing. — **MKT/JEREMY**
- [ ] **Run the pre-publish QA** (6 honesty rules; grep for banned terms; confirm verified figures:
      1B cap, ~2yr halving, 50 GPN/15s, ≈520M effective, $0.14–0.34/hr 4090). — **MKT**

### Media
- [ ] Hero image + logo pack delivered (`media-assets.md`). — **MKT/design**
- [ ] Screenshots + GIFs captured from the **live testnet** (dashboard, live AI job, the switch,
      explorer, shielded spend); blur any token price. — **PM07/MKT**
- [ ] Diagrams rendered (auto-switch, job lifecycle, routing, escrow, eligibility). — **MKT**
- [ ] Social cards + swag artwork produced. — **MKT**
- [ ] **Video** finished (75s master + 30s/15s/6s cut-downs), captioned, muted-safe. — **MKT**

### Page build
- [ ] Campaign page assembled; preview on desktop + mobile. — **MKT**
- [ ] Every claim traced to a source doc (no invented numbers). — **MKT**
- [ ] Confirm **no price/listing/return** language anywhere. — **MKT/JEREMY**

### Comms & press
- [ ] **Embargo / press** plan set; press kit + one-paragraph "why" boilerplate ready
      (`marketing/press-kit-and-release.md`, `mission-and-origin-narrative.md` §4). — **MKT**
- [ ] Build the **pre-launch email/Discord/X list**; schedule a "notify me on launch" push. — **MKT**
- [ ] Line up **creator/influencer/press** outreach (`influencer-outreach-list.md`,
      `media-targets-list.md`) with honest framing. — **MKT**
- [ ] Draft **day-1 social** posts and schedule them (`media-assets.md` §3, launch copy bank). — **MKT**

### Kickstarter review
- [ ] Submit the campaign for **KS review**; buffer for questions/rejections. — **JEREMY**
- [ ] Have the **fallback platform** package ready in case review fails. — **JEREMY/MKT**
- [ ] Dry-run the **"launch"** flow (preview link, back a test reward). — **MKT**

---

## 2. Launch day (T-0)

- [ ] **Flip the campaign live**; verify the page renders and the video plays. — **JEREMY**
- [ ] Post **day-1 announcement** (X thread, LinkedIn/FB, IG, Discord/Telegram, Reddit where
      allowed with affiliation disclosed). — **MKT**
- [ ] Send the **email/list blast** ("We're live"). — **MKT**
- [ ] Ping press/creators under the agreed embargo. — **MKT**
- [ ] Post **Update 1** on the campaign page (`campaign.md` §7). — **MKT**
- [ ] Monitor comments/backer questions; answer within SLA; keep answers honest and consistent
      with the FAQ. — **MKT**
- [ ] Watch for **KS policy flags**; be ready to adjust wording. — **JEREMY**
- [ ] Confirm **first-day traction** and the "funded %" bar reads correctly on mobile. — **MKT**
- [ ] Log metrics (visits, conversion, average pledge) for the tracking sheet. — **MKT**

---

## 3. Post-launch (T+1 → post-funding)

### During the campaign
- [ ] **Weekly updates** on cadence (`campaign.md` §7): what shipped, what's next, what slipped. —
      **MKT/PM07**
- [ ] **Milestone/stretch-goal posts** as they unlock. — **MKT**
- [ ] Reply to every substantive comment; surface tough questions rather than hiding them. — **MKT**
- [ ] Amp social with backer spotlights and testnet screenshots. — **MKT**
- [ ] **Mid-campaign push** (press round 2, community AMA) if traction dips. — **MKT**
- [ ] **Final-48h** reminder posts + email. — **MKT**

### If the goal is reached
- [ ] Thank-you post + backer update; publish the stretch-goal plan. — **MKT**
- [ ] Kickstarter releases funds after the campaign closes (minus ~8% platform+payment fees). —
      **JEREMY**
- [ ] **Charge/fulfillment plan** for swag (production + shipping) and compute credits (testnet
      provisioning). — **PM07/MKT**
- [ ] Send backer **surveys** (addresses for swag, account IDs for credits, tier choices). — **MKT**
- [ ] Track fulfillment milestones in the reward operations doc. — **MKT**
- [ ] Keep building in public; ship an update per milestone (auto-switch → audits → ceremony →
      mainnet rehearsal → mainnet). — **PM07**

### If the goal is NOT reached
- [ ] Post an honest **thank-you + next-steps** update; backers are **not charged**. — **MKT**
- [ ] Pivot to **fallback funding** (`README.md`: indie/OSS fundraising, public-goods grants,
      hardware/device framing). — **JEREMY/MKT**
- [ ] Keep the backer list warm for the next attempt. — **MKT**

### Compliance & housekeeping
- [ ] Archive the campaign page and all updates. — **MKT**
- [ ] Keep **tax records** of KS income (it is taxable) and fulfillment costs; engage an
      accountant. — **JEREMY**
- [ ] Confirm no reward was ever framed as a security/token/investment in any update. — **MKT**
- [ ] Post a **retrospective** internally (what worked, what to fix) and update this checklist. —
      **MKT**

---

## Reward-risk watchlist (keep visible)

- [ ] **Compute-credit delivery** must not imply a fixed mainnet date or a price. Credits are
      testnet usage allowances; mainnet redemption is "when it launches," announced via update.
- [ ] **"Early mainnet operator allow-list"** (Founding Operator) is an onboarding priority, not a
      token, stake, or financial interest.
- [ ] **No voting/governance perks** that could read as ownership; recognition only.
- [ ] **Swag shipping** costs and lead times are real — keep buffers in the delivery estimates.
- [ ] **Every update** re-runs the 6 honesty rules before posting.
