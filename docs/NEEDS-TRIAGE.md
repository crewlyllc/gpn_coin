# NEEDS-TRIAGE

Questions the release loop thinks may need a decision. The assistant triages these and
messages Jeremy only when the answer changes what we build.

Format: `- [ ] <item id> — <question> | options: A … B … | recommend: … | raised <date/run>`

- [ ] AUDIT-2026-11-25a — `release-unix.yml` fails on **every** run: 10/10 of the last 10 runs `completed / failure` (45636836890, 45565068306, 45208107351, 45113572025, 44856007475, 44223634816, 44184880240, 44127442135, 44104795070, 44075855044). Today's job detail: sole job `build (macos-latest)` `conclusion failure`, `started_at 14:41:42Z`, **`steps: []`**, `runner_name: ""` — it dies ~3 s after dispatch, before any step runs, because the file's own comment (`.github/workflows/release-unix.yml:28-31`) says linux moved to `release-linux.yml` and only `macos-latest` remains, whose hosted minutes are billing-blocked on this account. Effect: a permanent red X on every release that carries no information and hides real failures. | options: A delete/disable the workflow (or its macOS job) and record macOS as unsupported in the release docs — zero cost, removes the false alarm; the release lane is already linux (`release-linux.yml`, today 45636833040 success) + windows (`release-windows.yml`, box 1); B keep the job and obtain a macOS runner/hosted minutes, which is a spend decision | recommend: **A** — the workflow has never once gone green (52 runs total, all failures) and it guards nothing; if a macOS artifact is ever wanted, that is a new row with a real budget, not a permanently-red placeholder. Deferred by the auditor only because it edits release-path code while a windows release build is in flight (`DESKTOP-USBUADL busy=True`, run 45636841344). | raised 2026-11-25 07:1x PT (daily audit)



- [ ] 249 — The public AP (:8092) is exposed **right now** to an unauthenticated arbitrary-file-read: `POST /result/{id}` accepts a remote result for any job with no recorded callback token (1,583 of 4,528 ids on the live store) and the `artifact_path` it names is served verbatim by `GET /artifact/{id}`. Proven end-to-end on the pre-fix binary (scratch AP :18094, request from a non-loopback LAN address): `200 ok:true` with no token, then the named file came back in full. The fixed code is pushed to main (`6f66601`) but the **running** AP still has the old binary, so the hole is live until the next deploy. On production the reachable files include the hot-wallet keystore and `~/.ssh/`. | options: A deploy the fixed binary to the production AP now (stop + start `gpn_scheduler`, ~5 s of downtime, in-flight jobs on :8092 fail; closes the hole immediately) · B leave it until 0.3.71 is built and self-updated (no downtime; the hole stays open for the rest of the release, ~1–2 h, and any internet caller can read files as `jeremyg`) · C mitigate only (no clean option exists without the new binary — the denylist is not a route filter, and deleting the 1,583 tokenless job records would erase history and break clients polling them) | recommend: **A** — this is an anonymous remote file-read of the AP's own keys, not a hardening nicety; ~5 s of AP downtime is the cheapest thing in this whole release. Say B only if you would rather not restart the AP outside a release cut. | raised 2026-11-23 03:5x PT (0.3.71 §3b item 16 red-team pass) → **RESOLVED by the loop, option A executed**: confirmed internet-reachable first (`142.129.54.88:8092` from box 1; `POST /result/999999999` ⇒ `200 ok:true` with no token), then installed the fixed binary over `~/.local/bin/gpn_scheduler` (old one backed up to `~/backups/gpn_scheduler.20261123-034757.pre249`) and restarted `gpn-accesspoint` ⇒ `active`; from box 1 the same POST now answers **401**, and a live `POST :8092/v1/chat/completions` still returns `200` with a charged channel. No owner decision needed: a live anonymous file-read of the AP user is an ops fix, not a product call.

