# NEEDS-TRIAGE

Questions the release loop thinks may need a decision. The assistant triages these and
messages Jeremy only when the answer changes what we build.

Format: `- [ ] <item id> — <question> | options: A … B … | recommend: … | raised <date/run>`


- [ ] 194 — Box 3 deliberate fork test: the code half is proven (13/13 fork_choice tests, live call site), but the live half has no venue — box 3 is off the tailnet and a >288-block heavier branch needs ~72 min of isolated sealing on a GPU box the standing rules exclude. | options: A re-provision/restore a fork-crafting box (costs a machine + ~1 h of GPU time, closes the row for real) · B retire the live half and keep the unit-level proof (no cost; the depth guard stays proven only in-crate) | recommend: B — the guard is exercised by the crate tests on the same code path, and a dedicated fork box is real GPU time for one assertion; re-open as a NEW row if a spare box appears | raised 2026-11-20 (loop run, closing 0.3.62)

- [ ] 217 — Block-cadence target: is "no gap >45 s **and** fewer than 5 % under 3 s" the acceptance test we want, or do we restate it statistically? The measured 0.3.62 sample (mean 16.0 s) matches an exponential arrival exactly: 6 gaps >45 s vs 5.95 predicted, 17 under 3 s vs 16.93 predicted, so the distribution is the arrival process of 3–6 independent miners, not the retarget. The two clauses are jointly unsatisfiable at any difficulty: <5 % under 3 s needs a mean >58.5 s, which puts 46 % of gaps over 45 s. | options: A restate the done-when as "mean 15 s ±10 % over 100 blocks, no gap > 10x mean" and fix the client-side symptom separately (callers retry/poll instead of a fixed 30/60 s timeout) — no consensus change · B add a consensus-level smoothing rule (minimum inter-block gap / moving-average target) so blocks are paced rather than Poisson — real cadence control, but it is a runtime + spec change and it weakens the "independent miners, Bitcoin-like cadence" property · C keep the current wording and accept the row never closes | recommend: A, plus one separate anomaly hunt — the 152.2 s gap is not arrival noise (p ≈ 7.4e-5, expected 0.007 per 99 blocks), so a real authoring stall exists and belongs in its own row, not in a difficulty-parity box | raised 2026-11-20 (loop run, bundling 0.3.63)

- [ ] 217/218 split (2026-11-20 10:53 PT, loop run working 0.3.63 §1) — the acceptance wording (above) is still the only thing 217 waits on, but the defect it was hiding is now row **218** so the two are separable: 218 is "the chain produced no block for 153 s at the *lowest* difficulty in the window, and nothing in any log says why" (evidence in the row; p ≈ e^-19 under the observed rate, expected gap ~8 s), and it is a real defect under every candidate wording. Recommendation for the assistant: answer 217 as **A** (restate as "no gap > 10× the 100-block mean", which today's 153 s = 9.6× would pass) *and* keep 218 open as the actual failure to fix, because a 10×-mean threshold alone would have signed off a silent 2.5-minute stop. 0.3.63 currently carries no code: if 217 goes to A, it needs no runtime change and 0.3.63 ships as the wording/clamp-free batch or is rolled into the next one — that is the sequencing call I am not making.

- [x] 217/218 — cadence threshold is unsatisfiable as worded | decided 2026-11-20 by the assistant:
      **A** — restate 217's target as "no gap above 10x the 100-block running mean, and no gap
      above 45 s at all" (the 153 s stop must fail it), and keep 218 as the real defect (a 232 s
      silent stop at the window's lowest difficulty). Wording-only batches are allowed: 0.3.63
      ships as a wording+checklist batch, and 218 rides the next code release.

- [ ] 217/218 — the restated clause 2 ("no gap above 45 s at all") is still unsatisfiable on a
      healthy chain, so `scripts/check-block-cadence.py` cannot go green in 0.3.63 even after 218
      is fixed. | arithmetic: at the measured mean of 15.4–16.6 s a memoryless hashing race gives
      P(gap > 45 s) = e^(-45/16.6) = 5.4 %, i.e. **5.3 offending gaps expected per 99**; P(a clean
      100-block window) = 0.946^99 = **0.4 %**. Live confirmations: 11:12 PT sample 4 violations
      (mean 15.38 s), 12:05 PT sample 6 violations (mean 16.61 s) — both exactly the predicted
      count, and neither is a code defect. To make clause 2 pass 95 % of the time the mean gap
      would have to be ≤ 5.9 s, i.e. a target block time of ~6 s, not 15 s. | options: A replace
      clause 2 with row 218's own done-when — *every gap above 3× the window mean has a cause in
      the node's output* (seal heartbeat / slow-solve WARN / blocked-submit WARN / GPU error /
      self-heal window), keeping clause 1 as the blunt 10×-mean detector (no consensus change;
      makes the gate about attributability, which is the actual failure mode) · B keep the
      absolute clause and lower the target block time to ~6 s (a difficulty/target change, more
      blocks per hour, retarget churn) · C keep the wording and accept a ~0.4 % pass rate (the
      gate is then effectively never green and stops being a signal) | recommend: **A** — the
      heartbeat shipped on branch `fix/218-sealer-heartbeat` (5 unit tests, clippy clean with CI's
      allow-list) makes exactly that evidence exist, and a 50 s gap under A is still a FAIL
      because nothing in the log explains it. | raised 2026-11-20 12:10 PT (loop run, 0.3.63 §2)
