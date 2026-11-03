# GPN — One-Pager

*Verified against: chain spec 45 / release 0.3.11 — checked 2026-11-02 at block 236,986.*

*Last updated: 2026-11-02*

**What it is.** GPN is a **GPU-native network** where every node is **miner _and_ AI provider at the same time** — it mines between jobs and serves one when it is assigned. One install, no manual re-listing.

**The problem.** A GPU owner has two ways to earn — mine a coin, or rent the card for AI work — and must **pick one**. Mine and the coin dips → you lose. Rent and nobody books you → $0. The card can't move to where the money is.

**The fix.** GPN runs both markets on the same card: the GPU mines by default and serves AI whenever a job is assigned, with no profitability switch to configure. The chain does coordination + escrow + settlement; the heavy compute runs off-chain.

---

**How it works**

- **Mining** — GPU proof-of-work; the node that produces a block **keeps that block's reward** (Bitcoin-style).
- **Serving AI** — LLM inference, image generation, fine-tuning — priced **per token** against a throughput anchor, escrowed trust-minimally.
- **No switch to manage** — mining is the default and **stops completely** while an AI job runs, then resumes. A USD/hr profitability auto-switch is a **deferred roadmap item**, not a shipped behaviour.
- **No stake to start** — there is **no staking or delegation prerequisite** to attest, list, or serve. Staking is optional and only raises a reward multiplier.
- **Routing** — buyers hit one public endpoint (`https://gpncoin.ai/api/v1/*`, OpenAI-compatible, rate-limited); the network picks the **lowest-latency idle** attested GPU. Providers are discovered **on-chain**, and jobs are **pulled** by the provider, so a NAT'd box still serves.
- **Models** — a **curated, VRAM-gated** catalogue: `llama3.2:1b`, `qwen2.5:0.5b`, `qwen2.5:7b` on the reference 8 GB node (`GET /v1/models` is authoritative). A model that cannot fit the card's VRAM is never advertised at all; fine-tune artifacts (`tuned-*`) and sub-0.3 B toys stay out of the **public catalogue** but are still served if a caller names them — `/status models[]` is each node's real inventory, not the catalogue.
- **Runs on a gaming PC** — a local dashboard (`127.0.0.1:8099`) with an **Idle** switch: hand the GPU back for up to 6 h while the node keeps syncing, peering and serving the chain, then resumes on its own.
- **Stays current by itself** — signed (ed25519) bundles from `https://gpncoin.ai/releases.json`, **on by default**, installed with a per-node randomized schedule so the fleet never stampedes a release.

**Why it's different**

- **Both markets, one machine** — no incumbent (NiceHash, Vast, RunPod, Render, Akash, io.net) lets a GPU mine *and* serve at once.
- **Trust-minimal** — on-chain escrow, optional **payment channels** (no per-job block delay), opaque/private on-chain values.
- **GPU-only, provably** — proof-of-work + hardware attestation + a compute challenge; a CPU/iGPU can't earn. Attestation proves *which GPU is online*, not that a result is correct — no output verification exists today (roadmap).

**Token (GPN)**

- **Fair launch via mining** — no ICO, no hidden allocation. 12 decimals.
- **Halving every ~2 years** (4,204,800 blocks); block reward 50 GPN @ 15 s.
- **Hard supply cap ≈ 520 M GPN** — 420 M emitted over the halving schedule plus 100 M allocated to the treasury.
- **Disclosed, vested treasury — at mainnet launch** — 100 M GPN held by a keyless pallet account; 70 M unlocked at genesis, 30 M vesting over 10 years; spendable only by a **13-of-25 treasury spend roster**. The testnet genesis mints **0** into the treasury, so none of this exists on the testnet today.
- **Token value derives from real AI-compute demand** — not just mining inflation.

**Privacy.** A shielded pool supports private transfers and withdrawals (zero-knowledge spends; nullifier-based double-spend protection).

**Where it is (2026-11-02, testnet — labelled honestly)**

- **LIVE** — PoW chain with Bitcoin-style per-node rewards (spec 45, release 0.3.11, 15 s); **text/LLM inference** over the public HTTPS API; **image generation** (`POST /v1/images/generations`); **fine-tuning** (`POST /v1/finetunes`); per-token escrow + payment channels; on-chain provider discovery; shielded spends; peer discovery; node GUI + Idle; signed auto-update; **free** testnet inference.
- **BLOCKED** — video (LTX) is implemented but has never completed: the T5-XXL encoder exceeds 8 GB of VRAM, and there is no public `/v1/videos` route.
- **INTERNAL** — ML inference: an `MLInference` job type and an ML provider binary exist, but there is no ML-only public route and no completed job to point at.
- **GOVERNANCE — design:** a **5-member council with 3-of-5 approvals**; treasury spending is a separate **13-of-25 roster**. **What testnet actually runs (verified 2026-11-02, block 236144):** the same 5 members, 3 approvals, and a **10-block (~2 min) timelock** (the mainnet design is 3 days). There is **no cancel/veto extrinsic** and no referendum track — an approved proposal cannot be recalled. Testnet governance is a **bootstrap under operator control, not a security guarantee**, and imposes **no member-count requirement**.
- **GAP** — there is no result verification: output is settled as submitted, never checked. (The on-chain GPU-hour oracle feed is **live** since 2026-10-22 — a bonded operator posts token $0.15 / GPU-hour $0.30 / util 65% / APR 0%.)
- **Next** — result verification, the public CRS ceremony, mainnet (3-day governance timelock), and a cross-chain bridge.

**On price (no overclaim).** GPN prices tokens off **throughput**: an anchor constant (a 4060-class card earns ≈$0.10/hr; the live idle-hardware feed reports ≈$0.30/GPU-hr) divided by the token rate that card reaches on a 1B model, plus a 30% margin. That is **$0.379/1M output** and **$0.057/1M input** (input metered at 15% of output) regardless of which GPU serves the job — a faster card earns the provider more per hour ($0.371 on a 4090, $1.232 on an H100) rather than costing the buyer more. That sits next to `gpt-5-nano`'s $0.40, not a multiple of it.

**One line:** *A GPU should not have to choose.*

`gpn.network` · whitepaper + docs in the project corpus
