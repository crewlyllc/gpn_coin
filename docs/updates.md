# Updates — auto-update, the minimum-version cutoff, and release signing

*Verified against: chain spec 45 / release 0.3.11 — checked 2026-11-02 at block 236,986.*

A GPN node is a **consensus participant**, so "run an old binary" is not a
neutral choice: an out-of-date node can seal blocks that the rest of the network
rejects (it looks like a chain stall), serve jobs with an incompatible schema, or
talk a libp2p dialect its peers have dropped. GPN therefore has two related
mechanisms:

| | What it does | Opt-in? |
|---|---|---|
| **Auto-update** | Downloads, verifies and installs a new release for you | **Yes** — off unless you ask |
| **Minimum-version cutoff** | Refuses to let a build that is too old **mine or announce** | No — always on |

The deal an operator gets:

* **Opt in to auto-update** → signed updates are installed automatically, with a
  short restart, at whatever hour a release lands. Nothing to do, nothing to
  watch.
* **Update manually** → completely fine, *until* a release raises
  `min_version`. From then on the node keeps **syncing** but stops **mining**
  and stops **announcing** until it is upgraded. It is not banned and not
  slashed; it is simply not allowed to produce blocks it would have to be
  reorged out of.

Both paths verify the same thing: an **ed25519 signature** over a canonical form
of the release manifest, made with the GPN release key.

---

## 1. Version numbering

There is **exactly one version** in GPN, and one tool that changes it:

```text
Cargo.toml  ──▶  CARGO_PKG_VERSION  ──▶  gpn_node --version
     │                                        │
     │                                        ├─▶ gpn-build-v1 stamp in the binary
     │                                        │        │
     └── scripts/bump-version.py               │        ▼
                                              │   scripts/gen-release-manifest.py
                                              │        │   (reads the stamp, not a flag)
                                              ▼        ▼
                                       updater compares manifest.version with its own
```

Never edit a version by hand, and never type one into a release command. A
version that only exists in a hand-typed `--version 0.2.0` flag is how 0.1.0
came to be reported by every build from 2026-10-14 to 2026-10-21: the manifest
copied a value that never moved, so `is_newer()` was always false and the
`min_version` cutoff could never fire.

### The commands

```bash
scripts/bump-version.py patch        # 0.2.0 -> 0.2.1
scripts/bump-version.py minor        # 0.2.1 -> 0.3.0
scripts/bump-version.py major        # 0.3.0 -> 1.0.0
scripts/bump-version.py --set 0.2.0  # explicit; refuses to go backwards
scripts/bump-version.py --check      # print the version; fail if the crates disagree
```

It updates `gpn-agent/Cargo.toml` (`[package]`), `gpn-node/Cargo.toml`
(`[workspace.package]`), every workspace member's literal version, and prepends
a `CHANGELOG.md` stub. It validates strict `MAJOR.MINOR.PATCH` (no `-rc1`, no
`+build` — `update.rs::parse_version` ignores suffixes, so two builds could
compare equal and the updater would never fire), is idempotent, and refuses to
guess when the crates disagree.

### What each level means

| Level | When | `min_version` | libp2p protocol id |
|---|---|---|---|
| **MAJOR** | Breaking change to the **wire, consensus or p2p**: block/extrinsic format, the RPC a peer or the scheduler depends on, the p2p message set, the release-manifest canonical form. Also: a release that makes older nodes' data wrong. | **Raise it to the new version** (in the same release) | **Bump** |
| **MINOR** | New features, backwards compatible: a new endpoint, a new optional field, a new tool. An old node still interoperates. | Leave it, unless the release genuinely requires everyone to move | Leave it |
| **PATCH** | Fixes only — no format, no schema, no new capability. | Leave it | Leave it |

A runtime-only change (`set_code`) is **not** a reason to bump either the
protocol id or `min_version`: the runtime ships forklessly at a known block and
old and new *nodes* interoperate fine while it happens. Bumping the protocol id
is the network half of the cutoff — it isolates an incompatible peer instead of
letting it half-work (§7.2).

Bump **exactly one** level, in the commit that carries the change it describes,
and land the bump with that change — not in a later "release" commit. The
`0.x` line is not an excuse: while `MAJOR` is 0, a `MINOR` bump *may* break
compatibility, so `MINOR` + a raised `min_version` is the pre-1.0 way to ship a
breaking change. It is still a separate decision per release: nothing in this
repo raises a cutoff automatically.

### Where the version lives

| Path | Field | Written by |
|---|---|---|
| `gpn-agent/Cargo.toml` | `[package] version` → `CARGO_PKG_VERSION` of the node/agents | `bump-version.py` |
| `gpn-node/Cargo.toml` | `[workspace.package] version` | `bump-version.py` |
| `gpn-node/{runtime,node,gpn-shield,pallets/*}/Cargo.toml` | `[package] version` (literal, not inherited) | `bump-version.py` |
| `CHANGELOG.md` | newest `## [x.y.z]` heading | `bump-version.py` (stub) then a human |
| artifact bytes | `gpn-build-v1 version=x.y.z sha=<sha> spec=<n>` | `gpn-agent/build.rs` at compile time |
| `website/releases.json` | `version` / `min_version` | `gen-release-manifest.py` (reads the artifact stamp) |

`Cargo.lock` is left alone — cargo rewrites it on the next build.

### A release never replaces a newer local build

`gpn_update --apply` compares the commit in the **build stamp**, not just the
version. The stamp is now linked into every `gpn-agent` binary
(`firstrun::GPN_BUILD_STAMP`, `#[used]`), not only the ones that print a
version, so this works for `gpn_scheduler` and the providers too.

Two installs are refused, and the refusal leaves `current` and every file in the
install dir untouched (exit `3` — "a release exists but was not applied", the
same code `GPN_AUTO_UPDATE=0` uses, so an hourly timer fire that correctly
declines is not a failed unit):

| Installed | Release | Verdict |
|---|---|---|
| `0.3.28` `sha=e188444` | `0.3.28` `sha=77e3292` | **refused** — same version, different commit |
| `10.0.0` | `9.9.9` | **refused** — downgrade |
| `0.3.27` `sha=c59114f` | `0.3.28` `sha=77e3292` | installed |
| `sha=unknown` (either side) | anything | installed — unknown is not evidence |

Why it exists: a host that deploys a locally built `HEAD` (this testnet does,
via `scripts/deploy-bin.sh`) is running version `0.3.28` *of a later commit*
than the signed `0.3.28` cut. A version comparison sees "0.3.28 → 0.3.28, up to
date" — or, worse, "0.3.27 → 0.3.28, go ahead" when only the **supervisor** is
older than the release, which is exactly how a timer fire silently replaced a
newer `gpn_scheduler` (2026-11-07, `#148`).

`--force` is the deliberate override (re-installing the shipped build over a
local one, or rolling back by hand). Do not put it in the timer.

## 2. The release manifest

`https://gpncoin.ai/releases.json`

