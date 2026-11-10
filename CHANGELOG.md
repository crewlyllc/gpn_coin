# Changelog

All notable changes to GPN are recorded here. The version in this file is the
one `gpn_node --version` prints, the one the signed release manifest publishes,
and the one the updater compares (`docs/updates.md`).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions are [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
the GPN-specific meaning of each level documented in `docs/updates.md`.

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

