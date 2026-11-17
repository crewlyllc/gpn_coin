# Changelog

All notable changes to GPN are recorded here. The version in this file is the
one `gpn_node --version` prints, the one the signed release manifest publishes,
and the one the updater compares (`docs/updates.md`).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions are [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
the GPN-specific meaning of each level documented in `docs/updates.md`.

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