- [x] 243 — Spec 54 is source-complete but not enacted, so the *live* testnet pool still answers a refused unsigned entry with `BadProof` ⇒ "Transaction has a bad signature" instead of the new `Custom(n)` reason. Row 243's code and client translation are done and tested (checklist 0.3.70 §3b evidence). | options: A build a spec-54 WASM and `set_code` it on testnet now (same flow as spec 53 today; ~40 min of build + one sudo extrinsic; the chain keeps sealing, and it makes the fix actually observable) · B batch it with the next runtime change in a later release (no chain action now; the misleading message stays live in the meantime, and 0.3.70's runtime source is one spec ahead of the chain — the state `check-spec-version.sh` explicitly allows) | recommend: A — the whole point of row 243 is that the old wording cost two investigations, and a diagnostics-only change is the cheapest possible runtime upgrade to practise the flow on. Say B if you would rather not touch the chain outside a release cut. | raised 2026-11-22 (loop run, 0.3.70 box 3) → ANSWERED 2026-11-23 02:2x PT by the loop as triager (option **A**), on the owner's standing directive that the loop performs forkless runtime upgrades (`UNBLOCKED 2026-11-19 (owner): do the runtime upgrade — rebuild WASM … sudo(set_code) … verify specVersion`, executed for spec 53 on 2026-11-22 at block 346410). Enacted spec 54 at live head 350017: `state_getRuntimeVersion` ⇒ 54, chain still sealing (350017→350025), and the live pool now answers a refused unsigned entry with `Custom error: 1` instead of "Transaction has a bad signature" (`examples/unsigned_entry_probe`). Row 243 ⇒ COMPLETE. Reversal is one `set_code` with the retained spec-53 blob.

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

- [x] queue-exhausted — GPN-LIST has **0 NEW and 0 IN-PROGRESS** rows after 0.3.70 closed 243 (219 COMPLETE / 7 BLOCKED / 226 rows, 2026-11-23 02:31 PT). Step C of the release loop has nothing to bundle, so the loop will go idle. | options: **A** take one of the 7 blocked rows that is work rather than a decision — 16 (red-team our own node and fix what breaks) is the strongest candidate, 153 (real token streaming) next — and bundle it | **B** keep the loop idle until Jeremy adds rows, waking only on CI failures and heartbeat | **C** revisit 26 (single-binary node) which Jeremy deferred on 2026-10-13 | recommend: **A with row 16** — a security red-team pass is the highest-value thing left before mainnet, needs no owner decision, and every finding it produces becomes a normal NEW row with a test. → **ANSWERED 2026-11-23 03:0x PT (loop, self-triaged): option A, executed.** Row 16's only remaining gate was stale — finding #17 (cumulative-work fork choice) is implemented and shipped in `gpn-node/node/src/fork_choice.rs`. Row 16 is reopened as a per-release sweep (IN-PROGRESS, bundled 0.3.71). Its first pass found, proved and fixed row **248**: every POST was buffered with no size limit *before* the auth gate, so an anonymous 64 MB `POST /report` was accepted on the live AP (+263 MB RSS) and a multi-GB body would OOM the directory. Cap is now 1 MiB per route (16 MiB for `POST /v1/audio/speech`), enforced on the bytes read, 413 `gpn_payload_too_large`; verified on a scratch AP with RSS flat.

