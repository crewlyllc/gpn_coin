# NEEDS-TRIAGE

Questions the release loop thinks may need a decision. The assistant triages these and
messages Jeremy only when the answer changes what we build.

Format: `- [ ] <item id> — <question> | options: A … B … | recommend: … | raised <date/run>`


- [ ] 243 — Spec 54 is source-complete but not enacted, so the *live* testnet pool still answers a refused unsigned entry with `BadProof` ⇒ "Transaction has a bad signature" instead of the new `Custom(n)` reason. Row 243's code and client translation are done and tested (checklist 0.3.70 §3b evidence). | options: A build a spec-54 WASM and `set_code` it on testnet now (same flow as spec 53 today; ~40 min of build + one sudo extrinsic; the chain keeps sealing, and it makes the fix actually observable) · B batch it with the next runtime change in a later release (no chain action now; the misleading message stays live in the meantime, and 0.3.70's runtime source is one spec ahead of the chain — the state `check-spec-version.sh` explicitly allows) | recommend: A — the whole point of row 243 is that the old wording cost two investigations, and a diagnostics-only change is the cheapest possible runtime upgrade to practise the flow on. Say B if you would rather not touch the chain outside a release cut. | raised 2026-11-22 (loop run, 0.3.70 box 3)

- [ ] 194 — Box 3 deliberate fork test: the code half is proven (13/13 fork_choice tests, live call site), but the live half has no venue — box 3 is off the tailnet and a >288-block heavier branch needs ~72 min of isolated sealing on a GPU box the standing rules exclude. | options: A re-provision/restore a fork-crafting box (costs a machine + ~1 h of GPU time, closes the row for real) · B retire the live half and keep the unit-level proof (no cost; the depth guard stays proven only in-crate) | recommend: B — the guard is exercised by the crate tests on the same code path, and a dedicated fork box is real GPU time for one assertion; re-open as a NEW row if a spare box appears | raised 2026-11-20 (loop run, closing 0.3.62)

- [x] 217 — Block-cadence target: is "no gap >45 s **and** fewer than 5 % under 3 s" the acceptance test we want, or do we restate it statistically? The measured 0.3.62 sample (mean 16.0 s) matches an exponential arrival exactly: 6 gaps >45 s vs 5.95 predicted, 17 under 3 s vs 16.93 predicted, so the distribution is the arrival process of 3–6 independent miners, not the retarget. The two clauses are jointly unsatisfiable at any difficulty: <5 % under 3 s needs a mean >58.5 s, which puts 46 % of gaps over 45 s. | options: A restate the done-when as "mean 15 s ±10 % over 100 blocks, no gap > 10x mean" and fix the client-side symptom separately (callers retry/poll instead of a fixed 30/60 s timeout) — no consensus change · B add a consensus-level smoothing rule (minimum inter-block gap / moving-average target) so blocks are paced rather than Poisson — real cadence control, but it is a runtime + spec change and it weakens the "independent miners, Bitcoin-like cadence" property · C keep the current wording and accept the row never closes | recommend: A, plus one separate anomaly hunt — the 152.2 s gap is not arrival noise (p ≈ 7.4e-5, expected 0.007 per 99 blocks), so a real authoring stall exists and belongs in its own row, not in a difficulty-parity box | raised 2026-11-20 (loop run, bundling 0.3.63)  → ANSWERED 2026-11-20: clause 2 becomes "every gap above 3x the window mean has a cause in the node's own output"; clause 1 (10x mean) stands. 218 stays as the real defect.

