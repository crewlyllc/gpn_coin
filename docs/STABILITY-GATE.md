# Goal for now: a version that doesn't boot a newcomer off the network

Owner decision, 2026-11-18. **Nothing gets advertised until these pass.** Every item below is
checked on a machine we do not touch, judged only by what the network reports.

## The gate

| # | Test | Pass condition | Blocked by |
|---|---|---|---|
| 1 | Clean install, no prompt | no firewall dialog, no console window, node detached, `peers >= 1` within 5 min | 104, 106, 208 |
| 2 | Two unattended updates | the version changes twice on its own; a bad build rolls back and keeps running | 207 (in flight), 209 |
| 3 | Seven days peered | no unexplained disconnects; peer count never sits at 0 for more than a minute | currently passing |
| 4 | A job served by a remote node | attribution names a provider that is not the AP's own host | not yet verified |
| 5 | Self-heal | a deliberately forked node returns to the canonical chain with no human action | not yet verified |
| 6 | No silent failures | no `unfunded` refusal, no `Incompatible pruning modes`, no `update declined` that never retries | 205, 203 |

## The order

1. **0.3.60** — update-confirm for the three scenarios (slow start, RPC down, process died), with remediation and rollback.
2. **0.3.61** — `accept_job` free (205), whole-family stop (206), firewall rules at install (208), signing or Defender handling (209).
3. **Seven-day soak on box 3** — unmanaged, no SSH, judged by `/api/network` and our peer view only.

## Why the soak is the real test

Every failure this week appeared after hours, not minutes: an update at the six-hour mark, a fork
after 4,000 blocks, a console death on the first restart. A version that survives ten minutes
proves nothing. **A week of updates and jobs is what can be advertised.**

## What "advertise" then means

Not "it works" — "here is what is verified, here is what is not". A public claim that outruns the
gate is how the phrase "every node mines and serves" became untrue on our own boxes while the
documents said otherwise.