```json
{
  "version": "0.2.0",
  "min_version": "0.1.0",
  "published_at": "2026-10-21T20:41:16Z",
  "assets": [
    {
      "os": "linux",
      "arch": "x86_64",
      "url": "https://github.com/crewlyllc/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz",
      "sha256": "f0fa377f417b52560bb487154ceff48e0883865d0339167f51c0a010a057f9f8",
      "size": 331643208,
      "layout": "bundle"
    },
    {
      "os": "windows",
      "arch": "x86_64",
      "url": "https://github.com/crewlyllc/gpn_coin/releases/download/windows-latest/gpn-windows.zip",
      "sha256": "dab95de5deea3d7a14468f527789a7aeef8295b32ac49193c940d6e588400879",
      "size": 225578343,
      "layout": "bundle"
    }
  ],
  "signing_key": "<64 hex ed25519 public key>",
  "signature": "<128 hex ed25519 signature>"
}
```

| Field | Meaning |
|---|---|
| `version` | The newest release. `M.m.p`, numeric comparison (so `0.10.0 > 0.9.9`). |
| `min_version` | **The cutoff.** A node whose own version is below this must not mine or announce. Empty = no cutoff. |
| `published_at` | Informational (ISO-8601 UTC). Not used for any decision. |
| `assets[]` | One entry per `os`/`arch`: `linux`/`windows`/`macos` × `x86_64`/`aarch64` (`amd64`/`arm64` also accepted). `size: 0` means "not declared" — the hash is still checked. |
| `assets[].layout` | `bundle` (an archive of the whole release) or `single` (one binary). **Absent means infer from the URL**: `.tar.gz`/`.tgz`/`.tar`/`.zip` → bundle. |
| `assets[].files` | Optional `{relative path: sha256}` for a bundle. Signed. The build-stamp check already pins the version; this pins individual files. |
| `signing_key` | The key that produced `signature`. Must equal the key compiled into the node, otherwise the manifest is rejected **before anything is downloaded**. |
| `signature` | ed25519 over the canonical text below. |

### What exactly is signed

**Not the JSON.** The signature covers a canonical, line-oriented text blob, so
the signer (Python/PyNaCl) and the verifier (Rust/ed25519-dalek) cannot disagree
about JSON formatting, key order or whitespace:

```text
gpn-release-v1
version=0.2.0
min_version=0.1.0
published_at=2026-10-21T20:41:16Z
signing_key=<64 hex, lower-case>
asset=linux/x86_64 https://github.com/crewlyllc/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz <sha256> 331643208
asset=windows/x86_64 https://github.com/crewlyllc/gpn_coin/releases/download/windows-latest/gpn-windows.zip <sha256> 225578343
```

Rules (both sides implement them; `gpn-agent/src/update.rs::canonical_text` is
the reference implementation):

* assets are sorted by `(os, arch, url)` — file order does not matter;
* every line ends with `\n`, including the last;
* the two hex fields are lower-cased before hashing;
* the field names and `gpn-release-v1` prefix are load-bearing. Changing them is
  a manifest format change and needs a node release.

#### The extended rendering (`--sign-layout`)

A bundle can carry two more signed fields, appended to their asset's line:

```text
asset=linux/x86_64 https://…/gpn-linux.tar.gz <sha256> 331643208 layout=bundle
file=linux/x86_64 gpn_node <sha256>
file=linux/x86_64 gpn-node <sha256>
```

This is **additive**: with neither field present the rendering is byte-identical
to the pre-bundle format. The verifier tries the extended text first, then the
legacy one, so a manifest signed either way is accepted.

It is **off by default** (`--sign-layout` turns it on) for one concrete reason:
a node built before this work computes only the legacy text, so it rejects
anything else. The 0.2.0 manifest is signed in the legacy rendering precisely so
that the already-deployed 0.1.x/0.2.0 fleet can verify it and update — verified
on a real pre-bundle binary:

```
  update: manifest verified (https://gpncoin.ai/releases.json): latest=0.2.0 min=0.1.0
  result: this node (0.2.0) is supported — it would mine
```

Nothing is lost by leaving it off: what a *bundle* is comes from the signed
`url` (`.tar.gz`/`.zip`), never from the `layout` field. An attacker who edits
`layout` in a proxy-served manifest cannot turn a signed 331 MB archive URL into
"the new `gpn_node`" — see `asset_layout()` in `gpn-agent/src/update.rs`. From
0.3.0 on, once every node can verify both renderings, publish with
`--sign-layout` to bind `layout` and per-file hashes too.

This binds **everything**: the version, the cutoff, the assets and their hashes.
Editing a single character of `min_version` or of an asset's `sha256` invalidates
the signature.

---

## 3. Bundles: releases are archives, installs are versioned directories

A GPN release is **not one binary**. `gpn_node` is a supervisor that spawns
`gpn-node`, `gpn_scheduler`, `gpn_llm_provider`, `gpn_provider_status`, … — about
60 executables in `gpn-linux.tar.gz` (~331 MB) and `gpn-windows.zip` (~225 MB).
Replacing only `gpn_node` leaves the supervisor and every child it spawns on
different versions: different RPC schemas, different job formats, a node that
fails in ways that look like a chain problem. So the updater installs a **whole
tree per version** and switches between them.

### Layout

```text
~/.gpn/node/update/                  # <base_path>/update
  versions/
    0.2.0/                           # complete, stamp-verified release
      gpn_node  gpn-node  gpn_scheduler  …  scripts/  gpn-node.example.toml
    0.3.0/                           # the version staged/last installed
  current -> versions/0.3.0          # symlink (a directory junction on Windows)
  current.previous -> versions/0.2.0 # one-step rollback target
  pending-update.json                # what is staged, not yet switched
  update-state.json                  # cached latest/min version (offline cutoff)
```

### The flow (never half-swapped)

1. fetch the manifest, **verify the signature** — nothing is downloaded before it
   verifies;
2. stream the bundle to a temp file (not into RAM: 331 MB on a small VPS is how
   an updater gets OOM-killed mid-update);
3. verify the bundle's `sha256` **and** `size`;
4. extract into `versions/.<version>.tmp-<pid>/` — a hidden staging directory;
5. **prove identity**: the extracted `gpn_node` must carry the
   `gpn-build-v1 version=<manifest version>` stamp, every other stamp in the
   tree must agree, `gpn-node` must be present, and every per-file hash the
   manifest declares must match. A bundle that says 0.1.9 while the manifest
   says 0.2.0 is refused here;
6. `rename` the staging directory to `versions/<version>` (same directory, so it
   is atomic; the live tree is untouched until now);
7. write `pending-update.json`;
8. on restart, re-verify the version directory, then **flip `current`**: build
   the link under a temporary name and `rename` it over `current` — one atomic
   directory-entry replacement. `current` points at a complete tree or the old
   one, never at a half-written one;
9. write `current.previous`, re-exec the supervisor from `versions/<new>/gpn_node`.

Directories older than *current* and *previous* are pruned after a successful
switch, so a node keeps at most two copies (~660 MB) instead of growing by one
release every time.

