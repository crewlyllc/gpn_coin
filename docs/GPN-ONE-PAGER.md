# GPN — One-Pager

*Last updated: 2026-10-22*

**What it is.** GPN is a **GPU-native network** where every GPU **mines _or_ serves AI compute — auto-switching** to whichever pays more. Same card, one install, no manual re-listing.

**The problem.** A GPU owner has two ways to earn — mine a coin, or rent the card for AI work — and must **pick one**. Mine and the coin dips → you lose. Rent and nobody books you → $0. The card can't move to where the money is.

**The fix.** GPN watches both markets continuously and **moves the GPU automatically**. The chain does coordination + escrow + settlement; the heavy compute runs off-chain.

---

**How it works**

- **Mining** — GPU proof-of-work; the node that produces a block **keeps that block's reward** (Bitcoin-style).
- **Serving AI** — LLM inference, image/video generation, ML inference, fine-tuning — priced **per token** against GPU-rental cost, escrowed trust-minimally.
- **The switch** — designed to compare mining vs compute revenue in **USD/hr** and flip on a >5% margin. **Live today:** mining is the default and **stops completely** while an AI job runs, then resumes — the oracle-driven economic switch is **not yet wired**.
- **Routing** — buyers hit one public endpoint (`https://gpncoin.ai/api/v1/*`, OpenAI-compatible, rate-limited); the network picks the **lowest-latency idle** verified GPU. Providers are discovered **on-chain**, and jobs are **pulled** by the provider, so a NAT'd box still serves.
- **Models** — a **curated, VRAM-gated** catalogue: assistant-grade models only (fine-tune artifacts, sub-0.3B toys and anything that cannot fit the card's VRAM are never advertised).
- **Runs on a gaming PC** — a local dashboard (`127.0.0.1:8099`) with an **Idle** switch: hand the GPU back for up to 6 h while the node keeps syncing, peering and serving the chain, then resumes on its own.
- **Stays current by itself** — signed (ed25519) bundles from `https://gpncoin.ai/releases.json`, **on by default**, installed with a per-node randomized schedule so the fleet never stampedes a release.

**Why it's different**

- **Both markets, one machine** — no incumbent (NiceHash, Vast, RunPod, Render, Akash, io.net) lets a GPU mine *and* serve with an automatic switch.
- **Trust-minimal** — on-chain escrow, optional **payment channels** (no per-job block delay), opaque/private on-chain values.
- **GPU-only, verifiably** — proof-of-work + hardware attestation + a compute challenge; a CPU/iGPU can't earn.

**Token (GPN)**

- **Fair launch via mining** — no ICO, no hidden allocation. 12 decimals.
- **Halving every ~2 years** (4,204,800 blocks); block reward 50 GPN @ 15 s.
- **Hard supply cap ≈ 520 M GPN** — 420 M emitted over the halving schedule plus 100 M allocated to the treasury.
- **Disclosed, vested treasury** — 100 M GPN held by a keyless pallet account; 70 M unlocked at genesis, 30 M vesting over 10 years; spendable only by a 13-of-25 governance roster.
- **Token value derives from real AI-compute demand** — not just mining inflation.

**Privacy.** A shielded pool supports private transfers and withdrawals (zero-knowledge spends; nullifier-based double-spend protection).

**Where it is (2026-10-22, testnet — labelled honestly)**

- **LIVE** — PoW chain with Bitcoin-style per-node rewards (spec 41, 15 s); **text/LLM inference** over the public HTTPS API; per-token escrow + payment channels; on-chain provider discovery; shielded spends; peer discovery; node GUI + Idle; signed auto-update; **free** testnet inference.
- **WORKS INTERNALLY** — image generation (`job_type=ImageGeneration`, sd-turbo) completes via on-chain `POST /jobs`; it is **not** wired to a public `/v1` route. Fine-tune + ML providers ship but are not routed publicly.
- **BLOCKED** — video (LTX) is implemented but has never completed: the T5-XXL encoder exceeds 8 GB of VRAM.
- **STAGED, NOT APPLIED** — governance hardening (spec 42): 5-member committee, 3-of-5 approvals, proposal expiry, `cancel()`, hash-addressed preimages, real weights. **On chain today it is still 3 members, 2-of-3, 3-day timelock, no cancel/expiry, zero weights.**
- **GAP** — the on-chain GPU-hour price oracle feed is absent (the scheduler runs on hardcoded fallbacks), and input tokens are not metered.
- **Next** — the public CRS ceremony, mainnet, and a cross-chain bridge.

**On price (no overclaim).** GPN prices tokens off **throughput**: an anchor constant (a 4060-class card earns ≈$0.10/hr) divided by the token rate that card reaches on a 1B model, plus a 30% margin. That is **$0.379/1M output** ($0.095/1M input) regardless of which GPU serves the job — a faster card earns the provider more per hour ($0.371 on a 4090, $1.232 on an H100) rather than costing the buyer more. That sits next to `gpt-5-nano`'s $0.40, not 22× above it. The old market-anchored model ($3.00 GPU-hour → $8.75/1M) remains selectable with `GPN_PRICING_MODEL=market` for comparison.

**One line:** *A GPU should not have to choose.*

`gpn.network` · whitepaper + docs in the project corpus
