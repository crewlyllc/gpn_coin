# GPN — One-Pager

**What it is.** GPN is a **GPU-native network** where every GPU **mines _or_ serves AI compute — auto-switching** to whichever pays more. Same card, one install, no manual re-listing.

**The problem.** A GPU owner has two ways to earn — mine a coin, or rent the card for AI work — and must **pick one**. Mine and the coin dips → you lose. Rent and nobody books you → $0. The card can't move to where the money is.

**The fix.** GPN watches both markets continuously and **moves the GPU automatically**. The chain does coordination + escrow + settlement; the heavy compute runs off-chain.

---

**How it works**
- **Mining** — GPU proof-of-work; the node that produces a block **keeps that block's reward** (Bitcoin-style).
- **Serving AI** — LLM inference, image/video generation, ML inference, fine-tuning — priced **per token** against real cost, escrowed trust-minimally.
- **The switch** — every cycle the node compares mining vs compute revenue in **USD/hr** and flips when one beats the other by >5% (no thrashing). Real stop/start of GPU work.
- **Routing** — buyers hit one endpoint; the network picks the **lowest-latency, least-loaded** verified GPU. Providers are discovered **on-chain**.

**Why it's different**
- **Both markets, one machine** — no incumbent (NiceHash, Vast, RunPod, Render, Akash, io.net) lets a GPU mine *and* serve with an automatic switch.
- **Trust-minimal** — on-chain escrow, optional **payment channels** (no per-job block delay), opaque/private on-chain values.
- **GPU-only, verifiably** — proof-of-work + hardware attestation + a compute challenge; a CPU/iGPU can't earn.

**Token (GPN)**
- **Fair launch via mining** — no ICO, no premine. 12 decimals.
- **Halving every ~2 years** (4,204,800 blocks); block reward 50 GPN @ 15 s.
- **Disclosed, vested treasury** (100 M over 10 yr, 13-of-25 governance).
- **Token value derives from real AI-compute demand** — not just mining inflation.

**Privacy.** A shielded pool supports private transfers and withdrawals (zero-knowledge spends; nullifier-based double-spend protection).

**Where it is.** Live on a public testnet: PoW chain, per-node rewards, the full AI marketplace (LLM/image/video/ML/fine-tune), payment channels, shielded spends, on-chain provider discovery, peer discovery, committee governance. Next: the public CRS ceremony, mainnet, and a cross-chain bridge.

**One line:** *A GPU should not have to choose.*

`gpn.network` · whitepaper + docs in the project corpus