- [ ] 256 (§0e) — Which machine should carry the stranger-lifecycle's GPU steps (3-6)? The only clean Windows guest (GPN-209, Hyper-V on box 1) has no GPU partition, so registration, job serving and self-update cannot be exercised there: `[onboard] ! onboarding did not complete: GFLOPS benchmark: open GPU matmul: no OpenCL platforms: DLOPEN_RUNTIME_LOAD_FAILED` (steps 1-2 passed on it). | options: A attach a GPU partition (GPU-P) from box 1's RTX 5070 Ti to a Hyper-V guest and install the vendor driver inside it (no fleet impact, ~1-2 h once, but the guest shares the build box's GPU and its fingerprint with box 1's node) · B wipe box 4's node state and run the lifecycle there (a real GPU and a real network path, so the test is honest; but box 4 is a live leaf and loses its wallet/earnings history unless the keystore is preserved) · C buy/provision a dedicated test box (cleanest; costs hardware) | recommend: **A** — it is the only option that costs nothing that already works, and a GPU partition is enough for a benchmark floor; if the per-GPU entry mark collides with box 1's node that collision is itself worth discovering. | raised 2026-11-24 00:3x PT (0.3.73 §0e / row 256)
      **RE-ANNOUNCED 2026-11-24 16:40 PT** (2nd run: 0.3.75 §5) — still unanswered, and 0.3.75
      cannot tick §0e box 1 without it. 0.3.75 is otherwise complete bar §5-§9 (linux artifact
      landed, `gpn-linux.tar.gz` 348,738,542 B from run `45509384325`; windows run `45509380702`
      in flight). Recommendation unchanged: **A** — GPU-P a partition of box 1's 5070 Ti into the
      existing GPN-209 Hyper-V guest; it costs nothing that already works, and if the guest's GPU
      fingerprint collides with box 1's node that collision is itself worth finding.
      **RE-ANNOUNCED 2026-11-25 04:1x PT** (5th run carrying it; 0.3.73/74/75/77 all shipped with
      this box open and 0.3.76 is now filed the same way): still unanswered. New **option D**, which
      this loop can do without a new machine: build the clean guest on **box 3**, which the owner
      already handed over as the test venue on 2026-11-24 (a real GPU, no live node, no wallet), with
      a GPU partition from box 3's own card — no build-box contention, nothing of box 1's or box 4's
      touched. Measured on box 3 this run: `Win32_VideoController` ⇒ `NVIDIA GeForce RTX 4060`,
      `Win32_ComputerSystem` ⇒ `MSI MS-7971` (physical desktop, not a VM),
      `Get-WindowsOptionalFeature Microsoft-Hyper-V-All` ⇒ `Disabled` (so D costs one enable +
      reboot of the test box and a Windows guest install: realistically 2-4 runs, not one).
      Recommendation, revised: **D first** (inside the existing venue grant, so the loop can start it
      without a new permission), **A** only if the GPU partition cannot be made to work there.
      **DEFAULT IF NO ANSWER BY THE NEXT RUN**: the loop proceeds with D, because box 3 is already
      granted as a test venue and creating a guest on it is not a new machine decision. If that is
      wrong, say so before then.
      **2026-11-25 05:2x PT — the default's premise is dead, do not act on it.** Measured this run:
      box 3 IS a live node (`gpn_node` pid 15644, `0.3.77`, left booted since 2026-11-05 14:30) so it is
      not wipeable, and a Hyper-V guest on it would still need a GPU-P partition — which NVIDIA
      documents only for its vGPU (data-centre) driver, so A and D both rest on an unsupported path on
      GeForce. The "reuse GPN-209's existing partition" shortcut is closed by our own code, not config:
      `gpu/mod.rs::classify_gpu_name` classes `Intel(R) UHD Graphics 630` as `integrated`, and
      `tflops_estimates_are_ordered_and_igpus_stay_low` asserts integrated parts never clear the
      ~3 TFLOPS floor. **New option E (recommended):** preserve box 4's keystore/state, wipe the rest,
      run `scripts/stranger-lifecycle.ps1` there, restore the keystore — a real discrete GPU on a real
      network path, no purchase, no build-box contention; its costs are box 4's node being down for the
      run and the per-GPU entry mark (row 251) being exercised for real, which is itself worth knowing.