- [x] 217/218 split (2026-11-20 10:53 PT, loop run working 0.3.63 §1) — the acceptance wording (above) is still the only thing 217 waits on, but the defect it was hiding is now row **218** so the two are separable: 218 is "the chain produced no block for 153 s at the *lowest* difficulty in the window, and nothing in any log says why" (evidence in the row; p ≈ e^-19 under the observed rate, expected gap ~8 s), and it is a real defect under every candidate wording. Recommendation for the assistant: answer 217 as **A** (restate as "no gap > 10× the 100-block mean", which today's 153 s = 9.6× would pass) *and* keep 218 open as the actual failure to fix, because a 10×-mean threshold alone would have signed off a silent 2.5-minute stop. 0.3.63 currently carries no code: if 217 goes to A, it needs no runtime change and 0.3.63 ships as the wording/clamp-free batch or is rolled into the next one — that is the sequencing call I am not making.  → ANSWERED 2026-11-20: clause 2 becomes "every gap above 3x the window mean has a cause in the node's own output"; clause 1 (10x mean) stands. 218 stays as the real defect.

- [x] 217/218 — cadence threshold is unsatisfiable as worded | decided 2026-11-20 by the assistant:
      **A** — restate 217's target as "no gap above 10x the 100-block running mean, and no gap
      above 45 s at all" (the 153 s stop must fail it), and keep 218 as the real defect (a 232 s
      silent stop at the window's lowest difficulty). Wording-only batches are allowed: 0.3.63
      ships as a wording+checklist batch, and 218 rides the next code release.

- [x] 217/218 — the restated clause 2 ("no gap above 45 s at all") is still unsatisfiable on a
      healthy chain, so `scripts/check-block-cadence.py` cannot go green in 0.3.63 even after 218
      is fixed. | **decided 2026-11-20 11:58 PT by the assistant: option A.** Clause 2 is now
      "every gap above 3x the window mean has a cause in the node's own output" (sealer heartbeat /
      slow-solve WARN / blocked-submit WARN / GPU error / panic / restart / self-heal), clause 1
      stays the blunt 10x-mean detector. Implemented in `scripts/check-block-cadence.py` with a
      `--self-test` of the clause logic (7/7) and `--journal-unit` attribution; a 50 s gap with
      nothing in the log still FAILS, a 153 s gap with `sealer: still searching ... after 140.0s`
      passes as explained. Consequence recorded: 0.3.63 (no runtime change) ships the wording +
      gate; the live green is row **218**'s acceptance and rides the release that carries
      `fe71e35`.
      — retained below is the analysis that led to it. | arithmetic: at the measured mean of 15.4–16.6 s a memoryless hashing race gives
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

- [ ] self-heal/IPv6 — **the fleet's divergence self-heal cannot fetch the signed checkpoint, every ~5 min, and the cause is this host's IPv6 fallback — not the URL.** Evidence (2026-11-21 07:1x PT, audit): fleet log `cannot fetch the signed checkpoint (GET https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json: error sending request for url …)`; the same URL with `curl -4` → **200**; plain `curl` (and `curl -6`) → **000 / "Resolving timed out after 10000 ms"**, as do `https://github.com/`, `https://api.github.com/`, `https://objects.githubusercontent.com/`. `resolvectl query` answers AAAA records; `ip -6 addr show enp42s0` has **only fe80::/64** and `ip -6 route` has no default route. `git ls-remote` and `curl -4` both work, so it is a client fallback failure, not a dead uplink. Effect: the divergence guard has been effectively off since at least 07:03 PT. | options: A set IPv4 precedence in `/etc/gai.conf` (`precedence ::ffff:0:0/96 100`) — one file, reversible, no reboot · B disable IPv6 addressing at NetworkManager for `enp42s0` so DNS stops returning AAAA · C leave it and make `run-fleet.sh` force IPv4 in its HTTP client | recommend: A — smallest blast radius, benefits every client on the host, and `git`/`curl -4` already prove the IPv4 path is healthy. Standing rules forbid the auditor from changing DNS/firewall, so this needs owner approval. | raised 2026-11-21 (daily audit)

- [ ] **231 — enact spec 53 on the testnet chain (registration must be free on every path)** — the
      source half is done and tested, and the spec-53 WASM is **built and hash-pinned** as
      `wasm/gpn_runtime_spec53_free_registration.compact.compressed.wasm` (534,023 B, `sha256
      d5c1b69a32f3735da4e7e62384bac154fdfa191dbfdfd7f1d41bd33546a3419f`, 2026-11-21 14:37 PT; live
      node at 18:18 PT still answers `specVersion 52`, and `build-spec53-wasm.sh` states it never
      applies the artifact);
      `wasm/gpn_runtime_spec53_free_registration.compact.compressed.wasm`; the runtime is *not on
      chain*, so `register_provider` still charges a fee and the row cannot be closed. | question:
      may the loop enact spec 53 on the testnet chain, or will you sign it? | options: **A** you
      apply it with the offline testnet sudo key D5 (`Sudo.sudo(System.set_code)` via
      `gpn-agent/src/bin/gpn_upgrade_runtime.rs`, the path specs 44/45/51 took; ~2 min, no fork) ·
      **B** you delegate the enactment to the governance committee (3-of-5 + 10-block timelock,
      `gpn_gov_upgrade`) · **C** park the enactment to the next release cut so one `set_code`
      carries several runtime changes | recommend: **A** — the wasm is hash-verified and feature-
      matched to the live chain, the change is one weight annotation (no storage migration, no
      constant change), and doing it now is what makes "joining costs nothing" true on the network
      rather than just in git. | raised 2026-11-21 15:0x PT (loop run, 0.3.66 §3b item 231)
      **RE-ANNOUNCED 2026-11-21 16:20 PT** (4th run touching 0.3.66 §5): still unanswered, and
      0.3.66's binary half is now built and in flight — the only thing standing between "free
      registration" in git and free registration on the network is this key decision. Recommendation
      unchanged: **A**.
      **RE-ANNOUNCED 2026-11-21 17:56 PT** (5th run touching 0.3.66): unchanged and now the only open
      item — 0.3.66 is published, deployed and confirmed (`system_version 0.3.66`), the checklist is
      50/51 with this box deliberately unticked. The spec-53 WASM is built and hash-verified
      (`blake2_256 0xd4224aa6…a3e6a`, 534,023 B); everything else about this release is done.
      Recommendation unchanged: **A**.
- [x] 237 — Box 1 cannot reach 0.3.67 (or any later version) without a reboot, because #236's shutdown gate reads a stale `PendingFileRenameOperations` queue (118 dead entries; uptime 11d 15h; CBS and WU both say no reboot required) as "the system is shutting down" and defers every cycle: `deferred: system is shutting down - a reboot is pending (PendingFileRenameOperations is non-empty)`, `wrapper exit 75`, next check in 58 min. Until it is unblocked, no Windows node carries 0.3.67 and §10's "change observed on a real machine" cannot be ticked for 233–236. | options: A narrow the gate in the next release so only an *in-progress* shutdown/restart/logoff defers and pending-reboot markers are log-only, and reboot box 1 once after that build lands (correct fix, but box 1 stays on 0.3.66 for one more version; costs one reboot of the build runner, mid-CI jobs on it fail and must be re-run) · B reboot box 1 now so it picks up 0.3.67 on the next cycle, and narrow the gate afterwards (box 1 on 0.3.67 today, but it reboots into the same over-broad gate, so the very next version wedges again unless A ships too; any in-flight runner job dies) · C clear the stale PFRO entries by hand (and then each previous entry is consumed at the next boot anyway) | recommend: **A**, then one reboot once the narrowed build is on `C:\gpn\bin` — the gate is a defect of our own making and rebooting into it just re-arms it; if the reboot is inconvenient this week, do B as a stopgap and still ship A in 0.3.68 | raised 2026-11-22 (loop run working 0.3.67 §10)
      **RESOLVED 2026-11-22 09:5x PT — option A landed and no owner answer was needed.** The narrowed gate
      shipped in 0.3.68 (`1bc9488`), box 1 applied 0.3.68 at 08:42–08:46 PT and the deploy log reads
      `118 PendingFileRenameOperations entries - logged only, does not defer` → `installed 0.3.68` →
      `confirm: process=up rpc=yes 123s version=0.3.68` → `CONFIRMED`. Rows 237/238 are `COMPLETE 0.3.68`
      in `GPN-LIST.md`. Nothing left to decide here.
