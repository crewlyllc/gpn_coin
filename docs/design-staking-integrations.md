# Design — staking ETH and SOL from a GPN node (2026-11-27)

**Status:** discussion + feasibility, owner-requested alongside dual mining. Nothing built.

The request is simple to state and very different to deliver for the two chains, so this document
separates them. Ethereum staking and Solana staking are not two instances of one feature.

## 1. What "staking from the node" can mean

| Shape | What the node does | Custody | Effort |
|---|---|---|---|
| **A. Solo validator** | runs a full node + validator client, signs attestations | **yours** | heavy |
| **B. Delegation** | delegates tokens to a validator; no validation work | **yours** | light |
| **C. Provider integration** | calls a staking provider's API; provider runs the validator | theirs | medium |
| **D. Liquid staking token** | holds stETH / mSOL; no protocol integration at all | wrapper issuer | none |

## 2. Ethereum — the honest arithmetic rules out A on a node box

Solo staking requires:

```
32 ETH per validator
an execution client + a consensus client, always on
~2 TB NVMe for the execution chain (growing), 16–32 GB RAM
a static, reliable internet path; slashing risk if the box dies mid-attestation
```

A GPN node is a mining/AI box that already pauses the GPU for jobs. Asking the same machine to keep a
consensus client in sync — and to be slashed if it drops offline — is a different reliability class.

**So for Ethereum, only three options are real:**

- **C — provider integration** (Kiln / Figment / Rocket Pool's API): the node holds a key that can
  stake, the provider runs the validator. Requires a commercial relationship and custody of a
  staking key. This is a *business* decision before it is a code decision.
- **D — liquid staking**: the node displays a balance of stETH/rETH; there is no protocol to integrate,
  only a wallet balance. Honest, trivial, and not really "staking in the node".
- **A — solo**, only for operators who already run an Ethereum node; we would be duplicating infra
  that has its own failure modes.

**Recommendation: do not build Ethereum solo staking.** If ETH yield matters, show the liquid staking
balance (D) and document it as a wallet, not a feature.

## 3. Solana — delegation genuinely fits the node

Solana staking needs no validator infrastructure:

```
min stake      ~0.01 SOL (no 32-token floor)
mechanism      create a stake account (0.00228 SOL rent), delegate to a validator, wait ~1 epoch
returns        ~6–7% APY, paid as stake-account rewards
exit           1–3 days to deactivate (no slashing for delegators)
keys           the node's own keypair signs; no third party
```

That is: **A → no, B → yes.** A node can delegate its own SOL and earn without running anything
Solana-specific beyond an RPC connection.

**But be precise about what it is not:**

- **Delegated stake is locked for an epoch and deactivating takes 1–3 days.** A "withdraw" button
  that lies about that is worse than no button.
- **Validator choice is the product.** Rewards depend on the validator's uptime and commission;
  a default validator in a curated list is us picking where the operator's money earns. At minimum
  the list must be explainable, and the operator must be able to see the commission and uptime.
- **It is not risk-free.** Delegation has no slashing, but the tokens are locked and validator
  performance varies; APY is variable, never a promise.

## 4. Invariants (same shape as dual mining)

1. **Never mix balances.** GPN earnings, coin-B mining rewards and staking rewards are three
   separate lines. The airdrop export must be untouched by any of it.
2. **Keys are the operator's.** The node signs with its own keypair; no provider key we hold, and no
   "managed" account that the operator cannot withdraw from.
3. **Honest lock state.** If tokens are locked or warming up, the dashboard says so with a date,
   not a spinner.
4. **Opt-in, per chain, with an explicit amount.** No default that stakes anything.
5. **Unbonding is never hidden.** A "deactivate" that takes three days is shown as three days.

## 5. What I would build, in this order

1. **Dual mining: Ergo + Alephium** (owner chose B) — the work already scoped in
   `design-secondary-mining.md`.
2. **Solana delegation** — one chain, real yield, no infrastructure, keys stay local. This is the
   staking feature that fits the product.
3. **Ethereum: display-only** — show stETH/rETH balance if the wallet holds it; no staking code.

**Not now, and probably never:** Ethereum solo validators on node boxes.

## 6. Open questions for the owner

1. **Ethereum: is a provider relationship acceptable?** If yes, whose — and who holds the staking
   key? If no, Ethereum staking is out of scope and only the liquid-token balance is shown.
2. **Solana validator list:** curated by us (with commission/uptime shown), or the operator pastes a
   validator vote account?
3. **Reward accounting:** do staking rewards count toward the operator's dashboard "earnings" (a
   fourth line), or stay in a separate "other chains" panel?
4. **Minimums:** Solana needs ~0.01 SOL + rent; do we refuse below it, or warn?

---

## 7. OWNER DECISIONS (2026-11-27 10:02)

- **Ethereum staking: out of scope entirely** — not solo, not provider, **not even a display-only
  stETH panel**. No Ethereum code path ships.
- **Solana delegation: in scope** (row 295).
- **Dashboard:** staking rewards appear on the dashboard, and a **second "Other earnings" section
  appears only when the operator is staking or mining a second coin** — it is not a permanent
  fixture and never merges with GPN's own earnings line.
