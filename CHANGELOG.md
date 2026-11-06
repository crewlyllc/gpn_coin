# Changelog

All notable changes to GPN are recorded here. The version in this file is the
one `gpn_node --version` prints, the one the signed release manifest publishes,
and the one the updater compares (`docs/updates.md`).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions are [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
the GPN-specific meaning of each level documented in `docs/updates.md`.

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

