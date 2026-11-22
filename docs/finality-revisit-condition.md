# The GRANDPA revisit condition

**Status: not met (checked 2026-11-22). Do not adopt a finality gadget until all four hold.**

This file is the answer to one question: *what would have to be true before this network may call a
block final?* It exists so that decision is made on evidence rather than on mood, and so that a
future proposal can be refused or accepted point by point instead of by argument.

Background: `docs/design-settlement-finality.md`. Settlement today is **depth** —
`settlement_depth: 288` (≈72 min at 15 s), published by `network.json` and `GET /api/finality`
with `finalized_head: null`. `chain_getFinalizedHead` returns **genesis, forever**, because the
runtime has no GRANDPA pallet.

## The four conditions

Every one must hold. "Mostly" is not a state — a finality set that is one operator short is the
same fiction as a finality set that is three operators short.

### 1. Independent operators

**≥ 7 operators running validating keys that Jeremy does not hold, across ≥ 4 distinct
administrative domains.**

- *Why:* finality means "parties we do not control agreed this is permanent". Keys we hold are not
  independent parties, however many machines they sit on.
- *How to measure:* a published operator registry listing, per operator, a validating key, a
  contact, and an admin domain (the party that can pull the plug — hosting account, org, person).
  Count distinct keys **not** held by us and distinct admin domains. A machine count is not an
  operator count.
- *Today:* one operator (Jeremy). There is no published operator registry yet, so the count is not
  merely small — it is unmeasurable, which counts as zero.

### 2. Published validator-set change procedure

**Who can add or remove a validator, and how that is authorised, written down and implemented.**

- *Why:* without it the set is permissioned in secret, and the finality guarantee reduces to the
  honesty of whoever can edit the set.
- *How to measure:* a document naming the authorisation rule (e.g. on-chain governance referendum
  with a stated threshold and delay), the mechanism that enforces it, and a test or live run that
  shows an unauthorised change is rejected.
- *Today:* not written, not implemented. `sudo` exists in the runtime, which is the opposite of a
  published procedure.

### 3. Survival of any single operator's loss

**The network keeps producing and finalising with any one operator gone.**

- *Why:* a gadget whose quorum dies with one machine halts settlement for everyone, and a chain
  that stops finalising without saying so is worse than one that never claimed finality.
- *How to measure:* with N validators, stop one, observe continued finality (block number and
  finalised head both advancing) for at least one full round; repeat for each operator, or for a
  representative sample plus the argument that the sample generalises.
- *Today:* not tested — there is no validator set to lose a member of.

### 4. A finality-stall alarm

**Something notices when finality stops advancing, and says so where an operator will see it.**

- *Why:* adoption converts settlement from a probabilistic claim into a liveness dependency. The
  monitoring has to exist *before* the dependency, not after the first stall.
- *How to measure:* a check that compares `chain_getFinalizedHead`'s block against head over a
  window and alerts when the gap grows past a threshold; demonstrated by forcing the condition
  (stop the validators in a test net) and capturing the alert.
- *Today:* no such alarm. Depth-based settlement needs none, which is part of why it is the honest
  choice right now.

## Answering a proposal point by point

A proposal to adopt GRANDPA (or any other gadget) must fill this in, with evidence a stranger can
re-derive — not "we have several operators":

```
proposal: <gadget> at <spec_version>
1. operators:        <N validating keys not held by us> across <M admin domains>
   evidence:         <registry URL / commit> ; keys listed: ...
2. change procedure: <document URL> ; enforced by <mechanism> ; rejected-change test: ...
3. single loss:      <which operator stopped> ; finalised head before/after: ... ; duration: ...
4. stall alarm:      <where it fires> ; forced-stall run: <log line>
verdict:             ADOPT / REFUSE — every line above answered, or the answer is REFUSE
```

If any line is blank, the answer is REFUSE. That is the whole procedure.

## Re-evaluation

The status line at the top is checked whenever someone proposes a gadget, and at least once per
release that touches settlement. Moving a condition from *not met* to *met* means editing this file
with the evidence inline — the same rule as a checklist tick: a claim without a command output or a
URL is not a claim.

Wording of these conditions is deliberately identical to §4 of
`docs/design-settlement-finality.md`; `gpn-agent/tests/settlement_docs.rs` asserts both files carry
them, so the two cannot drift apart silently.
