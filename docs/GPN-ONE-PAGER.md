# GPN — One-Pager

**What it is.** GPN is a **GPU-native network** where every node is **both a miner and an AI provider**. One install, one card: mining runs whenever the GPU is free, and an AI job pre-empts it, runs on the whole GPU, then hands it back.

**The problem.** A GPU owner has two ways to earn — mine a coin, or rent the card for AI work — and must **pick one**. Mine and the coin dips → you lose. Rent and nobody books you → $0. The card can't move to where the money is.

**The fix.** GPN removes the choice: the node mines by default and **yields the GPU to each AI job**, so the same card earns from both markets. The chain does coordination + escrow + settlement; the heavy compute runs off-chain.

---

**How it works**

- **Mining** — GPU proof-of-work; the node that produces a block **keeps that block's reward** (Bitcoin-style).
- **Serving AI** — LLM inference, image/video generation, ML inference, fine-tuning — priced **per token** against real cost, escrowed trust-minimally.
- **The switch** — per job, not per market cycle: an incoming job pauses mining, frees VRAM, runs on the whole GPU, then mining resumes. One GPU runs exactly one job at a time.
- **Routing** — buyers hit one endpoint; the network picks the **lowest-latency, least-loaded** verified GPU. Providers are discovered **on-chain**.

**Why it's different**

- **Both markets, one machine** — no incumbent (NiceHash, Vast, RunPod, Render, Akash, io.net) is both a native PoW miner and an AI marketplace on the same card.
- **Trust-minimal** — on-chain escrow, optional **payment channels** (no per-job block delay), opaque/private on-chain values.
- **GPU-only, verifiably** — proof-of-work + hardware attestation + a compute challenge; a CPU/iGPU can't earn.

**Token (GPN)**

- **Fair launch via mining** — no ICO, no hidden allocation. 12 decimals.
- **Halving every ~2 years** (4,204,800 blocks); block reward 50 GPN @ 15 s.
- **Hard supply cap ≈ 520 M GPN** — 420 M emitted over the halving schedule plus 100 M allocated to the treasury.
- **Disclosed, vested treasury** — 100 M GPN held by a keyless pallet account; 70 M unlocked at genesis, 30 M vesting over 10 years; spendable only by a 13-of-25 governance roster.
- **Token value derives from real AI-compute demand** — not just mining inflation.

**Privacy.** A shielded pool supports private transfers and withdrawals (zero-knowledge spends; nullifier-based double-spend protection).

**Where it is.** Live on a public testnet: PoW chain, per-node rewards, the full AI marketplace (LLM/image/video/ML/fine-tune), payment channels, shielded spends, on-chain provider discovery, peer discovery, committee governance. Next: the public CRS ceremony, mainnet, and a cross-chain bridge.

**One line:** *A GPU should not have to choose.*

`gpncoin.ai` · whitepaper + docs at https://gpncoin.ai/docs/
