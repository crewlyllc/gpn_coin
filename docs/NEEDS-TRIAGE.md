# NEEDS-TRIAGE

Questions the release loop thinks may need a decision. The assistant triages these and
messages Jeremy only when the answer changes what we build.

Format: `- [ ] <item id> — <question> | options: A … B … | recommend: … | raised <date/run>`


- [ ] 194 — Box 3 deliberate fork test: the code half is proven (13/13 fork_choice tests, live call site), but the live half has no venue — box 3 is off the tailnet and a >288-block heavier branch needs ~72 min of isolated sealing on a GPU box the standing rules exclude. | options: A re-provision/restore a fork-crafting box (costs a machine + ~1 h of GPU time, closes the row for real) · B retire the live half and keep the unit-level proof (no cost; the depth guard stays proven only in-crate) | recommend: B — the guard is exercised by the crate tests on the same code path, and a dedicated fork box is real GPU time for one assertion; re-open as a NEW row if a spare box appears | raised 2026-11-20 (loop run, closing 0.3.62)