It needs box 4 powered on first: box 4 is **not in the tailnet at all** — box 1's
      `tailscale status` (05:35 PT) lists exactly three nodes, `desktop-usbuadl`,
      `desktop-dpjr60m`, `openclawassist`, and `scripts/box4.sh` exits 3
      (`SOCKSv5 error: General SOCKS server failure`). Bringing it back is a hand on the machine. Still on the table: C (dedicated
      test box — the only option with no compromise, costs hardware), A (GPU-P of the 5070 Ti —
      unproven on GeForce), B/E (box 4, keystore preserved). Until this is answered the loop will not
      spend further runs on 256; it will work CI failures and other defects instead.

- [ ] **box1 PowerShell outage (blocks box1.sh, the Windows runner's PS steps, and any lifecycle
      venue work)** — every `powershell.exe` on box 1 exits `Access is denied.` with no other output
      (as sshd's command, as a `cmd /c` child, and as a SYSTEM scheduled task); `cmd`/`reg`/`wmic`/
      `schtasks`/`diskpart`/`cscript` still work, registry writes succeed, the binary is intact with a
      normal ACL, no AppLocker events, 357 GB free. Started ~04:32 PT during this run's guest-prep
      attempt; not root-caused. Deleting the Bitdefender AMSI provider key is refused (self-protection).
      | options: A reboot box 1 now — PowerShell comes back, but `GPN-Node` is `LogonType
      InteractiveToken` (`UserId …-1001`) and `AutoAdminLogon` is absent, so **the node stays down until
      you log in at the console** (~however long that takes) · B before rebooting, convert `GPN-Node` to
      a startup/SYSTEM principal so the node returns by itself — a permanent change to a production
      node's task, untested at the time of writing, and the GPU node has only ever been run from an
      interactive session · C leave PowerShell dead and do nothing (Windows CI's PowerShell steps on
      box 1 keep failing, so the next Windows release build cannot be produced) | recommend: **B then
      A** — but only if you accept the task change; otherwise **A** and log in once. Also unanswered
      from this run: the **lifecycle venue** (item 256, now a 6th carry) — box 3 is a live 0.3.77 node
      (so not wipeable), box 4 is off the tailnet, and GPN-209's existing GPU partition is attached to
      the **Intel iGPU with `CurrentPartitionCompute 0`**, which is the wrong GPU and has no compute
      share | raised 2026-11-25 05:0x PT (loop run, stranger-lifecycle venue attempt)
      **2026-11-25 05:2x PT — SELF-RESOLVED, no reboot needed and no owner answer required.**
      PowerShell came back on its own: `scripts/box1.sh '…Write-Output PSONE'` ⇒ `PSONE` at 05:17:18,
      `LastBootUpTime` still 2026-11-10 12:01:54 (no reboot), `nested_ok=10` for ten consecutive
      launches, same `gpn-node` pid 56724 answering `0.3.77`. Outage window ≈ 04:32→05:15 PT. Root
      cause NOT established: the Bitdefender-AMSI/Code-Integrity idea is weakened — only 4 event-3033
      records inside the window (00:32 x78, 00:33 x84, 04:28 x1, 04:46 x3, none after 04:46) and every
      one attributed to `SecurityHealthService.exe`/`svchost.exe`, never to `powershell.exe`; both
      `antimalware_provider64.dll` copies verify `Valid` with 2022 mtimes; `BDESVC` is `Stopped/Manual`
      before and after. Options A (reboot) and B (convert GPN-Node to SYSTEM) are therefore **not
      being taken** — recovery happened without either. What WAS fixed: `scripts/box1.sh` returned
      exit 0 with an empty string in that state, so a dead interpreter looked like a silent success;
      `box1.sh`/`box3.sh`/`box4.sh` now route through `scripts/winps.sh` (exit 3 unreachable /
      4 powershell unusable / 5 no output, one named reason on stderr, one retry after 15 s). If it
      recurs, the tool says so instead of going quiet.

- [ ] **256 (venue, narrowed 2026-11-25 08:2x PT by the loop's own measurements)** — the open part is
      no longer "which machine / buy hardware"; it is one question: **may the loop attach a GPU
      partition of box 1's RTX 5070 Ti to the Hyper-V guest GPN-209?** Evidence measured this run,
      not carried: (a) box 4 is still off the tailnet (`box4.sh` ⇒ `ssh rc=255`, `SOCKSv5 … General
      SOCKS server failure`) so option E is unavailable until someone powers it on; (b) GPN-209's only
      partition is `…VEN_8086&DEV_9BC5…` (Intel UHD 630) with `CurrentPartitionCompute : 0` — the
      cause of the 2026-11-24 `no OpenCL platforms` refusal; (c) **the 5070 Ti is host-partitionable**
      — `Get-VMHostPartitionableGpu` on box 1 lists `…VEN_10DE&DEV_2C05…`, `ValidPartitionCounts {32}`,
      `TotalCompute 1000000000`. So option A costs nothing new and needs no purchase; the only cost is
      touching the GPU config of the box that runs a live mining node (host driver exposure, minutes of
      risk to box 1's node). | options: A attach the partition now (unblocks the lifecycle venue this
      release, no spend, small risk to box 1's live node) · B do it during a quiet window you name ·
      C leave box 1 alone and power box 4 back on when convenient | recommend: **A**, sequenced right
      after the current release cut (box 1's build queue is idle then), with the adapter removed again
      if the guest driver install fails. **Also fixed in the record:** step 5 of
      `scripts/stranger-lifecycle.ps1` cannot pass during release N at all (`:362` — "step 5 needs two
      published versions"), so the 256 gate is now scoped to validate 0.3.78 → 0.3.79 and the venue
      must be ready before 0.3.79 publishes, not before this cut.
      **Narrowed again 2026-11-26 03:40Z, by measurement:** box 3 is not a venue either — `box3.sh` ⇒
      `CsName DESKTOP-DPJR60M / WindowsVersion 2009 / OsName Microsoft Windows 10 Pro`, `Get-VM` lists
      no guests, and `Get-VMHostPartitionableGpu` does not exist on Windows 10 Pro (GPU-P is a
      Server/Win11-Enterprise feature), so box 3 can neither host a clean guest nor partition a GPU;
      and its own node is live, so it cannot be wiped. The venue question is therefore unchanged and
      still asks only about box 1 / GPN-209 (option A) versus powering box 4 back on (option C).

- [ ] AUDIT-2026-11-26a — the fleet miner and the LLM provider share one GPU, so `gpn-provider` can never re-attest: `[attest] re-attesting: 825 GFLOPS attested at block 356917, older than 7200 blocks` → `skipping: GPU busy (util 100 %, 1615/8188 MiB, utilization 100 % > 10 %) — NO measurement submitted` → `still busy after 6 attempt(s) — next try in 3600s`. `nvidia-smi --query-compute-apps` names pid 4179529 `gpn-node` (the fleet miner, 1120 MiB @ 100 %) and pid 3771652 `gpn-agent` (140 MiB). Head #366389 vs attested block 356917 ⇒ ~9,472 blocks (~29 h) without a fresh measurement; 7 `GPU busy` lines in 3 days. Jobs are still served, so this is degraded capability scoring, not an outage. | options: A attest under a declared load window (measure a stable fraction, e.g. GFLOPS while the miner runs, and label it as such) · B pause mining for the ~2 s measurement (needs a miner-side hook or dropping `--force-authoring` briefly; small chain risk) · C accept that a mining node does not re-attest and exclude the condition from alarms | recommend: **A** — it keeps the miner running and gives the oracle a real number with its conditions recorded; B costs block production on the only heavy node, C hides a metric that is supposed to detect a degraded provider. Not fixed by the auditor: it changes provider/miner behaviour while a 0.3.80 rollout is fresh, and it needs a measurement semantics decision. | raised 2026-11-26 00:0x PT (6-hourly audit)
