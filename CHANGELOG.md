# Changelog

All notable changes to GPN are recorded here. The version in this file is the
one `gpn_node --version` prints, the one the signed release manifest publishes,
and the one the updater compares (`docs/updates.md`).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions are [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
the GPN-specific meaning of each level documented in `docs/updates.md`.

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

