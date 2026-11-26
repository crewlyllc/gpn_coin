# Changelog

All notable changes to GPN are recorded here. The version in this file is the
one `gpn_node --version` prints, the one the signed release manifest publishes,
and the one the updater compares (`docs/updates.md`).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions are [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
the GPN-specific meaning of each level documented in `docs/updates.md`.

## [0.3.82] - 2026-11-26

Bumped from 0.3.81 (`scripts/bump-version.py patch`). A **critical** cut under the batch policy
(owner, 2026-11-25): image and audio requests could be refused with "service unavailable" while a
working media provider was online, which is the "AI does not work" rule, so it does not wait for a
batch of five. Row 285 rides along.

### Fixed
- **Images and speech no longer refuse while a media provider is online** (row 286). The endpoint
  walk behind `POST /v1/images/generations` and `POST /v1/audio/speech` took the **first three
  providers in queue order** and probed those; a `pull://` box that published no `channel_url`
  cannot be dialled (nothing to probe, zero cost) yet consumed one of those three slots. Measured
  on 0.3.81: a prompt whose candidate list began with one failing probe and two such boxes answered
  503 `no diffusion provider serves the channel render endpoint`; two minutes later the same
  request returned three completed PNGs from the local diffusion provider on channel 7. The walk
  now visits every candidate, skips structural refusals for free and budgets **real probes**
  (`MAX_ENDPOINT_PROBES = 8`), so a reachable provider is reached whatever its rank.
- **`GET /jobs` and `GET /job/{id}` publish accounts as ss58** (row 285). The chain read renders
  `JobProvider`/`JobRequestor` as 32-byte hex by design (providers compare against
  `signer_address_hex()`), and the AP published that string beside the store's ss58 lease, so 214
  of 256 job rows carried an account a caller could not join to `GET /providers`. The HTTP
  boundary now canonicalises them; the in-agent hex form is untouched, and historical hex leases
  render ss58 with no data migration.

## [0.3.81] - 2026-11-26

Bumped from 0.3.80 (`scripts/bump-version.py patch`). Batch release (owner policy 2026-11-25: five
fixes, or one critical): rows 278, 279, 282, 283 and 284 are the five waiting fixes, so they ride
one version and one fleet restart instead of five.

### Changed
- **The update hold is evaluated before the node is stopped** (row 278): the canary gate,
  `GPN_AUTO_UPDATE` policy and retry backoff moved out of `--apply` into one `release_hold()`
  function, and `--check` runs it too, so a node that is not the canary no longer pays a
  `taskkill /T /F` of the whole process tree plus a restart on every hourly check for a decision
  that needs no file unlocked. `--check` gains exit 5 (“a newer release exists but this node is
  held”); the generated `install.ps1` wrapper logs `holding … (node NOT restarted)` and exits 0
  before `Stop-NodeTree`.
- **`network.json` `spec_version` 54 → 55** (row 279), matching the runtime and the live chain, with
  a note in `network_registry.rs` that raising this floor is what declares older nodes incompatible;
  `scripts/check-spec-version.sh` now hard-fails on any registry/runtime divergence instead of
  passing 54-vs-55 clean.

### Fixed
- **A unit test can no longer append to the operator's real `~/.gpn/last-exit.json`** (row 282):
  `exit_report::default_path()` returns `$TMP/gpn-test-last-exit-<pid>.json` under `cfg!(test)`.
  The updater reads that file to classify a failed start, so a test run could previously be quoted
  as the reason a real update rolled back (32 such records were written on 2026-11-25/26).
- **Three shipped files no longer point at the deleted `~/.openclaw/workspace/projectmanager07`
  clone** (row 283): the cadence-gate unit template, `build-spec42-wasm.sh` and
  `gpn_gpulock_contend.py` now resolve `~/gpn-loop`, so reinstalling the shipped unit no longer
  restores one that fails `status=200/CHDIR` on every fire.
- **The soak recorder no longer deletes a `### Note` written under a day section** (row 284): a
  section boundary is now any `##`/`###` heading, and `splice_day` keeps notes above and below the
  days, so a same-day refresh cannot eat the explanations of that day's numbers (it had eaten two).

## [0.3.80] - 2026-11-25

Bumped from 0.3.79 (`scripts/bump-version.py patch`). Batch release (owner policy 2026-11-25: five
fixes, or one critical): rows 273-277 were the five waiting items, so they ride one version and one
fleet restart instead of five.

### Added
- **Per-node telemetry the audit can trend** (row 274): the audit now writes one small JSON per node
  per day (`~/.gpn/telemetry/<date>.json` — height, peers, block-interval p50/p95, restarts, job
  counts, update result, disk free), and with 7 consecutive files it prints a trend line and flags any
  metric moving more than 25% week over week. A slow drift is no longer invisible until it is an
  incident.
- **Three incident alarms** (row 276): the audit raises an alarm (Discord + `docs/NEEDS-TRIAGE.md`)
  when peers < 2 for 10 minutes, when no block arrives for more than 2 minutes, or when job success
  falls below 90% over an hour — each alarm carrying the measurement that triggered it, and each
  proven by injecting the condition once.
- **Canary rollout** (row 275): one node updates and proves itself (version `CONFIRMED` plus a served
  job) before the rest of the fleet moves; other nodes hold while the canary is in flight, and a failed
  canary leaves them on the old version.

### Changed
- **The seven-day soak gate is measured, not just defined** (row 273): `docs/STABILITY-GATE.md`
  specifies six tests that had never been executed with numbers beside them. A soak record
  (`docs/soak-<date>.md`) now carries a day-by-day line for this host and box 1 — uptime, peers,
  block-interval p50/p95, restarts, update outcomes, jobs attempted/succeeded, and any
  `Reason:`/ban line — plus a verdict per gate test, and names drift as drift instead of reporting it
  as healthy.

### Fixed
- **An aborted `release-cut` no longer leaves the repo locked** (row 277): `scripts/release-cut.sh`
  released `~/.gpn/release-in-flight` only when the holder line matched a pid format that
  `release-lock.sh take` does not write, so the safety net could never fire on the abort path — found
  2026-11-25 12:19 PT when a cut correctly refused to sign an unverified build and then left
  `holder: jeremyg@openclawassist pid 3872365` behind with no such process, refusing every subsequent
  `git commit`. The lock is now tracked by version/`LOCK_TAKEN` and released by version in `cleanup()`.

### Carried
- **Stranger lifecycle proof** (row 256): still `AWAITING-OWNER` for a venue — the open question is in
  `docs/NEEDS-TRIAGE.md` (attach a GPU partition of box 1's 5070 Ti to GPN-209, or power box 4 back on).

## [0.3.79] - 2026-11-25

Bumped from 0.3.78 (`scripts/bump-version.py patch`). Three defects found on 0.3.78 by running the
documented examples as a stranger would: a routing rule that could only fail, a refusal message that
said the opposite of the truth, and a warm-up window that told the first caller the service was down.

### Fixed
- **A 409 or unreachable candidate no longer fails the call** (row 270): the access point treated a
  loopback/private `channel_url` as dialable for a remote provider, POSTed to its own listener, got
  `409 {"error":"assignment names a different provider"}`, and ended the job `failed` /
  `not_charged` with "image generation is temporarily unavailable" — while a second identical
  attempt succeeded. A candidate that refuses now advances to the next candidate within the same
  attempt, and a loopback/private channel URL from a non-same-egress provider is not dialable.
- **The update guard's refusal names what it can know** (row 271): `gpn-agent/src/update.rs:1332`
  printed "Installing it would swap newer code for older code" for every same-version/different-sha
  case, including when the installed build was an ancestor of the release (measured during 0.3.78's
  self-update). The behaviour (refuse, require `--force`) is unchanged; the message no longer asserts
  a direction it cannot verify at runtime.
- **First jobs after a restart get a retryable answer, not a timeout** (row 272): the first two chat
  requests after an update restart died at the caller's 10 s budget while the local provider warmed,
  then the third answered in 0.193 s. The access point now answers `not ready` (distinct, retryable)
  inside the warm-up window instead of routing into it.

### Carried
- **Stranger lifecycle proof** (row 256): still `AWAITING-OWNER` for a venue — the open question is
  in `docs/NEEDS-TRIAGE.md` (attach a GPU partition of box 1's 5070 Ti to GPN-209, or power box 4
  back on). The gate validates 0.3.78 -> 0.3.79, so the venue must exist before this release publishes.

## [0.3.78] - 2026-11-25

Bumped from 0.3.77 (`scripts/bump-version.py patch`). One defect and one carried proof: an image or
video job cannot reach a provider that only pulls work, and the stranger-lifecycle proof still has no
clean Windows guest to run on.

### Fixed
- **Image/video jobs can reach a pull-only provider** (row 263): the access point resolved a render
  channel from the provider's published `status_url` alone, so a box that advertises
  `pull://<provider>` — which is every NATed box whose heartbeat is push-only — was classified
  `ChannelEndpointFailure::PullOnly` and an image request answered `gpn_unavailable: no diffusion
  provider serves the channel render endpoint`. LLM chat worked over the job transport the whole
  time; only the render lane had no pull path, and the diffusion provider's journal has never
  carried a `[channel] job` line. The AP now routes a render job to a pull-only provider over the
  job transport, so no inbound port is needed. Live proof: `POST /v1/images/generations` returns an
  image with 200 and one `[channel] job` (or job-transport) line names the provider.

### Carried
- **Stranger lifecycle proof** (row 256): still `AWAITING-OWNER` for a venue. Steps 1-2 pass on
  GPN-209; step 3 fails there on a guest with no GPU partition, box 3 is a live node and cannot be
  wiped, box 4 is off the tailnet. Option E (wipe box 4, keep its keystore) is the loop's
  recommendation and is in `docs/NEEDS-TRIAGE.md`.

## [0.3.77] - 2026-11-24

Bumped from 0.3.76 (`scripts/bump-version.py patch`). One defect, found by the second live run of the
box-3 deep-fork test on the published 0.3.76 build: the refusal survived the peer's work-less claim,
then stopped pausing sealing 56 s later when the same peer announced a chain merely *ahead* by height.

### Fixed
- **A `DeepFork` refusal now also survives a peer that is only ahead by height** (row 262, second
  measurement): `fork_choice::next_pending` held the refusal across `Keep(WorkUnavailable)` in 0.3.76,
  and box 3 showed that hold working — the pause was inserted and outlived the work-less claim. It
  then died on `22:15:10.770 WARN … chain-state: peer 12D3KooWNCT… is ahead at #769 (ours #692, same
  genesis) -- syncing from it`, i.e. `Keep(PeerAhead)`, which the hold treated as ordinary forward
  sync and released: `gpn-forked` was removed and sealing resumed underneath the heavier branch
  (ours mined #692 → #779 while that branch grew past #854). A peer being ahead by height is normal
  sync in general, but for a peer just refused as `DeepFork` it is the same heavier branch, taller.
  The hold now persists across `PeerAhead` and releases only when the announced tip is a block we
  hold — i.e. when that chain has actually landed here. Test
  `a_deep_fork_refusal_outlives_a_work_less_claim` asserts the ahead case and failed on the pre-fix
  function (`left: None, right: Some("deep-fork")`); `fork_choice` 15 passed, full bin suite 305
  passed, `cargo fmt --check` and clippy `-D warnings` clean. Live proof on a published binary is
  this release's §10.

## [0.3.76] - 2026-11-24

Bumped from 0.3.75 (`scripts/bump-version.py patch`). One defect, found on the last step of the box-3
fork test (row 194): a deep-fork refusal stopped pausing sealing about 23 s after the refusal.

### Fixed
- **A `DeepFork` refusal is held until the peer stops announcing a taller chain** (row 262): the
  refusal inserted `pending[peer] = deep-fork` and `refresh_forked_marker` wrote `gpn-forked`, but the
  peer's next announce carries `work: None` (row 194's rule: a work figure measured on another tip is
  not evidence), so `fc::evaluate` returned `Keep(WorkUnavailable)` — whose `pauses_sealing()` is
  false — and the pending entry (and the marker file) were dropped while the peer was still 150+
  blocks taller. Measured on box 3 with the published 0.3.75 Windows build (17:26:15 PT:
  `reorg=refused:deep-fork depth=633`, zero imports, no adoption — correct — but `gpn-forked` lived
  only 17:26:15→~17:26:38 and was gone at the 17:33:17 verdict, which is what made the test report
  `PASS=False`). The pending refusal is now cleared only by evidence that the fork is over: a claim
  on our own chain, the peer's tip becoming a block we hold, or the link closing.

## [0.3.75] - 2026-11-24

Bumped from 0.3.74 (`scripts/bump-version.py patch`). One fork-choice defect, found while working the
box-3 fork test (row 194): a node could accept a reorg toward a tip it already held, so the reorg was
accepted and then never performed.

### Fixed
- **A work figure is only valid for the tip it was measured on** (row 194, commit `87d77b3b`):
  `fork_choice` measured `our_work` on a 30 s timer and then published it beside whatever best block
  the node happened to be on. A peer that was syncing therefore announced e.g. `best=#192` together
  with the work of the `#229` branch it had just left, and the verdict could come out
  `reorg=accepted depth=49` for a branch that was not actually heavier — after which the sync engine
  had nothing to request (measured on box 3, 2026-11-24: `fork-choice: work ours=1030736
theirs=2869598 … reorg=accepted depth=49` → `requesting heavier branch` → zero imports, best
  unchanged). `work_for_tip` now claims and compares a figure only while the tip still matches on both
  height and hash, and the work index is refreshed when the best block changes instead of every 30 s.
  Tests: `cargo test --release -p gpn-node --bin gpn-node -- fork_choice` ⇒ 14 passed, 0 failed,
  including `work_is_only_claimed_for_the_tip_it_was_measured_on`.

## [0.3.74] - 2026-11-24

Bumped from 0.3.73 (`scripts/bump-version.py patch`). Four defects found while closing 0.3.73 and
running the stranger lifecycle test: docs pages the generator never published, an installer that
reported a start that did not happen, a dependency tree with 38 known advisories, and a release
lock warning that lied during a dry run.

### Changed
- **Dependency bump on `gpn-node`/`gpn-agent` for the `cargo audit` findings** (row 260): the
  `Dependency audit` job reported `38 vulnerabilities found!` across 1040 crates, including four
  high-severity DNSSEC/DNS-validation flaws in `hickory-proto` 0.25.2 and 0.24.4, `zstd-safe`,
  `wasmtime`, `quinn-proto`, `lru`, `libz-sys`, `tokio`, `chacha20`, `url`, `chrono`, `rustix`,
  `ring`, `libsecp256k1` and `quinn-udp`. The bump is its own patch release: both suites and a
  two-node testnet run go with it, and every remaining advisory is either fixed or an explicitly
  justified, dated `[advisories] ignore` in `deny.toml`.

### Fixed
- **Three docs links now point at pages that exist** (row 257): `run-a-node` and
  `technical-overview` linked `updates`, and `node-gui-and-idle` and
  `provider-address-and-models` linked their own pages, but only the slugs in
  `scripts/build-site-docs.py`'s `PAGES` are generated — so `https://gpncoin.ai/docs/updates`,
  `/docs/node-gui-and-idle` and `/docs/provider-address-and-models` answered 404 while the source
  files sat in `docs/`. The pages are generated now, and a check fails when any internal docs href
  resolves to a page the generator does not emit.
- **`install.ps1` no longer says "node started" when the task did not start** (row 259): on a
  headless install the scheduled task's `Interactive` principal can never run, so
  `schtasks /Run` returns 0, `GPN-Node` stays `Ready` with `LastTaskResult 267011` (0x41303, "task
  has not yet run"), no `gpn-node` process and no `node.log` exist — and the installer still printed
  `node started (GPN-Node)` and exited 0. The start step now waits a bounded time for a live
  `gpn-node` process or the RPC on 9944, and on absence says so, names the principal/logon type as
  the reason and exits non-zero.
- **`release-cut.sh --dry-run` no longer claims the lock tool is missing** (row 261): the lock block
  is `if [ -x "$RELEASE_LOCK_TOOL" ] && [ "$DRY" = 0 ]`, so every dry run fell into the `else` and
  printed `WARNING: … is missing or not executable — the repo is NOT locked` about a file that is
  `100755` in git. The three cases — tool missing, dry run, lock taken — now print three true lines.

## [0.3.73] - 2026-11-23

Bumped from 0.3.72 (`scripts/bump-version.py patch`).

Two items found while stamping and closing 0.3.72: a docs generator that silently reverts an
SEO fix, and the missing end-to-end proof that a stranger can install, serve and update.

### Added
- **`scripts/stranger-lifecycle.ps1` — the stranger lifecycle proof** (row 256): on a clean Windows
  guest, using only the snippet from `https://gpncoin.ai/download` and no credential or repo
  access, it asserts in order that the downloaded size and sha256 match `releases.json`, that
  `gpn-node --version` matches the manifest, that the provider registers unattended within 10
  minutes with a named reason on failure, that one AI job is served end to end through the access
  point, that the node self-updates to the next published version with no human, and that it is
  still registered and serving afterwards. Any step that needs a human, a credential or a retry
  fails the release.

### Fixed
- **A failed GPU check now tells the operator what to do** (row 258): a clean Windows guest
  without a GPU used to end onboarding with `GFLOPS benchmark: open GPU matmul: no OpenCL
  platforms: DLOPEN_RUNTIME_LOAD_FAILED` — true, and useless. That line now carries a
  `what to do:` sentence chosen from the failure class: install/repair the vendor GPU driver (or
  give a virtual machine GPU passthrough), enable the card when OpenCL reports no device, or fix
  the device permissions, with `gpn_node --diagnose` named as the fallback. The original error text
  is kept verbatim, so nothing that was diagnosable is lost.
- **`scripts/build-site-docs.py` emits the clean URLs the site actually uses** (row 255): the
  generator wrote `<link rel="canonical" href="…/docs/<slug>.html">`, `og:url` with `.html`, and
  nav/footer hrefs ending `.html`, so running it reverted the 2026-11-24 SEO 301 fix across 14
  files. The HEAD/FOOT/NAV templates now emit `/docs/<slug>`, `/download`, `/ai`, `/chat` …, and
  regenerating the committed tree is a no-op except the intended content change.

## [0.3.72] - 2026-11-23

Bumped from 0.3.71 (`scripts/bump-version.py patch`).

A bundle of five owner-directed items (2026-11-23 12:36 PT): update cadence back to hourly,
repeatable free registration, airdrop weight from verified contribution, a verified/unverified
label on every GPU fingerprint, and logging of rejected heartbeats.

### Added
- **Airdrop weight from verified contribution** (row 252): the `gpn-airdrop-input-v1` export now
  carries per-account `blocks_authored`, `jobs_served` and `jobs_settled_gpn` read from chain, and
  the allocation formula in `docs/` excludes zero-contribution accounts — a registered-but-idle
  identity receives nothing. Identity creation is no longer the thing worth farming.
- **`GET /api/rejected` + dashboard list of rejected heartbeats** (row 254): the most recent
  unregistered identities that tried to `/status` are readable without touching a distant machine.

### Changed
- **Update check cadence back to hourly** (row 250): `DEFAULT_CHECK_HOURS` is again `1`, per the
  owner's 2026-11-12 decision; the 6 h value set 2026-11-18 under a TEMPORARY label is reverted.
  `check_interval_is_clamped` still enforces a 1 h minimum, and the startup line reports
  `first check in <1h`.
- **Free registration is repeatable** (row 251): `FreeEntryGpus` is kept as history but no longer
  gates entry. A fingerprint may (re)register at most once per day, so a machine that onboarded
  once and lost its provider record can re-enter; a never-seen fingerprint still pays its entry
  work; a second attempt inside 24 h is refused with a named reason.
- **Every GPU fingerprint is labelled `verified` or `unverified`** (row 253): the AP `/providers`,
  `/api/network`, dashboard and explorer never present the self-reported `gpu_info_hash` as fact —
  the tier is `informational` until `ChallengeVerifierAccount` confirms a challenge, and the docs
  state that an unverified fingerprint is a claim.

### Fixed
- **Rejected heartbeats log the peer id and reported version before being discarded** (row 254):
  the AP now records `account + peer_id + reported version + source IP` once per minute, so
  "which build is that machine running" is answerable from our side instead of inferred.

## [0.3.71] - 2026-11-23

Bumped from 0.3.70 (`scripts/bump-version.py patch`).

**The access point stops accepting an unbounded request body.** A red-team pass on the live
public surface found that every POST was buffered with `read_to_string` and **no size limit at
all**, and that the read happened *before* the auth gate — so the three routes that are
deliberately token-exempt (`POST /report`, `POST /result/{id}`, `POST /provider/heartbeat`)
were anonymously reachable at any size. Measured on the live AP: one 64 MB `/report` body was
accepted (`{"ok":true,…}`) and the process grew from 122 MB to 379 MB RSS; concurrent
multi-gigabyte bodies would OOM-kill the directory, taking the routing table and every
in-flight job with it.

### Added
- **Per-route request-body caps** (row 248): 1 MiB for every route, 16 MiB for
  `POST /v1/audio/speech` only (it embeds the caller's reference clip as base64, itself capped
  at 8 MB decoded). Over-limit bodies answer **413 `gpn_payload_too_large`** — enforced on the
  bytes actually read, not just on the declared `Content-Length`, so a client that omits or
  understates the header is caught too.

### Changed
- The release loop's red-team row (16) is now an ongoing per-release sweep instead of a one-shot
  blocked flag: each pass files its findings as numbered rows and fixes them before the bump.
  Its old gate was stale — the cumulative-work fork choice it was waiting on a go-ahead for is
  already implemented and shipped (`gpn-node/node/src/fork_choice.rs`, 0.3.66–0.3.70).

### Fixed
- **Unauthenticated memory-exhaustion DoS on the access point** (row 248). Verified on a scratch
  AP (:18092) with the fixed binary: a 64 MB `/report` ⇒ **413** while RSS stayed flat at 36 MB;
  a normal `/report` still answers `ok:true`; a 2 MB `voice_ref` body still passes the cap (its
  own validation answers 403, not 413). Unit test
  `only_the_voice_clone_route_may_carry_megabytes` covers the route map.

## [0.3.70] - 2026-11-22

Bumped from 0.3.69 (`scripts/bump-version.py patch`).

**Finality told honestly, and entry onboarding fixed where it actually broke.** The chain has
no finality gadget — `chain_getFinalizedHead` answers with the genesis hash — so the API now
publishes the depth that *is* final in practice instead of a field that says nothing. And the
fifth machine's onboarding failure is traced to its real cause: the entry proof was rejected
at a difficulty boundary and reported to the operator as a bogus signature error.

### Added
- **Settlement depth is published; `finalized` no longer implies a gadget** (row 240).
  `network.json`, `/api/network` and a new `GET /api/finality` expose `settlement_depth: 288`
  (≈72 min at 15 s blocks) and `finalized_head: null` instead of the genesis hash. The docs
  warn that `chain_getFinalizedHead` returns genesis, a test asserts the warning, and one
  curl example shows how to check a block is 288 deep (design:
  `docs/design-settlement-finality.md`).
- **The GRANDPA revisit condition is written down** (row 241), so finality is adopted on
  evidence rather than mood: ≥7 independent operators with keys we do not hold across ≥4
  admin domains, a published validator-set change procedure, survival of any single
  operator's loss, and a finality-stall alarm. Referenced from the settlement design, with
  how each condition is measured and a proposal block whose blank line means REFUSE
  (`docs/finality-revisit-condition.md`; status 2026-11-22: 0 of 4 met, asserted by
  `tests/settlement_docs.rs`).

### Changed
- **Entry onboarding re-checks difficulty immediately before submit and re-mines instead of
  failing once** (row 244). On a proof rejection the client re-reads the live difficulty,
  re-mines up to twice, and reports `re-mined against the newer target` with both targets
  and both nonces — closing the 45-block ADJUST race between the client's last check and the
  pool's precheck.
- **A rejected entry proof is no longer reported as a bad signature** (row 243).
  `validate_unsigned` mapped every `precheck_pow` failure to `InvalidTransaction::BadProof`,
  which the node renders as `{"code":1010,"data":"Transaction has a bad signature"}` — the
  exact string that sent two investigations the wrong way. It now maps to
  `InvalidTransaction::Custom(n)` with a defined meaning (`entry proof rejected: below
  target` / `gpu already used` / `limits`).
- **The unsigned entry path is fixed so a zero-balance machine can onboard** (row 242). The
  fifth box mined a valid proof (195M hashes at 3.7 MH/s) and the submission died with
  `entry-submit-rejected: {"code":1010,"data":"Transaction has a bad signature"}`; the
  unsigned form encodes as version byte 4 against 0x84 signed, so the fault was in how the
  call is built. Done when a fresh 0-balance account registers through
  `register_provider_entry_unsigned` on the live chain with the submitted hex and inclusion
  block pasted.

### Fixed
- **The release cut no longer leaves a window where the asset 404s or the manifest describes
  the previous build** (row 245). The cut uploads under a temporary name, verifies it, then
  renames/replaces in one step; `releases.json` is written only after the asset is anonymously
  readable with the matching sha, and the cut asserts `anonymous GET == 206` and
  `manifest sha == asset sha` before any surface advertises the version.
- **The stranger-verify check is credential-free and filename-exact** (row 246). §8 ticked
  green against a URL that was returning 404 to anonymous callers. The check now runs with no
  token and no cache (`curl -sL -r 0-1024 -o /dev/null -w '%{http_code}'` must be 206),
  compares the manifest filename against the release's actual asset names, and fails the
  release if either is wrong — with the command and output pasted in the checklist.
- **The one-command Windows install detects a failed download instead of silently
  reinstalling the old build** (row 247). The snippet now uses `curl.exe -fL`, aborts on a
  non-zero exit or a file smaller than the manifest's size, verifies the zip's sha256 against
  `releases.json` before extracting, and `install.ps1` refuses to install when its own version
  is older than the manifest's latest. This is the reproduction of a fresh install reporting
  0.3.64 while the manifest served 0.3.68.

## [0.3.69] - 2026-11-22

Bumped from 0.3.68 (`scripts/bump-version.py patch`).

### Fixed
- **`/providers` no longer counts an unreachable endpoint as a provider** (row 239, the
  residual of 147). The access point kept an identity-less placeholder row for every
  configured/on-chain `/status` URL that did not answer, even when the machine behind that
  URL was already in the registry under its real account — so `GET :8092/providers`
  returned **7 rows for 5 providers** (measured 2026-11-22 10:25 PT: `5ENqSp5Q…`, `5Fh75o8N…`,
  `5FCwsMfV…`, `5HVtpozG…`, `5HgbpiTY…` plus `address: ""` rows for
  `http://gpn-testnet.duckdns.org:8093/status` and `http://23.241.109.255:8093/status`). The
  placeholder could never be matched to its own machine because the live row's `status_url`
  is `http://127.0.0.1:8093` (this host) or `pull://<account>` (box 1, no `multiaddrs`), so
  `known_hosts()` never met the URL host. Unreachable endpoints are now reported top-level as
  `unreachable_endpoints: [{url, last_error}]` instead of masquerading as provider rows.

## [0.3.68] - 2026-11-22

Bumped from 0.3.67 (`scripts/bump-version.py patch`).

**The two gates that had silently stopped Windows boxes from ever updating.
Box 1 could not update for two independent reasons: it read a leftover
file-replacement queue as "the machine is shutting down", and its updater ran
at a lower integrity level than the node it has to stop. Both are fixed in the
installer and both now fail loudly instead of looking like a normal deferral.**

### Fixed
- **A stale `PendingFileRenameOperations` queue no longer defers the update
  cycle (row 237).** `Get-ShutdownState` treated PFRO, CBS `RebootPending` and
  WU `RebootRequired` as "a shutdown is in progress". They only mean "a reboot
  is owed", and other installers leave them behind for days (118 dead entries on
  box 1, naming `C:\Config.Msi\*.rbf`, Brave and Dropbox temp files). The
  updater's own swap uses `MOVEFILE_DELAY_UNTIL_REBOOT`, so a successful update
  created the marker that blocked the next one. Now only `SM_SHUTTINGDOWN`
  defers; the reboot markers are logged as notes. `test-update-shutdown.ps1`
  asserts PFRO-only and CBS+WU-only are not deferred (27 checks).
- **The `GPN-Update` task is verified against the node's integrity level
  (row 238).** Box 1's task was registered at `RunLevel=Limited` on 2026-11-01
  while `GPN-Node` runs at `Highest`, so the updater could not stop the node
  (`Access is denied - it may be elevated`) and refused to apply, leaving the
  box on 0.3.66. Registration success is not proof: `Assert-UpdateTaskElevation`
  reads the *live* tasks, replaces the update task once with the installer XML
  if the level is lower, re-asks, and warns plainly if it is still lower. Both
  installer paths (fresh install and `--update-task-only` deploy) call it.
  `test-update-task-elevation.ps1` covers Highest, Limited-then-fixed,
  Limited-twice and unreadable (18 checks).

## [0.3.67] - 2026-11-21

Bumped from 0.3.66 (`scripts/bump-version.py patch`).

**An update no longer kills its own replacement, and a start the operating
system refused is no longer treated as a bad build.** A fifth machine updated
0.3.65 → 0.3.66, then rolled back to 0.3.64, then failed again; the only
evidence was `0xC000026B` and `CTRL_SHUTDOWN_EVENT` on a machine that was not
shutting down. Both signals were ours: the update's own console teardown, seen
through a loader code the confirm gate did not know, escalated into a rollback
loop.

### Added
- **A teardown-class start failure is classified, not rolled back (row 235).**
  `0xC000026B` (STATUS_DLL_INIT_FAILED_LOGOFF), `0xC0000142` (DLL init failed),
  `0xC0000135` (DLL not found), `0xC000013A` (CTRL+C exit) and `0xC0000005`
  (access violation at load) are loader refusals: the environment would not let
  the process start, which says nothing about the build. These are named in the
  confirm message, retried once, and then reported as `deferred: environment
  did not allow the process to start (0x…)` — never rolled back.

### Changed
- **The node is started from a context that survives the stop (row 233).** The
  replacement is created only after the old tree is fully gone (`taskkill …
  SUCCESS`) and detached from the console being torn down, so it cannot be born
  into a closing window station. `update.log` records the start strictly after
  the teardown.
- **A console teardown is no longer reported as a machine shutdown (row 234).**
  `CTRL_SHUTDOWN_EVENT` fires for our own stop as well; the distinction is now
  logged, recorded in `last-exit.json`, and the confirm message names the actual
  cause instead of `no process ever appeared`.
- **An update does not begin while the system is stopping (row 236).** Before
  the node is stopped, the wrapper checks for a pending shutdown/restart
  (`GetSystemMetrics(SM_SHUTTINGDOWN)`, `PendingFileRenameOperations`) and logs
  `deferred: system is shutting down`; the next cycle applies normally.

## [0.3.66] - 2026-11-21

Bumped from 0.3.65 (`scripts/bump-version.py patch`).

**Registering a provider becomes free on every path, and the product stops
telling people it costs money.** A failed update on a fresh machine now reports
why the node died instead of only that it did.

### Added
- **A confirm failure that names its cause (row 230).** When the child node exits
  before the first health poll, the failure carries the child's exit code, the
  first stderr line and the last line of the node log — so a missing ollama reads
  "ollama not found (exit 69)" instead of "no process ever appeared". The same
  evidence is written into `last-exit.json`.

### Changed
- **`register_provider` (call_index 0) no longer charges a fee (row 231).** The
  weight annotation carries `Pays::No`, matching
  `register_provider_entry`/`_unsigned` and `accept_job`; a zero-balance account
  can register through it, and a pallet test asserts
  `get_dispatch_info().pays_fee == Pays::No`. The runtime change takes effect at
  the next spec via set_code.
- **"Paid registration" language purged (row 232).** `onboard.rs`,
  `gpn_register.rs` and the website's buy/deploy pages no longer describe a
  registration cost that does not exist; a spent free entry now points at the
  unsigned re-registration path, and the site says plainly that joining costs
  nothing.

### Fixed
- **Update failures are diagnosable (row 230).** See Added.

## [0.3.65] - 2026-11-20

Bumped from 0.3.64 (`scripts/bump-version.py patch`).

**Two long-standing gaps close in this release: a node that accepts a reorg but
never downloads the branch, and a retarget constant set that did not match the
shipped artifact.** Alongside them, the access point stops being four
hand-written handlers and becomes one dispatcher over a task vocabulary, and
referral codes stop being typed strings and become wallet-derived, operator-only
attribution that can be exported as airdrop input.

### Added
- **Task vocabulary + `POST /v1/jobs` dispatcher (row 221).** `tasks.rs` names
  each task with its accept/produce modalities and capability; a resolver maps a
  request to `(task, inputs, model?)` with the documented fallback (no task and
  no model ⇒ chat); the four OpenAI-shaped endpoints become thin adapters over
  that one dispatcher, so `unknown task` is a 400 and `no provider for task=speak`
  is a 503 instead of being routed to an LLM. `website/ai.html` documents the
  word table with one runnable curl per live word, generated from the provider
  matrix.
- **Providers advertise a task matrix (row 222).** Registration carries
  `{tasks, models: {task: [model…]}, limits: {task: {…}}}` instead of a flat
  capability list; the AP refuses to route `caption` to a box advertising no
  vision model. Old-style registrations still parse (compat).
- **Wallet-derived referral codes (row 223).** `GPN` + base32(blake2b of
  `gpn-referral-v1` ‖ account id) with a checksum character: the same wallet
  reproduces its code on reinstall, a checksum typo is rejected naming the
  expected character, and a code that does not derive from the presenting
  node's address is recorded `invalid_for_address` rather than dropped. Legacy
  typed codes keep working, labelled `legacy`.
- **The operator can find and copy their own code (row 224).** Dashboard card
  with a working Copy button, a one-line startup summary, `gpn_node --referral`,
  `GET /api/referral`, and an explanation page on gpncoin.ai.
- **Operator-only referral visibility (row 226).** `GET /api/referrals?code=&sig=`
  returns `{code, referred, first_seen, recent[]}` only when the signature
  verifies against the address the code derives from (401 otherwise), contains
  no referee addresses, and no route lists third-party attributions. The full
  registry is `gpn-referrals --all` on the AP host.
- **Deterministic referral export (row 227).** `gpn-referrals --export` is
  schema-versioned, carries the registry sha256 and per-participant
  `first_seen`/`last_seen`/`referred_by`/`derivation_verified`, dedupes by
  address (five reinstalls appear once), is byte-identical on re-run, and keeps
  self-referrals and `invalid_for_address` entries as evidence outside any
  payout total.

### Changed
- **Referral share link is `/join?ref=<code>` (row 225),** with `/r/<code>`
  redirecting to it; `/join` is a landing page that bakes the code into all four
  install commands and shows it as plain text, so a screenshot keeps the
  attribution. No surface asks anyone to share a `/download` URL.
- **Retarget constants vs the shipped artifact resolved (row 229).** The 0.3.62
  binary's `ADJUST … blocks=10 avg_s=4 target=4` is traced to its commit; the
  source-vs-binary discrepancy is explained and any reasoning that relied on the
  source constants is corrected.

### Fixed
- **Reorg band 33–288 blocks (row 228).** Fork-choice said `reorg=accepted` but
  never downloaded the branch: an accepted fork at depth 40–250 is now imported
  and reorged, and the two gates (`may_ask_for_blocks`, anchored
  `anchor_mismatch`) are isolated experimentally so the log says which one
  refused.

## [0.3.64] - 2026-11-20

Bumped from 0.3.63 (`scripts/bump-version.py patch`).

**A node can now say why it stopped sealing.** 0.3.63 could only detect that a
block gap was unexplained; this release makes the sealer's silence audible, so
the next 153-second stop arrives with its own cause attached instead of a hole
in the log.

### Added
- **Sealer heartbeat (`sealer: still searching …`).** While one proposal
  template stays unsolved, the node logs one INFO line every 20 s naming the
  template fingerprint, elapsed seconds and nonce windows searched, and the
  fact that the GPU is healthy and `submit` has not been attempted yet. A
  2.5-minute stop is now visible as eight consecutive lines, not as nothing.
  (#218)

### Changed
- **A slow solve and a blocked submit are now log lines, not gaps.** A solve
  that takes 45 s or more logs `sealer: slow solve — template 0x… took N s` at
  WARN, and a `worker.submit` that blocks for more than 2 s logs the pre-hash,
  nonce and block time at WARN. Together with the heartbeat these are exactly
  the causes `scripts/check-block-cadence.py` looks for when it attributes a
  gap above 3× the window mean, so a gap can now pass by being explained rather
  than by being short. (#218, #217)

No change to the cadence gate itself: `scripts/check-block-cadence.py` and its
45 s clause are exactly as shipped in 0.3.63. What changes is what the node
prints for that gate to read.


## [0.3.63] - 2026-11-20

Bumped from 0.3.62 (`scripts/bump-version.py patch`).

**This version contains no runtime change.** It ships the measurement and the
acceptance wording for #217, and the caller that keeps measuring. No `.rs` file
differs from 0.3.62: the retarget tuning that #217 asks for is *not* in these
binaries and rides the next code release together with #218 (the sealer
heartbeat), because in the live sample the two are not separable — 2026-11-20
11:12 PT, blocks 335117→335216, mean 15.38 s but **median 9.08 s** with four
gaps over 45 s (50.8, 153.0, 66.0, 88.7 s): stops plus bursts, not an
exponential tail.

### Added
- **A block-cadence gate you can run yourself (#217).** `scripts/check-block-cadence.py`
  reads 100 live blocks from the node's RPC, takes each block's own chain time,
  and fails when a gap exceeds 10× the window mean or when a gap over 3× the mean
  has no explanation in this node's log. Installed on this box as the user timer
  `gpn-cadence-gate.timer` (every 30 min), which writes its verdict to
  `~/.gpn/cadence-gate.state`. It currently **fails** on the live testnet — that
  is the honest reading of #217/#218, not a regression introduced here.

### Changed
- **The cadence target is stated in observables (#217).** "No gap over 45 s and
  fewer than 5 % under 3 s" was unsatisfiable at any difficulty: with 3–6 miners
  the gaps are an arrival process, and at a 16.0 s mean an exponential predicts
  5.95 gaps over 45 s and 16.9 under 3 s per 99 blocks — the 0.3.62 sample (6 and
  17) *was* the process. The target is now "no gap above 10× the window mean, and
  every gap above 3× must have a cause in the node's output", which an operator
  can check against their own log.

### Note
- Difficulty, sealing and every consensus rule are byte-identical to 0.3.62.
  If you are upgrading for cadence, wait for the release that carries #217's
  retarget change; there is nothing here to measure that 0.3.62 does not already do.

## [0.3.62] - 2026-11-19

Bumped from 0.3.61 (`scripts/bump-version.py patch`).

The bundle for this version is #52, #191, #196/#205, #213, #214, #215 and #216.
Each entry below is what the change does for a user of a released node; every one
of them is checked against the built artifacts before the manifest is signed
(§6 of `docs/releases/archive/0.3.62.md`), so nothing here is published unverified.

### Added
- **The access point refuses a job no provider can serve (#191).** When the pinned
  `model` is not advertised by any candidate, the lease is refused immediately with
  the reason, instead of routing the job to a box that cannot run it and making the
  caller spend a lease, an escrow and a full round trip to learn that.
- **Accepting work is fee-free (#196/#205).** `accept_job` returns
  `DispatchResultWithPostInfo` with `Pays::No.into()` — the requester pays, the
  provider does not — so an account with a zero balance can accept a lease and earn.
  The access point's `unfunded` routing gate is removed with it, since no registered
  provider can be unable to afford accepting.

### Changed
- **The difficulty retarget no longer ratchets (#52, mechanism only).** The
  retarget is now log-symmetric and clamped to ±33 % per 45-block window, so a
  short window of fast blocks can no longer collapse the target −50 % in one step
  and then claw back only +37 % per window. Measured on the live testnet
  (blocks 334734→334833, 2026-11-20): the target moved one full clamp step
  (+34.4 %) and then held, mean inter-block gap 16.0 s against a 15 s target.
  The user-visible complaint is **not** fully fixed: 6 of 99 gaps in that sample
  were still over 45 s (max 152 s) and 17 were under 3 s, because the step is
  still coarse for a 3–6 miner field. That residual is tracked as #217 in
  `GPN-LIST.md`.

### Fixed
- **A second install on the same box no longer takes over the first install's
  firewall rule (#214).** An inbound rule whose program lies outside the current
  install directory is left untouched and logged (exit 3), while a leftover naming
  a deleted install is still adopted.
- **Clippy is clean on the update tests (#213).** The seven "unnecessary use of
  clone to create a slice from a reference" lints in `gpn-agent/src/update.rs` are
  gone, so the release lane's `-D warnings` gate passes.

### Release engineering
- **A release cannot be published unattested (#215/#216).** The artifact is signed
  before the publish step and a missing or unverifiable signature blocks the
  release: a fail-closed `Provenance gate` step throws unless a signature bundle
  exists for the exact sha256 being uploaded and verifies against the workflow
  identity. For this user-owned private repository GitHub will not persist a
  native attestation, so the mechanism is keyless cosign
  (`cosign verify-blob --bundle` against `…/release-windows.yml@…` and the Actions
  OIDC issuer, bundle published as `gpn-windows.zip.bundle`); the GitHub-native
  `attest-build-provenance` + `gh attestation verify` path is what the same step
  does when the repository becomes public. §6/§7 of the checklist require the
  verifier to be run by hand against the digest published in `releases.json`.

## [0.3.61] - 2026-11-19

Bumped from 0.3.60 (`scripts/bump-version.py patch`).

The bundle for this version is #202, #208, #209, #210, #211, #212 and #194. Each
entry below is what the change does for a user of a released node; every one of
them is checked against the built artifacts before the manifest is signed
(§6 of `docs/releases/archive/0.3.61.md`), so nothing here is published unverified.

### Added
- **Audio generation from a voice reference: text in, speech out (#202).** A new
  `AudioGeneration` capability sits beside `LLMInference`/`Image`/`Video`/
  `FineTuning` so the access point can route it, served by a provider child that
  calls a local TTS engine and advertises the voices it can run. Requested the
  same way as the other modalities — `POST /v1/audio/speech` — with the voice
  reference sent with the job (5–15 s clip, MP3 output, billed on output
  seconds). The reference is fetched per job by the provider that runs it and
  discarded afterwards: the access point stores no voice samples, and a request
  must carry an explicit "I have permission to use this voice" acknowledgement.
- **A Windows install opens its own firewall holes (#208).** Two inbound rules
  are added for the **Private** profile only — the node program (via the stable
  `…\update\current\gpn-node.exe` junction) and TCP 30333 — so a fresh install no
  longer pops a security prompt whose default answer leaves the node peering
  with nobody. RPC 9944 and the dashboard 8099 stay loopback. An update re-adds
  either rule if it is missing and logs one line saying which.
- **An operator can read their own balance without asking anyone (#211).** The
  startup summary prints `balance: N GPN` (one RPC read, a dash if unreadable),
  the dashboard shows a total next to "Earnings Today", and `gpn_wallet balance`
  with no endpoint argument defaults to the local node and keystore. From
  another machine, `GET /account/<ss58>` on the access point answers the same
  question — read-only, public (a balance is chain data), and an unreadable
  chain is a 502, never a plausible `0`.

### Changed
- **Auto-idle no longer mistakes GPN's own mining for someone else's work
  (#210).** `foreign_vram_mb = vram_used − gpn_vram_mb` counted an Ethash DAG as
  foreign and unattributable utilisation defaulted to `foreign`, so a box running
  nothing but GPN stepped aside from itself. Load is now attributed by VRAM
  owner and by the whole `gpn*` process family; unattributable utilisation is
  `unknown`, never `foreign`; and when the only compute is our own the log says
  `own workload` once instead of engaging.
- **"Earnings Today" now names its source and window, and is a chain figure
  (#212).** The panel shows the balance growth the chain itself records since
  00:00 UTC (block rewards plus settled job payments), says so on the card, and
  keeps its day baseline on disk so a restart neither resets nor double-counts
  the day. If the account spent money in the window, or the balance fell, the
  card shows a dash with the reason instead of a number that would be wrong.
- **Windows binaries are signed, or the installer asks to add a Defender
  exclusion (#209).** An unsigned new `gpn-node.exe` can be quarantined at first
  launch, which reads as "the update stopped the node and it did not come back".
  The release build signs the Windows binaries when the certificate is
  configured; otherwise the installer offers the two exclusions with the user's
  consent and a clear log line. The updater's confirm step detects the
  blocked-by-security-software case and names it, instead of timing out with a
  generic message.

### Verified
- **Box 3 fork test (#194).** A deliberate shallow fork must reorg back onto the
  canonical chain; a fork deeper than the 288-block window must refuse with
  `reorg=refused:deep-fork` and pause sealing (`gpn-forked`); box 3 is restored
  to canonical afterwards. No user-facing behaviour change — this is the
  verification the fork-choice code shipped since 0.3.51 owes.

## [0.3.60] - 2026-11-18

Bumped from 0.3.59 (`scripts/bump-version.py patch`).

### Changed
- **An update stops the whole `gpn*` process family, verifies none survive, then
  starts (#206).** `start gpn-node failed (exit 1)` on a fifth computer had the
  box-3 shape: an orphaned child (`gpn_provider_status` and siblings are *not*
  named `gpn-agent`) survived the supervisor's stop, kept a handle on
  `node.log` and the chain DB lock, and the launcher could not open its log for
  append — exit 1, nothing written. Before starting after a swap the updater now
  enumerates every process whose name starts with `gpn`, stops them
  children-first, polls until zero remain, and only then starts; after start it
  confirms via RPC and writes one line naming how many processes it stopped and
  the version confirmed. A process it cannot stop (another account) is reported
  instead of silently blocking the start.
- **Fee-free `accept_job` in the pallet source (#205, source half only).**
  `accept_job` returns `DispatchResultWithPostInfo` and `Ok(Pays::No.into())`,
  keeping `ensure_signed` so identity is still proven. **This does not change
  any fee on the network yet:** the node still embeds the 481,002 B runtime
  from `be390db` (2026-10-06), so `Pays::No` reaches no chain until that runtime
  is rebuilt with a spec bump and enacted via `set_code`. The AP's `unfunded`
  routing gate therefore stays — see #205.

### Fixed
- **The update confirm step tells "slow" from "dead" and always leaves a node
  running (#207).** A fifth computer reported `auto-update: confirm the new
  version failed (exit 1) not confirmed within 360 s` — a correct rollback fired
  on a *slow* first start. The wait now polls the process and the RPC
  separately and logs which is missing (`confirm: process=up rpc=no 45s`), a
  first start after an update gets 900 s (warm restart keeps 120 s), and a
  genuine failure rolls back to the previous version, confirms the old version
  is running, and otherwise starts the old one rather than leaving the machine
  down. One remediation+retry runs first, classified from
  `last-exit.json`/`node.log`. A `started` attempt no longer blocks a retry: the
  attempt is consumed by the confirm step.
- **An unplaceable node image no longer reports liveness as unknown
  (#207 follow-up).** Only an image that cannot be placed at all counts as
  unknown; a version that is merely slow is a slow start.

## [0.3.59] - 2026-11-17

Bumped from 0.3.58 (`scripts/bump-version.py patch`).

### Changed
- **A release now locks the repo instead of asking politely (#204).** While a
  release checklist is open, no other commit may land: `release-cut` and the
  release driver take `~/.gpn/release-in-flight`, and a `pre-commit` hook
  refuses any commit while that file exists (documented escape: `--no-verify`,
  for emergencies only). Every extra push during a release cancels a queued CI
  run for the release in flight — 3 of the API's `cancelled` runs on 2026-11-16
  were the driver's own doc commits — so the rule is now mechanical. The step-0
  sequencing gate already written into `docs/UPDATE-CHECKLIST.md` is committed
  with it.
- **The update check is back to 6 h (#161).** `DEFAULT_CHECK_HOURS` returns to
  `6` (it was `1` for the 2026-11-12 review window, so a bad release was caught
  the same day), and `check_interval_is_clamped` asserts the real default again
  instead of hard-coding an hour. Operator override is unchanged:
  `GPN_UPDATE_CHECK_HOURS`. A temporary setting with no revert date is how a
  debugging shortcut becomes permanent behaviour.
- **Accepting a job no longer costs the provider a fee (#205).** `accept_job`
  now carries `Pays::No` **on its weight annotation** (the transaction pool
  decides `pays_fee` from the call's `DispatchInfo` *before* dispatch, so a
  `Pays::No` returned only from dispatch is evaluated too late) and returns
  `Ok(Pays::No.into())` from a `DispatchResultWithPostInfo`, keeping
  `ensure_signed` so identity is still proven. That is the half that stranded
  incident #6551: a registered provider with an empty account could not pay to
  accept, so the access point leased it a job the caller then waited a full
  deadline for.

### Not in this release (recorded, so the changelog is not a promise)
- **The access point's `unfunded` routing gate is deliberately still in place.**
  `start_job`, `submit_result`, `submit_result_v2`, `complete_job` and `fail_job`
  still pay a fee from the provider's account, so dropping the gate now would
  re-create #6551 one step later: a 0-GPN box would accept the free lease, then
  be rejected by the pool at `start_job` and the job would sit `Accepted` until
  the janitor killed it. Making the whole provider lane fee-free is the same
  runtime change and belongs in the same `set_code` — it is the next row, not a
  sentence in this one.
- **`Pays::No` is compile-time**: it only affects a live node after this batch's
  runtime is enacted with a spec bump and a governance `set_code`. Publishing the
  0.3.59 binaries is not the end of #205.

## [0.3.58] - 2026-11-17

### Added
- `/status` publishes the updater's retry state while a release is waiting:
  `update_declined` (the exact operator line, `update declined until <UTC time>
  (last: <reason>)`), plus `update_pending_version`, `update_attempts`,
  `update_last_attempt_at`, `update_retry_at` and `update_last_reason`. A box
  whose last update succeeded publishes none of it, so a healthy `/status` is
  byte-identical to before.
- The Windows startup summary (`startup-summary.txt`, printed by `start-gpn.cmd`)
  gains one line when the last update attempt failed: either
  `update declined until <UTC> (last: <reason>)` or
  `update retry due now for <version> (last: <reason>)`.

### Changed
- **The updater never gives up.** A failed attempt (rolled back, failed, or died
  mid-install) now records `next_retry_at` — 1 h after the first failure, 2 h
  after the second, then 4 h — and the next tick after that retries the same
  release, on the same boot, with no reboot and no `--force`. The old rule
  blocked a version for the rest of the boot, which is how one box declined
  0.3.51 and 0.3.52 and sat two releases behind (2026-11-16).
- Every attempt and its reason is recorded in `update-attempts.json` (`attempts`,
  `reason`, `next_retry_at`), so "why is this box still on the old version?" is
  answerable from the ledger, `/status` and the update log.
- The Windows update wrapper logs the updater's own decline line (with the time
  and the reason) instead of the old `policy, or already attempted on this boot`.

### Fixed
- A re-cut release (same version, different bytes) is still eligible
  immediately; a successful update clears the retry clock instead of leaving the
  old failure as the newest record.

## [0.3.57] - 2026-11-17

One subject: **a provider publishes the build it runs** (task row #201). Box 3 became an
unmanaged reference node on 2026-11-17 — owner removed it from Tailscale — so "did that box
update?" has to be answerable from network-visible data alone, the way a stranger's machine is
judged. `/providers` returned `version: null` for every box, so it was not answerable at all.

### Added
- **`version` and `git_sha` on the provider `/status`, the signed heartbeat, `/providers` and
  `/api/network`.** `version` is `CARGO_PKG_VERSION`; `git_sha` is the commit the binary was built
  from, or `null` when the build could not tell (unknown is published as unknown, never as the
  literal `unknown` dressed up as a commit).
- **Signed over the build (heartbeat v3).** A heartbeat carrying a build signs `canonical_message_v3`
  (v2 + `|version|sha`); a version an operator cannot trust is worse than no version, because it is
  exactly what an unmanaged box is judged by. A pre-#201 client sends no build and still verifies
  under v2/v1, so the access point accepts either shape — the two sides upgrade on independent
  timers.
- Self-reported build strings are bounded (`[A-Za-z0-9._+-]`, ≤64 chars) before they are stored or
  relayed, so a signed-but-hostile provider cannot publish prose in the directory.

### Changed
- `/api/network` provider rows gain `version` / `git_sha`; a chain-only row (registered, never seen
  by this AP) publishes `null` for both — it has never told us anything.

## [0.3.56] - 2026-11-17

Bumped from 0.3.55. One subject: **the Windows updater is allowed to stop the node it replaces** —
0.3.55 made the failure visible, this removes it (task rows #199/#200).

### Changed
- **`GPN-Update` is registered `HighestAvailable`, not `LeastPrivilege`.** `GPN-Node` has always run
  with `-RL HIGHEST`; a medium-integrity task cannot terminate a high-integrity process. Box 1,
  2026-11-17 07:11:39 — the first cycle running 0.3.55's fixed wrapper logged every taskkill line
  (`ERROR: The process with PID 44560 could not be terminated. Reason: Access is denied.`), then
  `REFUSING to apply: gpn processes survived the stop`, and the box could never update itself while
  the node was up. The one cycle that did succeed (05:45) only worked because an earlier crash had
  left the node stopped. The installer that writes this XML is already elevated — it registers
  `GPN-Node` with HIGHEST — so the request is grantable, and a standard account still falls back to
  the Startup-shortcut loop when registration is refused.

### Fixed
- **`could not stop  (pid )` now names the process.** The `Stop-Process` catch interpolated `$_`
  inside `catch`, where `$_` is the error record rather than the process, so the log line carried an
  empty name and pid for every survivor. The process object is captured before the `try`.
- **The old assertions that demanded `LeastPrivilege` are inverted, not deleted**, in
  `scripts/tests/test-autostart-installers.sh` — they encoded the bug ("update task never requests
  elevation"), which is why it survived two releases.

## [0.3.55] - 2026-11-17

Bumped from 0.3.54. One subject: **the Windows update can no longer leave the box with no node
running**, and its tree kill no longer crashes on a `taskkill` error line (task rows #199/#200).

### Fixed
- **A `taskkill` stderr line killed the update cycle before it applied anything (#199/#200).**
  `Stop-NodeTree` piped `taskkill … 2>&1` into `if ($_.Trim())`. On Windows PowerShell 5.1 a native
  command's stderr arrives as an `ErrorRecord`, which has no `Trim()`. The second image name
  (`gpn-node.exe`) is normally already gone — it was a child of `gpn_node.exe`, killed a moment
  earlier — so taskkill answers `ERROR: The process "gpn-node.exe" not found.` on stderr and the
  unguarded call threw inside the function. Box 1's own transcript (2026-11-17 05:06): the cycle
  logged the taskkill successes and then `TerminatingError(ForEach-Object): "Method invocation failed
  because [System.Management.Automation.ErrorRecord] does not contain a method named 'Trim'."`,
  so `--apply` never ran, the restart never ran, and the box was left **with no node at all** (found
  down at 04:57 and restarted by hand). Output is now cast with `[string]$_` before any string
  method; proven in the same interpreter: the old idiom `THREW`, the new one logs
  `F: ERROR: The process "no-such-process-zz.exe" not found.` and survives.
- **Any unexpected error now restarts the node.** The wrapper's header has always promised "a failed
  update still restarts: a box with nothing running is worse than a box on the previous build", but
  a terminating error skipped every restart (it ended the `try` body and fell straight to `finally`,
  which only closed the lock). A `catch` now logs `UNEXPECTED: <message>` with the position, starts
  `GPN-Node`, logs `GPN-Node started (recovery from an unexpected error)` and exits 1.

## [0.3.54] - 2026-11-17

Bumped from 0.3.53. One subject: **the Windows self-update's confirm window is longer than a
cold start**, so a build that is merely still starting is not rolled back (task rows #199/#200).

### Changed
- **Windows: the update confirms for 360 s, not 120 s, and watches the process (#199).** Box 1's
  first fully automatic 0.3.53 cycle did everything right — `stopping GPN-Node` at 02:00:02 with
  no 60 s forced wait, `downloading 0.3.53`, `installed 0.3.53`, `current -> versions/0.3.53` — and
  then declared `NOT CONFIRMED: not confirmed within 120 s: rpc unable to connect` at 02:02:14 and
  rolled `current` back, while `node.log` shows the new process binding `127.0.0.1:9944` at
  02:02:18: **125 s** of GPU init and state load. The gate failed correct work by five seconds and
  left the box inconsistent (running process 0.3.53, `current` 0.3.50). The default window is now
  360 s (`GPN_UPDATE_CONFIRM_SECS` overrides it), and the wait is no longer a bare RPC poll.

### Fixed
- **A node that is still starting is no longer a failed update; a dead one fails at once.** New
  `Test-NodeAlive` in the generated `update-and-restart.ps1`: while the process is up and has not
  yet answered, the loop logs a `still starting: <version> has not answered yet (N s of 360 s)`
  progress line every 30 s; if the process is gone it fails immediately with `the node process
  exited before it answered` instead of waiting out the whole window. Rollback and
  `previous`-retention are unchanged.

## [0.3.53] - 2026-11-16

Bumped from 0.3.52. One subject: **the Windows update can stop the node it is replacing, and
`--check` can no longer report a version the box is not running** (task rows #199/#200). 0.3.52
taught a node to update itself on Linux; this makes the same sentence true on Windows.

### Fixed
- **Windows: an update is no longer a no-op that rolls itself back (#199).** Box 1's own
  `update.log` on the 0.3.50→0.3.52 cycle: `stopping GPN-Node` → `the node did not exit within 60 s`
  → `apply: installed 0.3.52` → `NOT CONFIRMED: the node reports 0.3.50` → `rolling back`.
  `schtasks /End` ends only the task's own `wscript.exe`; the worker tree under it
  (`start-node.cmd` → `gpn_node.exe` → children) kept running and kept answering RPC, so the new
  build could never take over. The generated `update-and-restart.ps1` now stops the **tree**
  (`schtasks /End`, then `taskkill /T /F` on `gpn_node.exe`/`gpn-node.exe`, then per-survivor
  `Stop-Process`) and, if any `gpn*` process survives, **refuses to apply** — it logs
  `REFUSING to apply: gpn processes survived the stop…`, restarts the node on the current build and
  exits 1, so the install is untouched and the boot's single attempt is not burned.
- **`gpn_update --check` can no longer invent the version it is checking (#199).** It read the
  build stamp in `bin`, which a rollback cannot refresh (`Access is denied` on the running
  `gpn_update.exe`), so box 1 printed `current 0.3.52 compiled-in (no build stamp in the install
  dir) -> up to date` while the junction, the runner and RPC all said 0.3.50. It now locates the
  state dir next to the target dir (`<install>/data/update`, then `<install>/update`) and, when
  neither `current` nor a stamp exists, **refuses**: `cannot tell which version this box runs…`,
  exit 1.
- **A re-cut release re-arms a rolled-back attempt.** The attempt ledger recorded only
  `version` + a boot id, so one rolled-back attempt disabled auto-update until a reboot — even if
  the release had been re-cut in the meantime. `UpdateAttempt` now carries the manifest's
  `release_id` (asset sha256); the same bytes still block, a different release re-arms.
- **A second install directory cannot hijack another install's scheduled tasks (#200).**
  `install.ps1` reused an existing `GPN-Node`/`GPN-Update` task only when the run-as user matched,
  so installing into `C:\gpn-ci` while box 1's task said `Jeremy`/`Jeremy Guo` recreated both tasks
  and repointed them at `C:\gpn-ci`. `Register-NodeTask`/`Register-UpdateTask` now resolve the
  install dir from the existing task's own `Task To Run` and **refuse to touch a foreign task**
  (`belongs to the install at '<dir>' -- NOT touching it`).
- Clippy gate repair: `discover_state_dir` uses `Iterator::find` instead of a manual loop
  (`-D warnings` in the agent lane).

## [0.3.52] - 2026-11-16

Bumped from 0.3.51. One subject: **self-addresses stop being advertised, persisted or
dialled**, plus the rustfmt repair 0.3.51 needed (that release's CI verdict was cancelled by a
later push, leaving three agent files unformatted; `cargo fmt --all --check` is red at 0.3.51
and green here).

### Fixed
- **Loopback and link-local addresses are never announced, persisted or dialled (#195).**  litep2p reports a `0.0.0.0` bind as `/ip4/127.0.0.1/tcp/30333` and `/ip6/::1/tcp/30333`, and
  the heartbeat republished every entry of `system_localListenAddresses` verbatim. Peers
  learned those, kept them as a *dialable* peer in `gpn-peers.json`, and re-dialled them —
  which on the dialling box reaches its OWN listener: box 1 logged **2,833 self-dial lines in a
  4,000-line window** on 2026-11-15. Three changes, one rule:
  - `gpn_provider_status` publishes only addresses a remote peer can reach
    (`/status.listen_addrs`); the raw list is still used internally as *transport evidence*,
    so a `/ws` node keeps publishing `/ws`. Private RFC1918 addresses are kept — a LAN peer is
    a real peer.
  - `gpn-node`'s peer store rejects loopback/link-local/unspecified on **write and read**, so a
    file poisoned by an older build self-heals instead of needing to be deleted by hand.
  - Every bootnode source (static, persisted, DNS, URL, on-chain) is filtered before use, so no
    publisher can inject a self-address.
- **The autostart gate follows the launcher it guards.** `test-autostart-installers.sh` still
  required the pre-#198 `Start-Process -FilePath $Command` fallback, which #198 replaced with the
  hidden `wscript.exe` launcher — so `ci` was red on every commit after it (that run's verdict had
  been cancelled by a later push, which is why nobody saw it). The assertion now names the hidden
  launcher and keeps its intent: a second, non-CIM launch path exists **and it is windowless**.

### Verified
Live, same node RPC, two binaries: the deployed 0.3.51 heartbeat on `:18093` published
`['/ip6/::1/tcp/30333/ws', '/ip6/fe80::769e:…/tcp/30333/ws', '/ip4/127.0.0.1/tcp/30333/ws',
'/ip4/192.168.1.20/tcp/30333/ws']`; this build on `:18094` publishes
`['/ip4/192.168.1.20/tcp/30333/ws']` — loopback and link-local gone, the LAN peer and the
public multiaddr `142.129.54.88` unchanged. Node side: with a poisoned store and a loopback
`--bootnodes` entry, the deployed binary merged **11** bootnodes silently; this build logged
`peer-store: dropped 4 self-address(es) … never dialled (#195)` and `peer-discovery: dropped 1
self-address(es) … from the bootnode set` and merged 7. Tests: `discovery` 46 (3 new,
incl. the predicate's edges), `gpn_provider_status` 66 (1 new + 1 assertion inverted on
purpose), full `gpn-node` workspace and both clippy lanes clean.

## [0.3.51] - 2026-11-16

Bumped from 0.3.50. One subject: a LAN behind one public IP can be reached, and the
directory stops repeating what nodes say about themselves.

### Fixed
- **One verified backbone per public IP, advertised over IPv4.** Box 1 and box 3 share
  `172.117.107.91`; one holds the 30333 forward, yet both published **IPv6-only**
  multiaddrs, so a peer without IPv6 could not reach that LAN at all. The access point
  now attaches its **own dial-back verdict** per row and decides publication from it:
  `wrong-peer` and `unreachable` disqualify a claim outright, the winner publishes
  `/ip4/…/tcp/30333/ws/p2p/<id>` with IPv6 as an *additional* path, and every loser
  becomes a leaf with `multiaddrs: []`. An IPv6-only node is marked `v6_only` instead of
  implying dialability.
  Proof of the old misclaim: `gpn_dialcheck /ip4/172.117.107.91/tcp/30333/ws/p2p/W9w2nz…`
  answered as `12D3KooWP1JAF…` — a `wrong-peer` verdict.
- **Counters now equal the rows they describe.** `backbone_now`, `leaf_now`, `serving_ai`,
  `healthy_now`, `gateways`, `ap_seen_now` (and the new `v6_only_now`) are computed over
  the **deduped** non-chain rows, the same list that is rendered. The directory had
  reported `backbone_now: 5` beside four rows.
- **IPv4-first listen/public address selection** (`gpn_provider_status`): RFC 6724 ordering
  was choosing the IPv6 listen address; a public IPv4 path is now preferred, with a
  one-line warning when only IPv6 is available.

### Verified
`/api/network` 2026-11-16T23:12Z — `backbone_now 2` (two rows), `leaf_now 2`,
`gateways 2`, `v6_only_now 1`, and a dial-back-verified IPv4 backbone on
`172.117.107.91`. Tests: 695 lib, 97 scheduler, 65 provider-status.

## [0.3.50] - 2026-11-16

Bumped from 0.3.49. One theme: **fork choice stops being a height comparison.** On 2026-11-16
box 1 was found mining its own branch at `#306,633` while the fleet was at `#309,96x`, and nothing
could see it — the newest *signed* checkpoint (`#305,244`) sat below the fork, so every existing
guard looked at a chain that was internally consistent. Two branches at the same height are
indistinguishable to a fork choice that only counts blocks.

### Added
- **Heaviest-chain fork choice** (`node/src/fork_choice.rs`, owner decision 2026-11-16). A branch's
  weight is the sum of `2^256 / Difficulty` over its blocks, using the real per-block targets the
  runtime stores and retargets — height is never used as a proxy for work. A same-genesis peer
  announcing strictly greater cumulative work, at or below our height, is requested through the
  sync engine's fork path and sealing pauses (`gpn-forked`) until it lands. Equal work, or a peer
  whose work cannot be computed (a 0.3.49 peer's `gpncs/1` claim carries none), keeps the previous
  behaviour exactly, so this release inter-operates with 0.3.49 peers in both directions.
- **`gpncs/2`, an additive chain-state frame** carrying cumulative work. `gpncs/1` is still
  emitted, so an older peer keeps parsing our claim; the new frame is what makes a fork visible.
- **A bounded reorg with a named refusal.** `MAX_REORG_DEPTH = 288` blocks (6.4 retarget windows,
  ~72 min at the 15 s target): deeper reorgs are *refused* with `reorg=refused:deep-fork`, logged
  loudly, sealing stays paused, and the snapshot/catch-up path is what must do the work — an
  automatic deep reorg would silently erase hours of sealed, rewarded blocks.
- **Bootstrap stance as a greppable constant.** `PRIMARY_AUTHORITATIVE` (default `true`) says the
  Linux host is the primary chain while the network is a single-writer bootstrap: a branch that
  does not contain the primary's chain never displaces it, and when that rule is what decides, it
  is logged as a separate override line rather than becoming an invisible special case. Set it to
  `false` for pure heaviest-chain once there are independent operators.

### Changed
- `gpn-forked` pauses both the GPU and CPU sealers, exactly like the existing `gpn-isolated`
  zero-peer guard, so a node never seals a branch it already knows is losing.

### Fixed
- **A fork above the newest signed checkpoint is now detectable at all.** The anchor gate in
  `sync_source` could only judge history it had signed; cumulative work is computed from this
  chain's own difficulty records, so a 4,700-block unanchored tail is now a decision, not a blind
  spot.

## [0.3.49] - 2026-11-15

Bumped from 0.3.48. One theme: **a node that joins late must not inherit a fork, and a peer that
was merely unlucky must not be banned for it.** A newcomer is bootstrapped from the signed
snapshot instead of crawling, a peer on another branch is never used as a sync source, and the
SDK's third-strike ban for a request that was in flight is replaced by a small, bounded report.

### Added
- **Snapshot-first onboarding on Windows.** `install.ps1` had no fast sync at all, so a fresh
  Windows install re-verified the chain from genesis. It now mirrors `install.sh`: fetch the
  signed `snapshot.json` → verify the signature → download → verify the bytes → import into a
  staging directory → move `chains/`. Every failure is one line (`fast sync unavailable (…) —
  falling back to a normal full peer sync (slower, still correct).`) and never an install
  failure; an existing database is never touched.
- **Automatic catch-up from the signed snapshot** (`gpn_agent::selfheal`).
  `SNAPSHOT_BEHIND_BLOCKS = 20_000`, override `GPN_SNAPSHOT_CATCHUP_MIN_BLOCKS`. A node that
  *answers* with a height more than 20k blocks behind the signed snapshot imports it instead of
  crawling; the lagging database is set aside as `chains-behind-<ts>` and the existing 24 h
  one-recovery ledger prevents loops. An unreadable node (RPC down, no answer) is never treated
  as "behind" — nothing is wiped on a guess.
- **Signed anchors reach the node.** After each verification the supervisor writes
  `<base>/verified-anchors.json` (151 checkpoints) and the node reads it at startup.
- **Peer choice by canonical progress** (`node/src/sync_source.rs`). A peer's claim is judged
  against the signed anchors *and* against "is its tip a block we hold?". A peer on another
  branch is `FORK`: it is never a sync source, while the link stays up so it can still be served
  canonical blocks. A behind-but-canonical peer is kept, never rejected.
- **`start-node.cmd --task`** — the scheduled task's headless worker mode, so closing the start
  window does not stop the node (Windows release bundle).

### Changed
- **Peer re-dial order is anchor match, then best block, then tenure.** On 2026-11-15 a laggard
  preferred whichever peer answered first and settled on a newcomer at #69,295 while a canonical
  peer at #307,300 was reachable. Arrival order is not a fact about the chain; the best block a
  peer claims is. The peer-id tie-break keeps the order identical on every machine.
- The block server asks an anchor-mismatched peer for nothing, and logs one INFO line per peer
  per state change: `peer X best=#N vs ours #M; canonical/anchor-match=<bool>; using/not using
  as sync source`.

### Fixed
- **An abandoned sync request no longer bans the peer.** `sc-network-sync` bans a peer on the
  third disconnect that happens with a request in flight; our journal for 2026-11-14 has 66 of
  those in one day against peers that had done nothing wrong. `sync_tolerance::TolerantSyncStrategy`
  wraps the stock strategy and replaces exactly that one `DropPeer` action — which carries the
  SDK's inflight reason — with a small report (-1024) charged against a per-peer lifetime budget
  ~23,000x below the ban threshold, an INFO line naming the peer and the count, and no
  `disconnect_peer`. Every other `DropPeer` (undecodable payload, bad signature, forged
  justification, genesis mismatch) still passes through untouched and stays fatal.

## [0.3.48] - 2026-11-15

Bumped from 0.3.47. Twenty-one commits, one theme: **the network stops lying about what it
did.** A provider that cannot run the model refuses by name, a job that dies says so, a box
that is pull-only is not a refusal, and a fairness claim is verifiable from `/jobs`.

### Changed
- **Routing is a capability filter, first-come-first-served.** `route_candidates` no longer
  scores busy tier, active count, measured latency or pull penalty — it filters on capability,
  model present (or pullable), VRAM/RAM, the GFLOPS floor with "unknown is not a pass",
  funded, deliverable and a free slot. The queue is a `WaitRing`: FIFO by `/assignments`
  poll arrival, the served provider goes to the back, a 90 s silence drops it, and a provider
  at capacity (GPU locked or `idle.json` set) polls with `take=0` instead of asking for work it
  cannot run. Why: with four healthy providers advertising `llama3.2:1b`, six concurrent jobs
  were leased **6/6** to the provider co-hosted with the access point (0 ms loopback beat every
  remote box).
- `GET /jobs` reports the lease provider, so fairness is observable instead of inferred.
- **One provider address is one candidate.** The registry holds a row per provider *process*
  and a lease is addressed to the account, so one box could fill the candidate list twice and
  burn two of three attempts on a box that had just refused.

### Added
- **A pinned model this box cannot serve is a refusal, not a substitution.** Serving
  `deepseek-r1:32b` on an 8 GB box returned HTTP 200 in 109 ms with `grapes`, produced by
  `llama3.2:1b`; the caller was billed for a model that never ran. `models::model_unservable()`
  now names the reason (absent, with what *is* installed; or too large for the card), wired at
  all three entry points — the channel lane refuses **before** `ch.accept` (409, nothing
  charged, the AP re-leases it), on-chain `run_job` fails `FAIL_CANNOT_RUN` so escrow refunds
  immediately, and `/infer` returns `{ok:false,error}`.
- `GET /jobs` honours `limit` and `since`: `?limit=3` returned 256 rows and cost 256 chain
  reads; it now reads newest-first with `JOBS_LIST_WINDOW` as the ceiling and still responds in
  the ascending shape callers already parse.

### Fixed
- **A job that can no longer be served stops the wait.** `/v1/chat/completions` blocked in a
  fixed 360 s loop and never asked the chain, so a janitor-cancelled job looked like a slow one
  (measured: `cancelled stale Pending #6582` at 08:01:16 while the AP logged handlers stuck
  >336 s). One chain read every ~5 s now stops at once on Cancelled/Failed, or when a budget
  derived from the janitor's own windows is spent, and the answer says "cancelled on chain",
  "failed on chain" or "still <state> after Ns" — never "timeout" when it was a cancel.
- **A pull-only candidate does not consume an attempt.** `failover()` spent all three attempts
  on pull-only Windows boxes (structurally undialable: `pull://`, no inbound port) and never
  dialled the healthy local provider sitting fourth in the list. Only dialable attempts count,
  and an all-pull-only list now says that instead of reporting an empty refusal list.
- **A pull-only box is not a provider that refused.** `channel_endpoint()` returns `None` for
  both a pull-only box and a malformed URL and the caller printed one sentence for both. Now
  `ChannelEndpointFailure{PullOnly,Undialable}` states the structural fact by name.
- **A payload-less lease is not a delivery.** `collect_leases` dropped a failed payload lookup
  silently while still stamping the delivery, so the janitor's 240 s veto sheltered a job no
  provider could run (box 4: `(payload stored)=0 / (payload missing)=24`, every lease stamped)
  and the caller got a Cancelled job four minutes after HTTP 200. Only a lease that carries a
  payload stamps the delivery; the omission is logged once per job.
- **An unreadable ledger is UNKNOWN, not empty.** `load_fresh()` used
  `unwrap_or_default()`, so one failed read made the process answer "this job has no payload"
  *and* let a read-modify-write truncate a 4.5k-entry ledger to the single job being added.
  `read_map()` now separates absent (a fresh host is empty) from unreadable (UNKNOWN);
  `load()` serves the last good map without caching the failure and `load_fresh()` returns `Err`.
- **A Windows task gets no `HOME`.** Every `~/.gpn/...` resolver fell back to `.`, which for a
  Task-Scheduler process is `C:\Windows\System32` — unwritable for the limited token — so from
  the 01:06 update restart the provider persisted nothing and every pulled lease came back
  "payload missing". `firstrun::state_home()` resolves `HOME` then `USERPROFILE`, and `main`
  adopts `USERPROFILE` once before any child spawns and logs it.
- **The progress tick follows the state home too** — the one resolver #185 missed still used
  `HOME.unwrap_or(".")`, so box 1's `provider-tick.json` froze for five hours about a healthy
  process while the runbook tells operators to diagnose a wedge by watching exactly that file.
- **A slow Ollama can no longer blank the inventory.** `:8093/status` read the model inventory
  inline with a 2500 ms timeout while the reader gives up at 1500 ms, so any stall >1.5 s made
  every provider publish no capabilities — observed live when the LLM, finetune and diffusion
  providers all logged "local /status unreadable" in the same second. The read is
  stale-while-revalidate (1200 ms budget, single-flighted refresh) and a failed read keeps the
  installed names, dropping only `can_pull`.
- **The Ollama budget is a compile-time assertion** at module scope, so `cargo build --release`
  and `cargo clippy --bin` fail if a read is allowed to outlive its reader.
- **A query string is not part of the route path.** `GET /jobs?limit=256` and even
  `GET /health?ts=1` returned "not found" because the router kept `jobs?limit=256` as a segment
  (`tiny_http` hands the target verbatim). Same defect on the `:8093` status routes — the path
  the access point polls to decide whether a box is reachable, so any cache-buster would have
  made a healthy provider look down.
- **The disk guard declined exactly when the disk was full.** `df --output=pcent` prints *used*
  percent and the script treated it as free, so it pruned with plenty of space and did nothing
  at 92 % used. The chain halted on a full root FS on 2026-10-19; the guard was silent then.
  Used is used, free is free, and both are reported.
- **The prune guard can see the checkout's own target trees.** It reclaimed 0 MiB while 66 G sat
  in `gpn-agent/target` + `gpn-node/target` that it never scanned, and the root FS hit the 10 %
  floor. It now scans `<crate>/target/debug` under `GPN_TARGET_ROOTS` too, never removes a
  directory merely *named* `debug`, and leaves release profiles and recently-touched trees alone.
- **The snapshot job refuses a stale verifier before it exports**, not after it has deleted the
  previous asset and uploaded a new one; sign + self-verify now precede touching the mirror, so
  the mirror cannot carry bytes with an unverifiable digest.
- **The lane check cannot mistake a timestamp for a fault.** A bare `503` grep counted
  `Nov 14 23:16:34.503` as an HTTP error: 438 "hits" against 0 real 503s. It matches only
  HTTP-shaped statuses and prose faults, and takes `--file` so a saved journal can be replayed
  as evidence.

## [0.3.47] - 2026-11-14

Bumped from 0.3.46. An update now has to prove it happened.

### Fixed
- **An update is not done until the running node reports the new version.** After
  the swap the supervisor restarts the node, queries the version that is actually
  running, and only then marks the update complete. If the new build does not
  report the expected version within the bounded window, it rolls back to the
  previous version and leaves the node running rather than down.
- One attempt per version per boot is recorded, so a broken release cannot
  restart-loop a machine.

### Why
On 2026-11-14 box 3 sat on 0.3.45 for eight hours with 0.3.46 unpacked in
`versions/`: the updater derived its "current" version from the install
directory, found no build stamp, reported `update_available: false`, and in any
case could not overwrite a running executable. Configured is not working.

## [0.3.46] - 2026-11-14

Bumped from 0.3.45. One theme: a peer doing something unusual but recoverable is
no longer exiled.

### Fixed
- **Repeated block requests are never fatal, from any peer, on any branch.** The
  SDK rule (`SAME_REQUEST = Rep::new_fatal`, −2^31, disconnect) is replaced by a
  small penalty with a lifetime budget ~1,450x below the ban threshold, so no
  number of repeats can accumulate into a ban. Excess repeats are **throttled**
  (full request withheld, bounded 8-block prefix for header requests, service
  resumes after the window) with no disconnect and no eviction. Malformed
  payloads, unknown attribute bits and unencodable responses remain fatal.
- **A peer behind on a branch we do not hold is divergence, not health.** The
  fork-tolerance check is now driven by the client ("do we hold their tip?"),
  not by comparing heights, and a protected peer is re-dialled every tick with
  no backoff so a ban cannot become a permanent eviction.
- **Self-heal actually runs.** The divergence check is scheduled in the node
  supervisor (startup + every 5 min) for `gpn_node` and `gpn_fleet`; a stranded
  node stops, renames `chains/` to `chains-fork-<ts>`, imports the signed
  snapshot and restarts. The 24 h ledger is written **after** a successful stop,
  so a failed stop cannot burn the day's single recovery.
- **Signed checkpoint history** in `snapshot.json`: a list of anchors, newest at
  or below the node's height is compared, so a node that is *behind* the weekly
  checkpoint — the common stranded case — can be judged and healed.
- **Pruning mode is resolved per spawn**, not once at supervisor start. A heal
  replaces the DB, and the next respawn previously asked for the old mode,
  producing a permanent `Incompatible pruning modes` restart loop.

### Added
- `DivergenceTolerantBlockServer` and `PoliteDownloader` (we never send a third
  identical request, and never ask a peer on a branch we do not hold).
- `gpn_snapshot` is required by both installers: a missing heal binary now fails
  the install instead of producing a node that silently cannot heal.

## [0.3.45] - 2026-11-13

Bumped from 0.3.44.

### Fixed
- **Published addresses now name the transport the node actually listens on.** A
  node listens on `/ws` in every non-validator mode, but every published
  multiaddr named raw TCP, so a dialler sent a Noise prologue to a websocket
  listener and the connection died after ~62 bytes with nothing logged. That is
  the whole of the "TCP connects, then silence" symptom that kept a second
  machine off the network for two days.
  The transport is now **derived** from the node's own listen set
  (`listen_transport` → `with_transport`) rather than hardcoded, so a
  validator-mode node still publishes raw TCP. Consumers were repaired too:
  `seedlist::clean_published_multiaddr` and the Worker's `sanitizeMultiaddr`
  both treated `/ws` as an unexpected protocol and **discarded every ws
  address**; the access point's own public announcements were bare sockets.
  `dialcheck` no longer retries a bare socket as `/ws` — it dials exactly the
  published string, so a wrong one now fails visibly instead of passing.

- **`network.json` no longer carries a dead seed or a stale `latest_version`.**
  `23.241.109.255` (peer `WFqZ2…`) is gone — TCP 30333/8093/22 all time out —
  and `latest_version` is derived instead of hand-edited, so it cannot sit seven
  releases behind the site.

- Update checks run **hourly** instead of every 6 h (temporary, owner request,
  2026-11-12): in-process, the Windows `GPN-Update` task, its Startup fallback
  loop, and the Linux `gpn-update.timer`. Revert together — see task #161.


## [0.3.44] - 2026-11-12

Bumped from 0.3.43 (`scripts/bump-version.py patch`).

### Added
- **Dial-back verification before a public address is published**
  (`gpn-agent/src/dialcheck.rs`). An address may now only be published by the
  peer id a third party reaches when it dials it: the checker parses the
  multiaddr, refuses a `/p2p` tail that disagrees with the claim, skips (never
  passes) a same-egress verifier, and completes a real noise handshake to read
  the answering peer id. New `gpn-dialcheck` binary for operators (exit 0/2/3/4,
  `--json`). Default mode `wrongpeer` fails open on *unreachable* — only positive
  evidence of a different peer id unpublishes.
- **`gpn-agent/tests` now cover the publish gate**: an address that answers as
  another peer id is never published.

### Changed
- **The gateway election is wired** (it had been `#[allow(dead_code)]`,
  "deliberately not wired yet"). `DialbackOutcome` distinguishes
  `Verified` / `WrongPeer{expected,actual}` / `Unreachable` / `Skipped`;
  same-egress is `Skipped`, never a pass. `SocketRegistry` keeps one live claim
  per socket, newest verified claim wins, and the loser demotes on its next
  cycle without a restart. Two boxes behind one NAT can no longer both be
  gateway. Behaviour is byte-identical while `GPN_SITE_ELECT` is unset.

### Fixed
- **`dialability.json` is keyed by node.** The record now carries `peer_id` and
  `probe_peer_id`, and a verdict naming another peer — or a legacy record with
  no identity — is **Unknown**, never a borrowed leaf/backbone. Live symptom it
  caused: a dead probe node's "leaf" verdict held the canonical seed at
  `multiaddrs: []` while its ports were open.

## [Unreleased]

### Fixed
- **A published seed list could kill a starting node instantly.** `merge_dedupe`
  deduplicated bootnodes by the full `/p2p/<id>` string, so one socket advertised
  under two peer ids survived as two bootnodes; `sc-network` then rejects the
  config with `Error::DuplicateBootnode` and the service aborts ~1 ms after the
  network backend starts (log: `Essential task 'txpool-background' failed`,
  panic `SelectNextSome polled after terminated`). Bootnodes are now deduplicated
  by **address**, first source wins, conflicting entries are dropped with a
  warning. Seen live 2026-11-12: `https://gpncoin.ai/seeds.json` listed
  `/ip4/172.117.107.91/tcp/30333` twice (access-point id + static seed id); every
  node started with a fresh base path died until the list was fixed.
- `worker/worker.js` (the seeds.json publisher) applies the same rule: one socket
  is published under exactly one peer id, so the conflict cannot be re-emitted.

## [0.3.43] - 2026-11-11

Bumped from 0.3.42 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.42] - 2026-11-10

Bumped from 0.3.41 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.41] - 2026-11-10

Bumped from 0.3.40 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.40] - 2026-11-10

A node no longer reserves a slot for itself.

### Fixed
- **Tier-0 oracle seeds exclude our own peer id.** `network.json`'s bootstrap
  list contains box 1's address, so box 1 was told to reserve and re-dial *itself*
  every 60 s — wasting a reserved slot and inflating the "tier-0 connected"
  count. Observed in box 1's own log (2026-11-10): the reserved list named its
  own peer id twice.

## [0.3.39] - 2026-11-10

Remove the LAN seed I added in 0.3.29 — it was wrong for every remote box.

### Fixed
- **`/ip4/192.168.1.20/…` is gone from the shipped bootnodes** (network.json, the
  Windows launcher defaults, `/seeds.json`). A private address only means
  something on the network that owns it: box 1 runs on a *different* network that
  also uses `192.168.1.0/24`, so the entry pointed it at a stranger's device while
  its real route — the public address — was never tried. Measured 2026-11-10:
  our ARP table shows `192.168.1.46 → FAILED`, i.e. box 1 is not on our wire.
- Local peers still get a LAN address when it is genuinely useful: the installer
  computes it per machine from a *private* seed literal on the same /24, and the
  supervisor does the same at runtime. Neither ships one.

## [0.3.38] - 2026-11-10

The dashboard is on by default.

### Changed
- **`GPN_GUI` defaults to ON.** It was opt-in (`--gui` / `GPN_GUI=1`), so any
  start path that forgot the flag produced a node with no panel — which reads as
  "the node is broken", because the console only scrolls and the dashboard is the
  only place state is visible (box 1, 2026-11-10). It binds `127.0.0.1` only, so
  defaulting it on exposes nothing.
  Opt out with `GPN_GUI=0` or `--no-gui`; `--gui` and `GPN_GUI=1` still win.

## [0.3.37] - 2026-11-10

A node tells the network how many peers it has, instead of someone guessing.

### Fixed
- **`/status` publishes the node's own peer count (#W78).** The status service now
  reads `system_peers` live and reports `peers` as a number (a real `0` for a lone
  node) or `null` when the node RPC is unreadable — silence is never rewritten as
  zero. The access point merges it into the provider row, so `/providers` and
  `/network` carry `peers` too, and `nodes.html` decides "peered" from `peers > 0`
  when a box reports one, falling back to peer-of-this-node only for older builds.
  Box 1 was syncing from box 2 while the site labelled it "not peered"; the label
  was derived from bootnode membership rather than the node's own view.

### Added
- **`gpn_snapshot checkpoint` exit-code contract documented (#156)** and
  `scripts/auto-release-if-behind.sh`: publishing what is already cut but
  unpublished no longer waits for a human to remember the two manual steps.

### Why the patch

0.3.36 was signed from `e7e93d7`, and the peer-count fix landed after it. The
updater then correctly refused to install 0.3.36 over the locally deployed
binaries (same version, different build) — a rollback the version number could
not express. 0.3.37 is the first version that contains everything the fleet is
actually running.

## [0.3.36] - 2026-11-09

Peering stops being "whoever answers first".

### Added
- **Ranked peer priority (#154).** Tier-0 oracle seeds (up to 5, today the Linux
  box) hold reserved slots and are re-dialled every 60 s; everything else is
  ranked by on-chain tenure (first block at which the peer appears as a provider
  or full node), newest last. Unknown peers fill leftover slots and are evicted
  first. Design: `design-peer-priority-checkpoints.md` §1.
- **Peer exchange, `gpn/peers/1` (#155).** Every 5 min a node gossips up to 32
  known peers, so a node that cannot reach the seed still joins the network
  instead of pairing off with whoever is left. This is the fix for the
  2026-11-09 split into `linux ↔ box 2` and `box 1 ↔ box 4`.

## [0.3.35] - 2026-11-09

A snapshot now says WHICH chain it is.

### Added
- **`snapshot.json` v2 carries a checkpoint**: `genesis` plus the `block_hash` at
  `block`, both inside the signed canonical text. The signer
  (`scripts/snapshot-metadata.py`) emits v2 when both are supplied, and the
  verifier accepts **v1 and v2** — the prefix is part of the signed bytes, so old
  documents keep verifying forever.
- `scripts/snapshot-weekly.sh` reads genesis + the block hash from the node's own
  RPC and **refuses to publish a snapshot without them**. A checkpoint-less
  snapshot can only say "a chain with the right genesis", which is how a node
  ends up 117k blocks down a fork (2026-11-09).

### Verified
- Round-trip test: v2 signs and verifies; v1 still signs and verifies; the
  checkpoint fields are present in v2 and absent in v1. Half-filled checkpoints
  are rejected by the verifier rather than silently accepted.

## [0.3.34] - 2026-11-09

A legacy chain DB starts instead of restart-looping.

### Fixed
- **`Incompatible pruning modes [stored: Constrained(256); requested: ArchiveAll]`.**
  For a DB with no archive marker the supervisor passed *no* pruning flags and
  relied on the node's compiled default — which is archive. A DB created by the
  old constrained default therefore hard-failed at every start and the supervisor
  restart-looped it (box 1, 2026-11-09, three kills in 14 s). It now asks for the
  mode that is actually stored (`--state-pruning 256 --blocks-pruning 256`), so
  the node starts and the operator can still wipe the DB deliberately.

## [0.3.33] - 2026-11-09

A node that cannot see the network no longer extends it.

### Changed
- **Block sealing pauses at zero peers** (owner decision 2026-11-09). The
  peer-watch task writes `<base-path>/gpn-isolated` when connected peers reach 0;
  the miner refuses to submit seals while that marker exists and resumes the
  moment a peer connects. Rationale, measured: box 1 spent days mining a
  117k-block fork alone — six healthy children, a green dashboard, blocks
  sealing — while the network was at 274k. "Identity is genesis + chain-id" is
  still true, but a private chain is indistinguishable from a working one until
  somebody compares heights, so the node now stops instead of producing.
  This supersedes the earlier "explicitly NO 0-peer mining ban" note in
  `design-network-identity-fork-tolerance.md`.

### Changed (ops)
- The chain snapshot is built **daily** (was weekly), so a new install boots
  from a copy of the correct chain at most ~24 h old.

## [0.3.32] - 2026-11-09

`--stop` actually stops things.

### Fixed
- **`gpn_node --stop` reported "not running" for a live tree.** The first version
  matched exact names (`gpn_provider`, `gpn_finetune`, `gpn_ml`,
  `gpn_provider_heartbeat`); the real children are `gpn_llm_provider`,
  `gpn_finetune_provider`, `gpn_ml_provider`, `gpn_provider_status`. It now
  enumerates the process table and stops **everything whose name starts with
  `gpn`**, children first (the supervisor respawns what it still owns), so it
  works whatever the binaries are called.
  A stop command that answers "nothing to stop" while ports and files stay locked
  is worse than no command: box 1, 2026-11-09.

## [0.3.31] - 2026-11-08

The fleet stops needing a human at the keyboard.

### Added
- **`gpn_node --stop`** — stops every GPN process on the machine, children
  included. Stopping the supervisor (or `schtasks /End`) leaves children holding
  the GPU and ports 8093/8092; the next start then dies with `port-in-use` (75)
  and looks like a broken node. That trap cost box 1 and box 2 an evening each.
- **Update fallback without Task Scheduler.** When `schtasks /Create` is refused
  (a standard account on box 2 and box 4), the installer now writes
  `update-loop.ps1` plus a Startup shortcut that runs the same wrapper at logon
  and then every ~6 h (±30 min jitter). A machine that cannot register the task
  still updates itself.

### Changed
- The Linux release build reclaims the debug cargo profile after packaging: the
  cache has filled the root filesystem four times (it halted the chain once).

## [0.3.30] - 2026-11-08

Version bump only — no source change.

It existed to escape a real deadlock in the updater guard: this box's installed
`gpn_update` was stamped `0.3.29/9c9e82c` while the published asset was built
from `c77c306` — same version, older commit — so #148 correctly refused to swap
newer local code for an older published build and the fleet stayed on 0.3.28.
Two builds of one version cannot be told apart by version, hence the bump.

## [0.3.29] - 2026-11-08

The LAN can finally peer, and the console points at the dashboard.

### Added
- **Same-LAN seed address in the bootnode defaults** (`/ip4/192.168.1.20/...`),
  in `network.json`, the Windows launcher defaults, and `/seeds.json`.
  Four machines on one LAN could heartbeat to the access point but never peer:
  they dialled the seed's PUBLIC address, the router looped it back (hairpin
  NAT), and the connection was dropped as a self-dial. Off-LAN peers fail this
  dial instantly and use the public entries.
- **The launcher prints the dashboard URL** (`http://127.0.0.1:8099`). The
  console only prints failures; status lives on the dashboard.

### Changed
- `gpncoin.ai/nodes` no longer calls a serving node "offline" just because it is
  not a libp2p peer of the bootnode: ranking is serving → registered →
  connected → offline, and a serving-but-unpeered node reads
  **"mining + serving (not peered)"**.

## [0.3.28] - 2026-11-07

Fix a crash loop I introduced in 0.3.26.

### Fixed
- **`--reserved-peers` is not a flag. The binary takes `--reserved-nodes`.** The
  supervisor passed the invented name, so every node whose launcher set
  `GPN_RESERVED_PEERS` started the chain node, had it exit code 2 on the unknown
  argument, restarted it, and repeated — the dashboard showed "node is down"
  forever. Box 2 was crash-looping on it (2026-11-07 00:00).
  Verified against the real CLI: `gpn-node --help` lists `--reserved-nodes`,
  `--reserved-only`, `--bootnodes`, `--bootnodes-dns`, `--bootnodes-url` — and
  nothing called `--reserved-peers`.

### Note
- Nodes that already installed 0.3.26/0.3.27 can be unblocked immediately by
  clearing the variable (`set "GPN_RESERVED_PEERS="` in start-node.cmd) without
  waiting for this release.

## [0.3.27] - 2026-11-06

LAN-aware seed dialling.

### Added
- **Same-subnet seed addresses, computed per machine.** A node that shares a
  subnet with the seed now dials it on the LAN instead of through the router.
  Two mechanisms, deliberately independent:
  * `install.ps1` prepends `/ip4/<my-lan-ip>/tcp/<port>/p2p/<id>` to that
    machine's launcher when a seed literal is on the same /24 — written only
    into that machine's `start-node.cmd`, never into the bundle (a private
    address is meaningless, and possibly harmful, on somebody else's network).
  * the supervisor does the same at runtime for seeds configured with a private
    address (UDP-connect probe, no packet sent), so auto-updated machines get it
    without the launcher being rewritten.

### Known limitation (stated, not hidden)
- This does **not** solve a seed reached through a *public* address from inside
  the same LAN (hairpin NAT): the peer cannot derive the seed's private address
  from its public one, and libp2p may reject the looped connection as a
  self-dial. That case needs the seed's LAN address configured explicitly on the
  LAN nodes, or mDNS where the router permits multicast.

## [0.3.26] - 2026-11-06

Block rewards finally reach the machine that mined them.

### Fixed
- **The miner now signs seals as the node's own account.** The reward goes to
  whoever `FindAuthor` recovers from the pre-runtime digest, so a node mining
  under a shared/dev key pays somebody else. Measured on testnet before this
  change: **95,454 blocks authored by the dev seed "Alice", 0 by any real box** —
  every machine's wallet sat at 0 GPN while it mined for days. The supervisor now
  derives `GPN_MINER_ACCOUNT` from the node's own keystore (`GPN_SIGNER_FILE`)
  and logs `mining as this machine's account: <ss58>`.
  Verified: this box went from 0 → 3 authored blocks and +150 GPN in the first
  minute after the change.
- `--alice` removed from the fleet launcher: it seeds the node keystore with the
  dev key, which the miner falls back to when `GPN_MINER_ACCOUNT` is unset.

### Added
- `firstrun::ss58_to_public` — ss58 → raw 32-byte public key, so the miner can
  sign as exactly the account the operator sees.

## [0.3.25] - 2026-11-06

The launcher stops guessing about the key.

### Added
- **`gpn_node --ensure-node-key [--file <path>]`.** Opens the key file and
  reports the truth: creates it when absent (via `gpn-node key
  generate-node-key`), exits 3 with the OS error when it exists but is
  unreadable / the wrong size / held open by another process. Also creates the
  parent directory.

### Changed
- The Windows launcher's key handling is now **two lines**: set
  `GPN_NODE_KEY_FILE`, call `--ensure-node-key`, stop if it exits non-zero. The
  `if not exist` / `copy` / `mkdir` / `keygen` probe chain is gone.

### Fixed
- **The misleading "refusing to start a node with a shared/default identity"
  message is gone.** On box 2 (2026-11-06) `cmd`'s `if not exist` reported a
  64-byte, readable, unlocked key as missing, minutes after `if exist` reported
  it present; the launcher then blamed the identity. Three wrong diagnoses in a
  row (ownership, privilege, wallet) traced back to that one probe.

## [0.3.24] - 2026-11-06

An update now repairs the launcher it left behind.

### Fixed
- **Windows: `gpn_update --apply` rewrites a stale `start-node.cmd`.** 0.3.23's
  installer writes a launcher that execs `gpn_node.exe` directly; before it, the
  launcher ran a PowerShell pipeline that exits 1 *before the node starts* when
  spawned without a console. Updates only swap binaries, so every machine
  installed before 0.3.23 reported the new version and stayed dead — box 1 on
  2026-11-06: `gpn_node 0.3.23` on disk, `--diagnose` all green, and
  `last-exit.json` saying `version: launcher, pid: 0`.
  The repair lives in the binary (not the installer or the wrapper) because the
  binary is the one thing an update always replaces, including on machines whose
  update wrapper predates the fix. The old line's own paths are reused, a
  `.bak-<ts>` copy is kept, and an unparseable line is left untouched and logged.

## [0.3.23] - 2026-11-05

Zero-balance onboarding that actually works, plus the four Windows bugs that
made three installs look broken.

### Added
- **Unsigned proof-of-work entry (chain spec 50/51).** A node with no coins and
  no on-chain account now registers itself: the PoW is verified in
  `validate_unsigned`, the account is created at existential deposit 0, and the
  provider is registered. No signature, no fee, no deposit.
  Verified live: account with nonce 0, balance 0, `RegisteredProviders = true`.

### Changed
- The runtime's node-key path on Windows is resolved **at launcher runtime**
  from `%USERPROFILE%`, not baked at install time; an existing install-time key
  is used as a fallback so older machines keep their identity.
- Windows launcher runs `gpn_node.exe` **directly** instead of through
  `powershell | Tee-Object`, which exited 1 before the node started whenever the
  launcher was spawned without a console.

### Fixed
- `gpu::get_gpu_info` no longer **invents** a device ("Unknown GPU", 8 GB,
  compute 8.0) when both `nvidia-smi` and `rocm-smi` fail; a failed read is now
  an error (exit 67, `no-gpu`) with a remediation line.
- Windows installs that cannot create a scheduled task keep their node but lost
  auto-update; the exit is now reported with its real cause instead of
  `exit 1 / unknown`.

## [0.3.22] - 2026-11-05

Bumped from 0.3.21 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.21] - 2026-11-04

Bumped from 0.3.20 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.20] - 2026-11-04

Bumped from 0.3.19 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.19] - 2026-11-04

Bumped from 0.3.18 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.18] - 2026-11-04

Bumped from 0.3.17 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.17] - 2026-11-04

Bumped from 0.3.16 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.16] - 2026-11-04

Bumped from 0.3.15 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.15] - 2026-11-03

Bumped from 0.3.14 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.14] - 2026-11-03

Bumped from 0.3.13 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.13] - 2026-11-03

Bumped from 0.3.12 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.12] - 2026-11-03

Bumped from 0.3.11 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.11] - 2026-11-02

Bumped from 0.3.10 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- `gpn_update`: a standalone updater for every deployment mode where `gpn_node`
  never runs (`gpn_fleet`, hand-run nodes, containers). `--check` verifies the
  signed manifest without downloading (exit 3 when newer); `--apply` installs
  into `versions/<v>/`, flips `current`, keeps `current.previous`, and copies
  the binaries over the install dir atomically per file.
  `scripts/gpn-autostart.sh` now also writes `gpn-update.service` +
  `gpn-update.timer` (every 6 h, randomized ±30 min).

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.10] - 2026-11-01

Bumped from 0.3.9 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.9] - 2026-11-01

Bumped from 0.3.8 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.8] - 2026-11-01

Bumped from 0.3.7 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.7] - 2026-11-01

Bumped from 0.3.6 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.6] - 2026-11-01

Bumped from 0.3.5 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.5] - 2026-11-01

Bumped from 0.3.4 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.4] - 2026-10-30

Bumped from 0.3.3 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.3] - 2026-10-30

Bumped from 0.3.2 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.2] - 2026-10-30

Bumped from 0.3.1 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.1] - 2026-10-28

Bumped from 0.3.0 (`scripts/bump-version.py patch`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [0.3.0] - 2026-10-26

Bumped from 0.2.0 (`scripts/bump-version.py minor`). Describe the release before publishing it.

### Added
- _TODO_

### Changed
- _TODO_

### Fixed
- _TODO_

## [Unreleased]

### Added

- **Fleet-spread update schedule** (`gpn-agent/src/update.rs`): the auto-update
  check no longer fires at "every 6 h from start" for the whole fleet. Each node
  checks at its own random phase in `[0, interval)`, derived from its payout
  account (or base path), then every `interval ± 5–10 %`; a found release is
  downloaded after a further random `[0, GPN_UPDATE_SPREAD_SECS]` (default
  20 min, `0` disables). A starting node never waits for either — the work stays
  on the auto-update thread. See `docs/updates.md` → "The schedule".

## [0.2.0] - 2026-10-21

_MINOR — new features, backwards compatible. No wire, consensus or p2p change:
`min_version` stays at 0.1.0. This is the first release to carry a real version
bump (0.1.0 was reported by every build from 2026-10-14 to 2026-10-21)._

### Added

- **First-run wizard** (`1f980e5`): `gpn_node` downloads the chain spec, starts,
  and asks one question instead of requiring a hand-written config.
- **Opt-in auto-update + minimum-version cutoff** (`15bd1ad`, `719abc7`): signed
  (ed25519) release manifest, version comparison, staged/atomic binary swap, and
  the sync-only standby role when a build is below `min_version`. New
  `gpn_node --check-updates` (read-only verdict).
- **Release tooling**: `scripts/bump-version.py` (this release's version is its
  first product), version/SHA derived from the built artifact in
  `scripts/gen-release-manifest.py`, and the policy + release checklist in
  `docs/updates.md`.
- **Provider peer ids are published** (`8b859dc`) so a live provider is dialable
  from the node directory, and the AP-hub off-chain escrow protocol with its
  job state machines (`d606264`).

### Changed

- `gpn_node --version` now prints the build identity:
  `gpn_node 0.2.0 (spec 41, <sha>)`, and `--version`/`--help` are answered
  before the wizard runs (`89aaa4e`, `b1c0ffa`).
- Dialability gate accepts `/dns4` and reports STUN/TCP evidence instead of
  halting the chain; heartbeat takes `--endpoint` and `GPN_SEEDS_URL`
  (`37a1786`, `47153f4`).
- Job token counts are the backend's real numbers, not zeros (`b03b8c4`).

### Fixed

- Never claim a job before its `create` is included (`1dbfb09`), and never
  submit a seal for a superseded template (`1c77e61`) — both were chain-visible
  stalls/reorgs.
- An unreadable `NextJobId` no longer becomes `0`; the AP refuses to create
  instead of handing out job id 0 (`cf70f67`).