Windows has no symlinks without Developer Mode, so `current` is a **directory
junction** (`mklink /J`, no privileges needed) built under a temp name and moved
into place. Removing a junction deletes the link only — the version tree stays.

### Why the stamps, not just the hash

The signature proves *GPN published this bundle*. The build stamp proves *this
tree is the version the manifest names* — it comes from `gpn-agent/build.rs` and
is baked in at compile time, so it cannot be fabricated without rebuilding. That
is the check that catches "the bundle asset was not re-uploaded after the bump",
which would otherwise install a 0.1.9 tree as 0.2.0 and then blame the release.

Only `gpn_node` actually carries the stamp — `build.rs` lives in `gpn-agent`, and
`gpn-node` is a separate workspace crate with no build script. So the rule is:
the supervisor **must** be stamped with the manifest version, and any *other*
stamp found in the tree must agree with it. Requiring a stamp on `gpn-node` too
(the first version of this check) rejected every real release. Verified against
the published artifact:

```
$ GPN_REAL_BUNDLE_TREE=/tmp/gpnstage cargo test --lib real_release_tree_verifies -- --ignored
switched-in: gpn_node 0.2.0 (spec 41, 7e9ff08)
```

### `--check-updates` is bundle-aware

```
  update: manifest verified (https://gpncoin.ai/releases.json): latest=0.2.0 min=0.1.0
  update: asset linux/x86_64 layout=bundle 331643208 bytes sha256=f0fa377f417b5256
  update: 0.2.0 is already installed at /home/op/.gpn/node/update/versions/0.2.0 — nothing to do

  result: this node (0.2.0) is supported — it would mine — installed bundle: 0.2.0
```

It is still read-only: it fetches the manifest, reports, and installs nothing.

---

## 4. Key custody

| | Where it lives | Who has it |
|---|---|---|
| **Private** key (32-byte ed25519 seed) | Offline with GPN release operations. Default on an ops machine: `~/.openclaw/credentials/gpn-release-key.txt` (mode `0600`). **Never in the repo, never in CI logs, never in a chat, never a GitHub Actions secret.** Only `scripts/release-cut.sh` (local, or local cron) reads it. | Release ops only |
| **Public** key | Compiled into every node: `gpn-agent/src/update.rs::RELEASE_PUBKEY_HEX` | Everyone |
| Fingerprint | `scripts/gpn-release-key.sh --show` → first 16 hex of `sha256(pubkey)` | Published with each release |

Consequences worth being explicit about:

* **A compromised web host is not enough to own the network.** An attacker who
  can serve a forged `releases.json` from `gpncoin.ai` cannot sign it, so nodes
  reject it and keep running.
* **Rotating the key needs a node release.** That is a feature: it means the
  trust root only changes when operators install something they chose to
  install.
* **Losing the private key** means: publish one more build signed with the
  *current* key (or a hand-verified download) carrying the *new* embedded public
  key, then switch. Never overwrite the embedded key silently.
* Back the key up offline (two copies, separate locations). It is a 32-byte
  secret; a printed hex string in a safe is a legitimate backup.

Create or inspect the key:

```bash
scripts/gpn-release-key.sh --show     # print public key + fingerprint
scripts/gpn-release-key.sh            # create if absent (never overwrites)
scripts/gpn-release-key.sh --force    # regenerate — BREAKS every deployed node
```

If you regenerate, you **must** update `RELEASE_PUBKEY_HEX` in
`gpn-agent/src/update.rs` and ship that build before it can receive any update.

---

## 5. Cutting a release

Three steps: **bump**, **dispatch the build workflows**, **cut**. The third one
is automated and idempotent — you can run it by hand, or let cron do it.

### 1. Bump — one level, with the change that earned it

```bash
scripts/bump-version.py minor
$EDITOR CHANGELOG.md                 # replace the TODOs; this text ships
scripts/bump-version.py --check      # all crates agree, version is strict semver
```

Land the bump in the same commit as the change. The build stamps the version and
git SHA **into the binary**, so an uncommitted tree ships under a SHA that does
not contain it.

### 2. Build — dispatch the two workflows

```bash
gh workflow run release-unix.yml    --repo jeremyzguo/gpn_coin
gh workflow run release-windows.yml --repo jeremyzguo/gpn_coin
```

They upload `gpn-linux.tar.gz` to release `linux-latest` and `gpn-windows.zip`
to `windows-latest` in the build repo (`jeremyzguo/gpn_coin`). Windows builds on
its own runner; see `windows-build-log.md`.

### 3. Cut — mirror, sign, publish, verify

```bash
scripts/release-cut.sh --dry-run     # what it would publish, touches nothing
scripts/release-cut.sh               # do it
```

That single command is the whole former 30-minute last mile:

1. **detects** whether the built assets are already the published ones —
   `already released 0.3.5 — nothing to do.` and exit 0 when they are, so it is
   safe to run on a timer;
2. **downloads** both bundles and verifies each sha256 against the digest
   GitHub reports — a truncated upload can never be signed. Interrupted runs
   resume from `~/.cache/gpn-release-cut` (the cached copy is pinned to the
   expected digest, so a new build never resumes onto an old partial file);
3. **reads the version out of the bundles** (`gpn-build-v1 version=…`); it never
   guesses. `--version X.Y.Z` is accepted as an *assertion* and a mismatch
   aborts before anything is uploaded;
4. **mirrors** both assets into the public repo `crewlyllc/gpn_coin`, renaming
   the previous asset aside first so a failed upload leaves a complete mirror
   rather than a 404 for every auto-updating node, then verifies the mirror's
   digest and deletes the superseded copies;
5. **signs** `website/releases.json` with the local ed25519 key and verifies its
   own signature against the key compiled into the node;
6. **publishes** (commits only `website/releases.json`, pushes `main`) and polls
   `https://gpncoin.ai/releases.json` until it serves the new version;
7. **verifies** by downloading each mirrored asset anonymously and comparing its
   sha256 to the signed manifest. Any mismatch fails loudly.

`min_version` is **carried over** from the published manifest unless you pass
`--min-version`, so an automated cut never silently cuts off a segment of the
fleet.

Useful flags: `--dry-run`, `--skip-mirror` (once the sync workflow mirrors the
assets itself), `--version`, `--min-version`, `--timeout N` (seconds to wait for
the live manifest, default 300).

It refuses to run at all if `website/releases.json` is dirty in the working
tree — that is somebody else's edit, and signing it would launder it.

**Cron does this for you.** An OpenClaw job runs `scripts/release-cut.sh` every
30 minutes in an isolated session and announces only when it publishes or
fails. So the normal flow is: dispatch the workflows, walk away, and get a
message when a signed release is live. Nothing is announced when there is
nothing new.

If you must do it by hand (a non-GitHub mirror, an unusual cutoff), the
underlying commands are still:

