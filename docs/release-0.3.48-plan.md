# Release 0.3.48 — consolidated from the 2026-11-14/15 review

Everything below came out of one session of testing the AI side. Each item names the
evidence that produced it, the rule it enforces, and the test that proves it. Nothing here
is "done" until the test passes on a running system — see `AGENTS.md` "Testing Gate".

## A. Routing: capability only, first come first served (in progress)

**Evidence.** Four healthy providers all advertising `llama3.2:1b`; six concurrent requests
were served **6/6 by the provider on the access point's own host**. Cause:
`route_candidates` (gpn_scheduler.rs:1989-2111) scores
`unverified + busy_tier + active*100 + effective_latency_ms + pull_penalty` and sorts
ascending. Loopback latency beats every remote provider, always.

**Rule (owner).** The access point filters by capability only — capability, model present (or
pullable), VRAM/RAM for that model, GFLOP floor with "unknown is not a pass", not unfunded,
recent pull. Selection is FIFO by pull arrival. No latency, no busy penalty, no tie-break.

**Node side.** The provider decides whether to pull: not already serving, GPU not busy with a
seal or another job, operator has not set it to idle.

**Test.** Two eligible providers, N concurrent jobs, one with a deliberately inflated latency
override → jobs must alternate. An ineligible provider polling first is never chosen.

## B. `accept_job` must be fee-free (runtime change, spec bump)

**Evidence.** The AP excludes `unfunded` providers because a provider must submit a signed
`accept_job` and pay a fee; with no balance the job can never be claimed (incident #6551).

**Why it matters.** Joining is permissionless — unsigned PoW entry, `Pays::No`, ED=0, no
deposit. Then the first paying job requires funds. **Onboarding is permissionless; earning is
not.** That contradicts the network's premise.

**Rule.** `accept_job` becomes `Pays::No` (unsigned if an account check still requires it),
with the same PoW/attestation proof used for entry so it cannot be spammed for free. Once it
lands, the AP's `unfunded` gate is removed and eligibility really is capability-only.

**Test.** A provider account with zero balance accepts a job end to end; a spammer sending
`accept_job` without the proof is rejected.

## C. Per-job attribution

**Evidence.** `/jobs` returns `provider: null` for every row, and the gpn job id in the
response does not match the chain job id in the list. Which provider served a job is knowable
only from that provider's log.

**Rule.** Record the serving provider for every lease and expose it. Without it, fairness
cannot be observed — the 6/6 localisation went unnoticed until a test was forced.

**Test.** Each served job names its provider in `/jobs`; a six-job run can be attributed.

## D. Registry states instead of one `healthy: false`

**Evidence.** Two provider rows report `healthy: false` with no peer id and no models —
indistinguishable from a node that was serving yesterday and died this morning.

**Rule.** Four distinct states: `serving-now`, `registered-never-seen`, `stale (last seen N)`,
and `on-a-fork` (from chain-state). Never collapse them.

**Test.** A node that stops heartbeating moves to `stale` with its last-seen age, not to a
silent false.

## E. Sealing gate on a recent signed anchor

**Evidence.** Box 1 and box 3 mined a private branch for ~4,000 blocks because each had one
peer — the other exile — and the rule pauses sealing only at **zero** peers.

**Rule.** Do not seal unless our hash at a **recently signed anchor** matches. Weekly anchors
are too old to judge a 4,000-block divergence; anchors need to be frequent (hourly) and
published alongside `seeds.json`.

**Test.** A node on a branch that diverges above the newest anchor stops producing blocks and
says why, instead of mining a private chain.

## F. The last SDK ban: "Peer disconnected with inflight after backoffs"

**Evidence.** Seven occurrences in twenty minutes on 2026-11-14, pre-existing; drops a peer
that has done nothing wrong, for the SDK's own retry behaviour.

**Rule.** Same class as the repeat ban: cap the penalty, never fatal, throttle instead.

**Test.** A peer that disconnects mid-request is not banned after three occurrences.

## G. Node 3 must survive a reboot — **OWNER DECISION 2026-11-15: NOT DOING THIS NOW**

**Evidence.** Its task is `/SC ONLOGON`, interactive only — a reboot with nobody logged in
leaves the machine down.

**Decision.** Jeremy declined the boot-time task (it needs stored credentials or S4U, which
changes security policy on the machine). **Do not implement G.** Leave the logon-triggered
task exactly as it is.

**Accepted consequence, recorded so it is not rediscovered as a mystery:** after a reboot of
box 3 with nobody signing in, GPN stays down until a person logs on. That is a known,
deliberate limitation, not a defect to fix later without asking.

## Order

1. A (in progress) and C — routing correctness and the ability to see it
2. F — the remaining peer-drop
3. E — stop isolated nodes minting
4. D — honest registry states
5. B — runtime change, its own release with a spec bump

G is out of scope by owner decision.
