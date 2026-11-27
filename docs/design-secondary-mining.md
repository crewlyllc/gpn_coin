# Design — mining a second coin alongside GPN (2026-11-27)

**Status:** discussion document, requested by the owner before any code. Nothing here is built.

## 1. Why it is worth discussing

A GPN node has three states: **mining GPN**, **serving an AI job**, and **idle**. Today the idle state
is only reached when the GPU cannot mine (no peers, no template) or when an operator stops it.
Operators ask the obvious question: *if my card is already hot and powered, why can it not earn
something in the gaps?*

Two honest answers exist, and they are not the same idea:

- the card is often **not** idle — it is either mining or serving, so a second coin has to **share**;
- revenue from a second coin is **not** free — it costs GPN hashrate, heat, power and, in the worst
  case, AI job latency.

## 2. The three possible shapes

### A. Switch mining (time-sliced, one at a time)

The node mines GPN; when a policy says "GPN is not worth mining right now", it switches the card to
coin B until that stops being true. Never two hashers at once.

- **Pros:** no protocol change; VRAM for one DAG; AI jobs still take the card exclusively;
  the "one GPU, one job" invariant survives.
- **Cons:** earnings are sequential, not additive — you are choosing which chain to support at each
  moment, and every switch costs a DAG changeover (seconds to minutes on 8 GB cards).
- **Triggers worth considering:** GPN difficulty above a threshold, no peers, operator-declared
  off-peak hours, or "GPN has no template to mine".

### B. Concurrent dual mining (two hashers, one card)

Both miners run at once with reduced intensity (the classic "dual mining" of ETH+SC).

- **Pros:** additive-looking revenue.
- **Cons:** two DAGs in VRAM (GPN's Ethash DAG is ~1 GB/epoch and grows; a second Ethash coin adds
  another), so 8 GB cards are marginal and 4 GB cards are out; both rates drop; the card runs hotter
  and louder; and an AI job arriving mid-cycle must now interrupt **two** miners.

### C. Merge mining (one proof, two chains)

The same Ethash solution is submitted to both chains — the Namecoin/Dogecoin model.

- **Pros:** no extra hashrate, genuinely free revenue where the second chain supports it.
- **Cons:** **the second chain must implement auxiliary-PoW** (a parent-block commitment field and a
  verifier). Almost no active chain does. This is a change to *their* protocol, not ours; out of our
  control. It is also a change to **our** block structure if we want to be the parent.

**Recommendation: A first, B as an explicit opt-in for people who accept the trade, C only if a
partner chain already supports it.**

## 3. What must not break

Whatever is chosen, these hold:

1. **AI jobs outrank mining.** A job arriving must take the card within the same bound as today
   (mining pauses). A second coin must never delay a lease.
2. **One job at a time.** No second hasher may run while a job holds the GPU — not even at low
   intensity.
3. **Honest accounting.** Rewards from coin B do not appear in GPN's "earnings today"; the dashboard
   must show them as a separate line or not at all. Conflating them would corrupt the airdrop record.
4. **No hidden spend.** Switching to another chain costs power and heat; the operator opts in, and
   the log says when the card left GPN.

## 4. Open questions for the owner

1. **Which coin?** The answer drives everything: an Ethash-family coin shares tooling (we already
   have an Ethash miner and DAG builder); a memory-light coin (KAWPOW, Autolykos) needs a second
   miner implementation; a CPU coin is not the same product.
2. **Who decides when to switch** — the operator, or a profitability signal? We have no price feeds
   on chain; a "switch on difficulty" rule needs no oracle and is honest.
3. **Default on or off?** Off is safer: a node that switches chains without being asked looks like a
   bug to every dashboard.
4. **Does it dilute the pitch?** GPN's claim is "your GPU mines *and* serves AI on this network".
   Mining a second coin during GPN's gaps is defensible; mining it *instead of* GPN when GPN is
   profitable is not.
5. **Pool or solo?** Most second coins require a pool for practical payout — which introduces a
   third party, an account, and a payout address that is not the node's wallet.

## 5. Smallest useful first step (if approved later)

Switch mining, one alternative chain, operator opt-in, no profitability oracle:

```
GPN_MINE_SECONDARY=<coin>       opt-in, empty = off
GPN_SECONDARY_ADDRESS=<addr>    payout address, validated at startup
policy: mine GPN; if no GPN template for N minutes OR GPN difficulty > X, mine <coin> until
         either a job arrives or GPN becomes minable again; log every transition with the reason
```

That is a policy engine and a second miner binary — days, not weeks — and it is reversible by
clearing one environment variable.

## 6. What this is not

It is not a way for the network to pay two rewards for one unit of work, and it is not merge mining
by another name. GPN's block reward still goes to whoever sealed the GPN block. A second chain pays
its own way, on its own rules, to the same operator.