```bash
# digest READ from GitHub, never typed; --version is an assertion
scripts/gen-release-manifest.py \
    --bundle linux/x86_64=https://github.com/crewlyllc/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz \
    --bundle windows/x86_64=https://github.com/crewlyllc/gpn_coin/releases/download/windows-latest/gpn-windows.zip \
    --version 0.3.5 --min-version 0.1.0 \
    --key ~/.openclaw/credentials/gpn-release-key.txt \
    --out website/releases.json
scripts/commit-paths.sh -m "release: sign 0.3.5" website/releases.json && git push origin main
```

`layout: "bundle"` is written to the JSON but not into the signature (no
`--sign-layout`) so pre-bundle nodes can still verify the manifest.

### 4. Watch one node do the real thing before announcing

```bash
GPN_AUTO_UPDATE=1 GPN_UPDATE_CHECK_HOURS=1 journalctl --user -u gpn-node -f
# "downloading 0.3.5 (linux/x86_64, layout=bundle)" → "downloaded 317/302 MiB"
#   → "staged bundle 0.3.5 at …/versions/0.3.5" → "restarting to apply"
#   → "0.3.5 verified and switched in; previous build kept at …/versions/0.3.4"
gpn_node --check-updates      # read-only: manifest + asset digest, installs nothing
```

### Why signing stays on this machine, not in CI

The private key is deliberately **not** a GitHub Actions secret and the release
workflows cannot sign anything. A CI job can only produce an *unsigned* asset;
`release-cut.sh` refuses to sign one whose version stamp disagrees with what it
was told to publish, and it verifies the bytes it downloaded against the digest
GitHub recorded. Putting the key in Actions would trade that boundary for
convenience: one leaked repo secret, or one compromised third-party action,
could then sign a release that every auto-updating node installs. Build
infrastructure being writable by CI is acceptable; the trust root is not.

The cost is that cutting needs this box awake. That is why the automation is a
local cron job rather than a workflow — and why a completed build can no longer
sit unsigned for days.

**Do not** sign or publish a manifest over a bundle that does not match the
commit you built from: the signature makes it authoritative, and every
auto-updating node will install it. If a release is wrong, publish a new version
(a PATCH); do not re-sign the same one. In particular, if you bump the version
and forget to re-upload the bundle, the *node* catches it — the extracted build
stamp will not match the manifest and the update is refused — but only after
every node has downloaded 331 MB.

### Publishing without a GitHub release

For a non-GitHub mirror, declare the digest yourself — it is still verified on
the node:

```bash
scripts/gen-release-manifest.py \
  --bundle linux/x86_64=https://dl.example/gpn-linux.tar.gz \
  --bundle-digest linux/x86_64=$(sha256sum gpn-linux.tar.gz | cut -d' ' -f1):$(stat -c%s gpn-linux.tar.gz) \
  --version 0.2.0 --min-version 0.1.0 --out website/releases.json
```

### The pre-0.2.0 workflow (for reference)

This documented a hand-typed version and a hand-typed `min_version`:

```bash
scripts/gen-release-manifest.py \
    --version 0.2.0 \
    --min-version 0.1.5 \
    --asset linux/x86_64=dist/gpn_node \
    --asset windows/x86_64=dist/gpn_node.exe \
    --url-base https://gpncoin.ai/dl \
    --key ~/.openclaw/credentials/gpn-release-key.txt \
    --out website/releases.json
```

`--version` still exists, but only as an **assertion**: if it disagrees with
what the artifact was built from, the script now fails instead of publishing a
manifest nobody's binary matches (that mismatch — `0.2.0` in the manifest,
`0.1.0` in the binary — is what made the first auto-update a no-op). A
cutoff-only manifest (`--allow-empty-assets`) still needs `--version`, because
there is no artifact to read.

The script **verifies its own output before finishing** — it will not leave an
unsigned or mis-signed `releases.json` behind. Deploy `website/releases.json`
with the site (the Worker serves it at `/releases.json`; see
`worker/worker.js::releaseManifest`).

Ordering matters:

1. upload the **artifacts** first (so `url` resolves),
2. then upload `releases.json` (so no node ever sees a manifest pointing at a
   404),
3. `min_version` only rises **after** the new binaries are downloadable.

Raising `min_version` with `--allow-empty-assets` is supported, so a cutoff can
be published before (or without) a binary — useful for a coordinated upgrade
window, or to force everyone off a bad build.

---

## 6. Auto-update at setup (ON by default)

### At setup

`gpn_node --setup` asks once, after the ollama check. The prompt is **opt-out**:
the capital letter marks the default, and `Currently:` echoes what the config
says at that moment (a fresh node is **ON**):

```
  Keep this node auto-updated? [Y/n]
    yes: verify + install GPN releases automatically (signed manifest,
         ed25519), restarting briefly when one is published. Recommended:
         the network drops nodes that fall below the minimum version, and an
         old node cannot mine.
    no : you update manually. This node will REFUSE TO MINE once its
         version is below the published minimum (it keeps syncing).
  Currently: ON
  >
```

`n`/`no` writes `auto_update = false` into the config file; `y`/`yes` writes
`auto_update = true`; both survive restarts with no env var needed. Pressing
Enter changes nothing — it keeps the current value, which is **ON** unless you
previously opted out. With no TTY (systemd, CI) the question is skipped with
`auto-update: unchanged (ON)` rather than silently disabling updates. The same
opt-out is available as `GPN_AUTO_UPDATE=0` / `auto_update = false`.

### Later

```bash
# env (a single run)
GPN_AUTO_UPDATE=1 gpn_node

# or the config file (~/.gpn/node.toml)
auto_update = true
```

### What it actually does

On a **per-node randomized schedule** (see "The schedule" below) — first check
somewhere in the first `GPN_UPDATE_CHECK_HOURS` after start, then roughly every
`GPN_UPDATE_CHECK_HOURS` (default 6):

1. `GET https://gpncoin.ai/releases.json` — 15 s timeout, no retries.
2. **Verify the signature.** Wrong key, missing signature, tampered field, or
   unparseable JSON → log one line, do nothing else. There is no code path that
   downloads without a valid signature.
3. Compare `version` with this binary. Not newer → nothing to do (one log line
   only when it *is* newer).
4. Select the `os`/`arch` asset. None for this platform → log and stop.
4b. Wait a random `[0, GPN_UPDATE_SPREAD_SECS]` (default 20 min) **before
   downloading** — the install stagger, so release day does not turn into N
   simultaneous 300 MB pulls. Logged with the reason; `0` disables it.
5. Is it a **bundle** (`layout: "bundle"`, or a `.tar.gz`/`.zip` URL)?
   * **bundle** — stream it to a temp file, verify `sha256`+`size`, extract into
     `versions/.<version>.tmp-<pid>/`, prove `gpn_node` **and** `gpn-node` carry
     the `gpn-build-v1 version=<manifest version>` stamp (plus every per-file
     hash the manifest declares), then `rename` the directory to
     `versions/<version>`. A mismatch deletes the staging directory and stops.
   * **single** — download into memory, hash it, compare with the manifest's
     `sha256` (and `size` when declared). Mismatch → discard, log, stop.
