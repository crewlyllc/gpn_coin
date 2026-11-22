# Design — settlement depth instead of finality (2026-11-22)

**Decision:** the chain has **no finality gadget** and will not pretend to have one. Settlement is
expressed as **depth**, published as a number, and GRANDPA is deferred until there are independent
operators to run it. Owner-approved 2026-11-22.

## 1. What is true today

```
chain_getFinalizedHead  → 0xb8e4636a…19a5c   = the genesis block, forever
head                    → #346,372 and advancing
runtime construct_runtime!  → System, Timestamp, Balances, TransactionPayment, Sudo, Governance,
                              Bridge, GPUPoW, GPUAttestation, Liveness, ComputeMarketplace
                              … and no Grandpa
```

With no GRANDPA pallet nothing is ever voted final, so "finalized" means genesis. PoW authoring and
heaviest-chain fork choice work; `MAX_REORG_DEPTH = 288` bounds how deep a competing chain can win.

## 2. Why not add GRANDPA now

Finality is a claim of the form *"a set of parties we do not control agreed this block is
permanent."* Today the validator set would be the same three-to-five machines one operator runs.
That is not a weaker version of the guarantee — it is a **false absolute**: the operator could
finalise a fork that everyone else rejects, and every client would be told it was irreversible.

Publishing a guarantee that cannot hold is worse than publishing none, because it removes the
reader's own caution. Proof of work with a stated confirmation depth is an honest probabilistic
claim; a captured finality set is fiction with a protocol's authority.

## 3. What we publish instead

- **`settlement_depth: 288`** in `network.json`, `releases.json`'s sibling metadata, and
  `GET /api/network` — with the time equivalent at the current block interval (`≈72 min` at 15 s).
- **`GET /api/finality`** →
  `{"mode":"depth","confirmations":288,"finalized_head":null,"note":"this chain has no finality gadget"}`.
  A caller that needs a boolean gets a truthful answer instead of a block hash from genesis.
- **A loud warning wherever `finalized` is documented:** explorer, API docs, `nodes.html`, and the
  `chain_getFinalizedHead` mention in any guide — *"returns genesis; use depth"*.
- **One sentence in the operator docs:** *"a payment is settled at 288 blocks deep (≈72 min); a
  shallower block can still be reorged."*

## 4. The revisit condition, written down so it is not a mood

Adopt GRANDPA (or any finality gadget) only when **all** of these hold:

1. **≥ 7 independent operators** running validating keys that Jeremy does not hold, with at least
   **4 distinct administrative domains**;
2. a published validator-set change procedure (who can add/remove a validator, and how that is
   authorised) — otherwise the set is secretly permissioned;
3. a demonstrated ability to survive the loss of any single operator without stalling;
4. a monitoring story for *"finality stalled"* — a stalled gadget halts settlement, and a chain
   that stops finalising without saying so is worse than one that never did.

Until then, depth is the contract.

## 5. Acceptance

1. `network.json` and `/api/network` report `settlement_depth: 288` and the time equivalent.
2. `GET /api/finality` answers as above; `finalized_head` is `null`, never a genesis hash.
3. Docs that mention `finalized` carry the warning; a test asserts the string is present.
4. A tool that asks for a *number of confirmations* (rather than a boolean) is the documented
   pattern, with a curl example for a 288-deep block.
5. The revisit conditions above appear in `docs/` so the decision can be re-litigated on evidence,
   not recollection.