6. Stage: bundle → the versioned directory (above); single →
   `<gpn_node>.new.tmp-<pid>` → `rename` → `<gpn_node>.new` (same directory as
   the binary, so a half-written file is never visible under a name we would
   exec).
7. Bundle: re-verify the version directory from disk immediately before the
   switch, then flip `current` with one atomic `rename`. Single: hash the staged
   file **from disk** and verify again, then `rename` it over the binary.
8. One line per action in the log. Then, when `GPN_UPDATE_RESTART` is on
   (default): stop the children cleanly, apply, re-exec from the new bin dir.

The log looks like this:

```
[supervisor 03:14:07] update: auto-update ON — checking https://gpncoin.ai/releases.json every 6h ±5–10% jitter, first check in 2h 41m (phase from account:5Grwva…; install spread 0–1200s; restart_to_apply=true)
[supervisor 05:55:09] update: downloading 0.2.0 (linux/x86_64, layout=bundle) sha256=… size=331643208
```

On release day the download is preceded by the stagger line:

```
[supervisor 05:55:08] update: 0.2.0 is available — waiting 7m 12s before downloading so the fleet does not stampede the release host (GPN_UPDATE_SPREAD_SECS=1200s, chosen for this node and version from its identity; 0 disables)
```

…and after each check the next wait is logged:

```
[supervisor 05:56:41] update: next check in 6h 24m 31s (+1471s)
```

#### The schedule (why not "every 6 hours from now")

A fleet of identical nodes installed from one image would otherwise check the
manifest in the same second and, on release day, pull the same 331 MB archive in
the same second. That is a self-inflicted DDoS on the release host, and it is
entirely avoidable without a server-side scheduler. Three independent pieces:

1. **Per-node phase.** The first check happens at `start + phase`, where

   ```
   seed  = sha256("gpn-update-phase-v1" NUL identity)     # identity = payout account, else base path
   phase = (seed[0..8] XOR (hash(seed, start_unix)[0..8] & 0xFFF)) mod interval
   ```

   `phase ∈ [0, interval)`, is stable for a given node (same identity + same
   start second → same phase), and covers the whole interval across a fleet. The
   restart term is only 12 bits of bounce, so a restart re-rolls the phase
   *lightly* instead of moving a node to the far side of the day. The identity is
   the payout account (public, unique per node, survives upgrades) or the base
   path when no account exists yet. The signer is never hashed: it may be a
   secret URI.

2. **Per-check jitter.** After each check the wait is

   ```
   jitter = ±[5 %, 10 %] of interval, from sha256("gpn-update-jitter-v1" seed counter)
   wait   = interval + jitter
   ```

   so nodes that happened to start together drift apart instead of staying
   locked in step forever.

3. **Install spread.** Finding a newer release does not start the download: the
   node waits `hash("gpn-update-spread-v1", seed, version) mod (spread + 1)`,
   i.e. uniform in `[0, GPN_UPDATE_SPREAD_SECS]` (default 1200 s = 20 min).
   Deterministic per `(node, version)`, so a retry after a failed download waits
   the same amount rather than re-rolling.

All of it runs on the auto-update thread, which is spawned after the supervisor
is already up: **a starting node never waits for the phase or the spread.** Both
sleeps are interruptible in 500 ms steps, so shutdown is prompt.

| Knob | Default | Meaning |
|---|---|---|
| `GPN_UPDATE_CHECK_HOURS` / `update_check_hours` | `6` | base interval, clamped to 1–168 h; the real wait is this ±5–10 % |
| `GPN_UPDATE_SPREAD_SECS` / `update_spread_secs` | `1200` | max delay between "update found" and starting the download (clamped to ≤ 86400); `0` disables the stagger |
| `GPN_UPDATE_RESTART` | `1` | apply immediately (short outage) vs. on the next start |

With 100 simulated nodes the phase schedule occupies 87 distinct one-minute
buckets of a 6 h interval, with no minute ever holding more than 5 % of the
fleet — versus all 100 in the same second without it.

### The restart is a short outage — by design

Applying a new binary means executing new code, and there is no way to do that
without a restart. It is deliberately the *smallest possible* one: children are
stopped gracefully (not killed mid-job; a running job is finished or released as
on any shutdown), the swap is a single atomic `rename`, and the supervisor
re-execs itself immediately. Expect seconds, not minutes. Chain sync resumes from
the last block — no re-sync.

If you would rather choose the moment:

```bash
GPN_UPDATE_RESTART=0   # download + verify + stage now, apply on the next start
```

Then a normal restart (or a service restart) applies it.

### Opting out / pausing

| Want | Do |
|---|---|
| Never auto-update | set `auto_update = false` or `GPN_AUTO_UPDATE=0` |
| Pause for a while | the same — it is checked at start, so `GPN_AUTO_UPDATE=0` disables the thread |
| Air-gapped / no manifest access | `GPN_UPDATE_CHECK=0` — disables *all* manifest fetches. ⚠ the cutoff cannot be evaluated either, so the node trusts its ≤ last verified state |
| Keep checking but never restart | `GPN_UPDATE_RESTART=0` |

---

## 6b. Every deployment mode: `gpn_update`

Auto-update used to exist **only inside `gpn_node`**. That is fine for hosts
started by the installer, but a host can run the same binaries a different way:

| Deployment | What starts | Was it ever checked? |
|---|---|---|
| installer / systemd | `gpn-node.service` → `gpn_node` | yes — the supervisor's own thread |
| fleet | `run-fleet.sh` → `gpn_fleet` | **no** — froze at the tarball's version |
| hand-run / container / process manager | `gpn-node`, `gpn_scheduler`, … | **no** |
| Windows zip | `gpn_node.exe` | yes |

Replacing the fleet with `gpn_node` is not an option: the fleet carries the
fleet directive, `GPN_MINER_ACCOUNT` and the challenge-verifier secrets. So the
check is its own binary, shipped in every bundle and installer alongside the
node:

```text
gpn_update --check    # default: fetch + verify the manifest, download NOTHING
gpn_update --apply    # download, verify, install, copy, (optionally) restart
```

It reuses `gpn_agent::update` — the same manifest parser, the same ed25519
verification against the compiled-in release key, the same size+sha256 checks,
the same `versions/<v>/` extraction and `current` flip. Nothing about
verification is re-implemented.

### `--check` (read-only)

```text
$ gpn_update --check
  current      0.3.11   /home/me/.local/bin/gpn_node
  latest       0.3.12   published 2026-11-02T18:00:00Z
  min_version  0.3.11   (supported)
  asset        linux/x86_64 layout=bundle 317.1 MB sha256=a300453a…
  -> update available: 0.3.12 (run `gpn_update --apply`)
```

The version on the left is read from the **installed** `gpn_node` build stamp
(falling back to the updater's own version), so an updater that is itself one
release ahead still reports what the box is actually running.

| Exit | Meaning |
|---|---|
| `0` | up to date |
| `1` | error: bad signature/key, no asset for this platform, I/O |
| `2` | the manifest could not be fetched, or the body was not a manifest (offline, DNS, HTTP ≠ 2xx, captive portal) |
| `3` | a **newer verified** release exists — the scriptable "update me" signal |

### `--apply`

1. Fetch + verify the manifest (same checks as above).
2. If the version is not newer → exit `0`, nothing touched.
3. Refuse when `GPN_AUTO_UPDATE=0` → exit `3`, **no download**.
4. Download the asset to a temp file whose extension comes from the signed URL
   (`.tar.gz`/`.tgz`/`.tar`/`.zip`), verify `sha256`+`size`, extract to
   `versions/<v>/`, prove the build stamps say `<v>`.
5. Flip `current`, keep `current.previous`, prune older version directories.
6. Copy every `gpn*` file from `versions/<v>/` into the install dir
   (`GPN_UPDATE_TARGET_DIR`, default `~/.local/bin`), **one atomic rename per
   file** (`.<name>.tmp-<pid>` → `<name>`). A crash leaves either the old
   file or the complete new one — never a half-written executable.
7. Run `GPN_UPDATE_RESTART_CMD` if set; otherwise print the command to run.
   **Nothing is restarted by default.**

`--json` prints one object; `--quiet` prints nothing (the timer uses it).

First run on a host that has never been updated: the binaries in the install dir
are snapshotted into `versions/<running>/` before the flip, so even the first
update has a real rollback target instead of a one-way door.

If the copy step fails, the tree and `current` are already new but the install
dir is mixed. Re-run `gpn_update --apply`: it re-verifies and reuses the tree
instead of downloading again, then finishes the copy.

### The timer the installer ships

`scripts/gpn-autostart.sh install` writes four user units: `gpn-ollama.service`,
`gpn-node.service`, `gpn-update.service` and `gpn-update.timer`.
(`install-updater` writes only the last two — see "Enabling it on a
`gpn_fleet` host" below.)

```ini
# ~/.config/systemd/user/gpn-update.service
[Unit]
Description=GPN signed-release updater (one check + apply)
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
Environment=GPN_UPDATE_DIR=/home/me/.gpn/update
Environment=GPN_UPDATE_TARGET_DIR=/home/me/.local/bin
Environment="GPN_UPDATE_RESTART_CMD=systemctl --user restart gpn-node.service"
Nice=10
SuccessExitStatus=0 2 3
ExecStart=/home/me/.local/bin/gpn_update --apply --quiet

[Install]
WantedBy=default.target
```

```ini
# ~/.config/systemd/user/gpn-update.timer
[Unit]
Description=Check for GPN releases every 6 hours (randomized across the fleet)

[Timer]
OnBootSec=10min
OnUnitActiveSec=6h
RandomizedDelaySec=30min
AccuracySec=1min
Persistent=true
Unit=gpn-update.service

[Install]
WantedBy=timers.target
```

`SuccessExitStatus=0 2 3` is deliberate: being offline (`2`) or holding at
`GPN_AUTO_UPDATE=0` (`3`) is not a unit failure; only a real error (`1`) is.
`RandomizedDelaySec` is what stops a fleet from pulling 300 MB in lockstep.

### The timer the Windows installer ships (`GPN-Update`)

Windows has no systemd, and a Windows node does **not** keep checking the
manifest between starts the way a Linux one does — a box that never reboots can
sit on an old build for days (that is exactly what box 1 did). `install.ps1`
therefore registers a scheduled task, `GPN-Update`, that is the Windows
equivalent of `gpn-update.timer`:

| | Value |
|---|---|
| Task name | `GPN-Update` |
| Cadence | every **6 hours**, with `<RandomDelay>PT30M</RandomDelay>` (jitter is drawn per repetition, so the fleet drifts apart) |
| Action | `powershell.exe -NoProfile -ExecutionPolicy Bypass -File "C:\gpn\update-and-restart.ps1"` |
| Concurrency | `MultipleInstancesPolicy = IgnoreNew` **and** an exclusive lock file `C:\gpn\update.lock` |
| Missing a run (box off) | `StartWhenAvailable = true` |
| Run level | `HighestAvailable`, as the installing user (`InteractiveToken`) |

The task is registered from XML (`schtasks /Create /TN GPN-Update /XML ... /F`)
and not from `schtasks /SC DAILY /RI 360`: the CLI cannot express jitter, and
the ScheduledTasks module's `-RandomDelay` only dithers the *initial*
StartBoundary, not every repetition. `/F` replaces the task by name, so running
the installer twice never creates a second task or a duplicate trigger.

#### The wrapper's control flow

```text
update check started
  gpn_update --check                 -> 0  up to date        -> log, exit 0   (node NOT restarted)
                                     -> 2  manifest offline  -> log, exit 0   (node NOT restarted)
                                     -> 3  update available  -> continue
                                     -> 1  verification error-> log, message box, exit 1
  schtasks /End /TN GPN-Node         (Windows locks a running .exe, so the swap
                                      cannot succeed while the node runs)
  wait up to 60 s for gpn* to exit; force-stop the stragglers if it does not
  gpn_update --apply                 (downloads, verifies, swaps, copies to C:\gpn\bin)
  schtasks /Run /TN GPN-Node         ALWAYS - even if --apply failed
  only then, if --apply failed, the message box explaining why
```

Three rules are load-bearing:

* **Stop before apply.** The updater reports a locked `.exe` as an error; the
  wrapper prevents the case instead of relying on the error.
* **Always restart.** A failed update must never leave a box with nothing
  running. The restart happens *before* any dialog, so a modal message box
  cannot be the reason the node stays down.
* **No restart when there is nothing to do.** A 6-hourly bounce for no reason
  looks like instability; exits `0` and `2` return without touching the node.

The restart window is the time the supervisor takes to come back — typically
**10-30 s** (children stop gracefully, chain sync resumes from the last block,
no re-sync). `gpn_update --apply` downloads the ~225 MB Windows bundle first,
so a release day cycle takes minutes, not seconds; the node is only down for
the swap itself.

#### Checking what happened

```powershell
Get-Content C:\gpn\update.log -Tail 40
schtasks /Query /TN GPN-Update /V /FO LIST   # last run time + last result
schtasks /Run  /TN GPN-Update                # run a cycle now
```

`C:\gpn\update.log` is append-only and always ends with either
`up to date - nothing to do (node NOT restarted)`, `update cycle complete`, or
`FAILED: ...` with the reason. When an update fails, the same text is shown in
a message box (via the generated `report-exit.ps1`, the only channel a hidden
task has) and the node is already back up.

Update state lives in `C:\gpn\data\update\` (the same `versions/`, `current`,
`current.previous` layout the node itself uses); binaries are installed into
`C:\gpn\bin`. Removing the auto-update is part of the normal uninstall:

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall
```

That stops and deletes `GPN-Node` and `GPN-Update`, and removes
`update-and-restart.ps1` (the log and the chain data are kept).

Existing boxes heal themselves: the release deploy step runs
`install.ps1 -UpdateTaskOnly`, which writes the wrapper and registers the task
without re-copying binaries, touching ollama, or restarting the node. It is
guarded, so a checkout without `install.ps1` only warns.

The supervisor still has its own in-process auto-update thread. Both paths use
the same signed manifest, the same `versions/<v>/` trees and the same `current`
junction, and both are idempotent (an already-installed version is reused, not
re-downloaded), so they do not fight. If you want exactly one updater on a
Windows box, set `auto_update = false` in `%USERPROFILE%\.gpn\node.toml` and let
`GPN-Update` own it.

### Enabling it on a `gpn_fleet` host

A fleet host's node is owned by `gpn-fleet.service`, not by the installer's
`gpn-node.service`, so `install` would start a **second** supervisor against the
same base path. Use the updater-only action — it writes just the two units and
picks `gpn-fleet.service` as the restart hook when that unit exists:

```bash
# gpn_update ships in every release bundle; it must sit next to gpn_node
scripts/gpn-autostart.sh install-updater --prefix "$HOME/.gpn" --bin-dir "$HOME/.local/bin"
```

On this repo checkout the binary is `gpn-agent/target/release/gpn_update`; copy
it into the bin dir first if the bundle did not (that is also how it gets
upgraded afterwards).

The generated unit then carries exactly these three `Environment=` lines:

```ini
[Service]
Environment=GPN_UPDATE_DIR=%h/.gpn/update
Environment=GPN_UPDATE_TARGET_DIR=%h/.local/bin
Environment="GPN_UPDATE_RESTART_CMD=systemctl --user restart gpn-fleet.service"
```

(Write them by hand in `~/.config/systemd/user/gpn-update.service` if you prefer
— `install-updater` only generates that file plus the timer shown above.)

Two details that matter on a fleet box:

* `GPN_UPDATE_RESTART_CMD` must name the unit that owns `gpn_fleet`. If the
  fleet is started from a shell or a different unit, **leave it unset** — the
  updater then installs and prints the command instead of guessing, and a wrong
  guess cannot take the network down. `install-updater` leaves it unset when it
  finds neither `gpn-fleet.service` nor `gpn-node.service`.
* `GPN_UPDATE_DIR` is separate from the fleet's base path (`~/.gpn/node`) on
  purpose: the updater owns `versions/`, `current` and `current.previous`, and
  must not be confused with the chain database next door.

Verify without changing anything:

```bash
~/.local/bin/gpn_update --check; echo "exit=$?"
systemctl --user list-timers gpn-update.timer
journalctl --user -u gpn-update.service -n 50
```

Undo just the updater (never touches the node/fleet units):

```bash
scripts/gpn-autostart.sh remove-updater
```

---

## 7. The minimum-version cutoff

Three layers, weakest to strongest. Only the first is implemented in this
release; the other two are **designed and staged** (see
`docs/design/design-min-node-version.md`).

### 5.1 Node self-check — **SHIPPED**

The supervisor checks the signed manifest on **every start**, whether or not
auto-update is on (a manual updater still needs to be told). If
`version < min_version`:

```
  ============================================================
  !! UPGRADE REQUIRED — this node is 0.3.9, the network requires >= 0.3.11
  ============================================================
  This node will SYNC (so its data stays useful) but will NOT MINE and
  will NOT ANNOUNCE itself. An out-of-date node can otherwise produce blocks
  the rest of the network rejects — and that looks like a chain stall.
  Fix: install the current release from https://gpncoin.ai/downloads
       (or set GPN_AUTO_UPDATE=1 and restart — it updates itself).
```

Mechanically, in standby the supervisor launches the chain node with
`--no-seed --no-discoverable` and `GPN_STANDBY=1`, and `gpn-node` downgrades its
role from `Authority` to `Full`, so **neither the block proposer nor the miner
starts**. It still syncs, still serves its RPC, and can be upgraded in place.

Why it must be this way: a stale node that keeps mining does not "keep the chain
alive" — it produces blocks with an outdated runtime/schema that the upgraded
majority will reject, forcing reorgs and confusing every explorer. Syncing-only
is the useful, honest state.

The cutoff survives being offline: the last **verified** manifest is cached in
`<base>/update/update-state.json`, and that `min_version` keeps applying when the
fetch fails. A node never "forgets" it is too old just because the network is
down.

Escape hatch: `GPN_MIN_VERSION_ENFORCE=0` (or `min_version_enforce = false`)
mines anyway, with a loud warning. It exists for one scenario — GPN publishes a
bad `min_version` and needs to unwind it — and it is **not** a supported
configuration: blocks such a node seals may be rejected by upgraded peers.

### 5.2 Peer/network — **POLICY (staged)**

libp2p peers only talk to peers speaking the same protocol id. When a change is
consensus- or wire-incompatible, bump the network protocol id/version so
incompatible peers are **dropped naturally** instead of half-working:

* **Bump the protocol id** when: block format, extrinsics, RPC the scheduler
  depends on, or the p2p message set changes; anything where an old peer would
  serve data a new peer misinterprets.
* **Do not bump** for a runtime-only change: those ship forklessly via
  `set_code`, and old/new *nodes* interoperate fine while the runtime is
  upgraded on-chain at a known block.
* Bumping is the *network* half of the cutoff: even a node that ignores
  `min_version` (or uses the escape hatch) ends up isolated, which is the safe
  failure mode — no peers, no propagation, no damage.

Rollout: bump in the release that raises `min_version`, never before, so the two
mechanisms agree. See `docs/design/design-min-node-version.md` §3.

### 5.3 On-chain `MinNodeVersion` — **STAGED, NOT APPLIED**

The durable version: a `MinNodeVersion` storage item in the runtime, settable by
governance, readable by every node. That makes the cutoff part of consensus
rather than of a web server, and lets a node prove to itself (and to anyone
auditing) what the network requires.

**Nothing is applied.** The design (storage item, governance call, migration,
WASM upgrade path, review gate) is in
`docs/design/design-min-node-version.md`, and the staged-artifact pointer is
`wasm/README-pending-upgrade.md`. Applying it means building a spec-42 WASM and
submitting `sudo(System.set_code)` — a review-first, chain-wide action that is
explicitly **out of scope** for this change. The canonical genesis WASM is
untouched.

---

## 8. Rollback

A bundle install keeps the **previous complete version directory** and the
pointer to it, so a rollback is one link flip — no binary copying, nothing to
download.

```bash
ls -l ~/.gpn/node/update/          # current -> versions/0.3.0
cat ~/.gpn/node/update/current.previous     # the rollback target
```

(`gpn_update` deployments: the same files under `~/.gpn/update/`. To roll a fleet
box back, flip that link **and** re-run `gpn_update`'s copy step by re-applying
the old manifest, or copy `versions/<old>/gpn*` over the install dir by hand.)

```bash
systemctl --user stop gpn-node

# Flip `current` back to the previous version directory (stop the node first:
# on Windows a junction under a running image cannot be replaced).
ln -sfn "$(dirname $(readlink -f ~/.gpn/node/update/current))" /tmp/prev
rm ~/.gpn/node/update/current
ln -s "$(cat ~/.gpn/node/update/current.previous)" ~/.gpn/node/update/current

# make sure it stays: pin the version until the fix ships
#   auto_update = false   (~/.gpn/node.toml)

systemctl --user start gpn-node
~/.gpn/node/update/current/gpn_node --version     # the old version, again
```

Windows (PowerShell, **not** `ln`):

```powershell
Stop-Service gpn-node
Remove-Item C:\gpn\data\update\current            # removes the junction only
New-Item -ItemType Junction -Path C:\gpn\data\update\current `
         -Target (Get-Content C:\gpn\data\update\current.previous)
Start-Service gpn-node
```

Practical notes:

* The rollback target is **one step deep** — `current.previous` names the build
  that was live before the last switch. Older version directories are pruned
  after a successful switch (current + previous are kept, ~660 MB total).
* Each version directory is a *complete* tree, so the supervisor and every child
  it spawns always come from the same version. There is no "half-rolled-back"
  state.
* If a release was bad, GPN publishes a new `releases.json` with a fixed
  `version` (and leaves `min_version` alone) — auto-updaters recover on their
  next check, which is why rollback is a stopgap rather than a strategy.
* Rolling back below `min_version` puts the node in standby (sync-only). That is
  intentional: roll back to a version that is still supported, or accept
  sync-only until the fix ships.
* A **single-layout** install (the pre-bundle shape, or a manifest that points at
  a lone binary) still keeps `<gpn_node>.bak` next to the binary: `mv` it back.

### `--check-updates`

Read-only. Fetches the manifest, verifies the signature, applies the cutoff logic
and prints the verdict — no download, no install, no children started:

```bash
$ gpn_node --check-updates
  updates: auto=off (manual) every 6h restart_to_apply=true manifest=https://gpncoin.ai/releases.json
  update: manifest verified (https://gpncoin.ai/releases.json): latest=0.2.0 min=0.1.5

  result: this node (0.1.0) is supported — it would mine
```

This is the first thing to run when a node stops mining unexpectedly: it
distinguishes "below the cutoff" from "no peers", "no ollama", or a stuck miner.

### If an update fails

| Symptom | Meaning | Action |
|---|---|---|
| `manifest signature does NOT verify against the GPN release key` | Tampered file, or a key rotation shipped without a node update | Do **not** bypass. Report it. The node stays on its current build. |
| `manifest is signed by key …, but this node only trusts …` | Key rotated | Upgrade by hand once, then auto-update resumes |
| `downloaded file hash … does not match` | Truncated/corrupted download, or a mismatched artifact | Transient — it retries next cycle. Persistent → the published artifact and manifest disagree; tell GPN. |
| `staged … could not be swapped in while running` | Windows locks a running `.exe` | Normal: applied at the next service start |
| `staged update REJECTED at swap time` | The staged file changed on disk between download and swap | Discarded; the old binary keeps running |
| `release manifest not published` (HTTP 503) | No release published yet | Nothing to do |

---

## 9. Reference

### Env vars

| Var | Default | Meaning |
|---|---|---|
| `GPN_AUTO_UPDATE` | `1` | **on by default**; `0`/`false`/`no`/`off` = opt **out** (manual updates) |
| `GPN_UPDATE_URL` | `https://gpncoin.ai/releases.json` | manifest URL |
| `GPN_UPDATE_CHECK_HOURS` | `6` | interval, clamped to 1–168 h |
| `GPN_UPDATE_RESTART` | `1` | `0` = stage now, apply at the next start |
| `GPN_UPDATE_CHECK` | `1` | `0` = never fetch the manifest (air-gapped) |
| `GPN_MIN_VERSION_ENFORCE` | `1` | `0` = mine even when below `min_version` (unsupported) |
| `GPN_UPDATE_DIR` | `$GPN_PREFIX/update`, else `~/.gpn/update` | `gpn_update`: where `versions/`, `current`, `current.previous` live |
| `GPN_UPDATE_TARGET_DIR` | `~/.local/bin` | `gpn_update`: install dir whose `gpn*` binaries are replaced |
| `GPN_UPDATE_RESTART_CMD` | *(unset)* | `gpn_update`: run after a successful swap, e.g. `systemctl --user restart gpn-fleet`. Unset = print, never restart |

Config-file equivalents (`~/.gpn/node.toml`): `auto_update`, `update_url`,
`update_check_hours`, `update_restart`, `update_check`, `min_version_enforce`.

### Files

| Path | Contents |
|---|---|
| `<base>/update/pending-update.json` | the staged marker: version, sha256, size, layout, version dir, per-file hashes |
| `<base>/update/update-state.json` | last verified `version`/`min_version`, last check time (drives the offline cutoff) |
| `<base>/update/versions/<v>/` | one **complete** release tree per installed version |
| `<base>/update/current` | symlink → the live `versions/<v>` (a junction on Windows) |
| `<base>/update/current.previous` | the rollback target (`versions/<old>`) |
| `<base>/update/.<v>.tmp-<pid>/` | extraction in progress; never visible as an install |
| `<bin>/gpn_node.new` | single-layout staging (only between staging and swap) |
| `<bin>/gpn_node.bak` | single-layout rollback copy of the previous binary |

`gpn_update` uses `GPN_UPDATE_DIR` (default `~/.gpn/update`) as `<base>` above;
the supervisor uses its node base path (`~/.gpn/node/update`). Both layouts are
the same on disk.

### Code

| Path | Role |
|---|---|
| `gpn-agent/src/update.rs` | manifest parse + signature verify, version compare, cutoff decision, **bundle extraction + stamp verification + versioned staging + atomic `current` switch + rollback**, cached state, the embedded public key |
| `gpn-agent/build.rs` | injects `GPN_RUNTIME_SPEC_VERSION`, `GPN_GIT_SHA` and the `GPN_BUILD_ID` stamp |
| `gpn-agent/src/firstrun.rs` | `version_line()` → `gpn_node <ver> (spec <n>, <sha>)` |
| `scripts/bump-version.py` | the only writer of the version (+ `CHANGELOG.md` stub) |
| `CHANGELOG.md` | one heading per released version, newest first |
| `gpn-agent/src/bin/gpn_node.rs` | opt-in policy, startup check, updater thread, restart, `--setup` question |
| `gpn-agent/src/bin/gpn_update.rs` | standalone updater for every other deployment mode (`--check`/`--apply`, exit 3/2/1, atomic per-file install, restart hook) |
| `gpn-node/node/src/service.rs` | `GPN_STANDBY` → no proposer, no miner |
| `scripts/gen-release-manifest.py` | build + sign + verify the manifest (single binaries **and** bundles; digests read from the GitHub API, never typed) |
| `scripts/gpn-release-key.sh` | create/inspect the release key |
| `worker/worker.js` | `GET /releases.json` (headers, 503 when unpublished) |
| `docs/design/design-min-node-version.md` | the staged on-chain cutoff proposal |
