# Updates — auto-update, the minimum-version cutoff, and release signing

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
| **Private** key (32-byte ed25519 seed) | Offline with GPN release operations. Default on an ops machine: `~/.openclaw/credentials/gpn-release-key.txt` (mode `0600`). **Never in the repo, never in CI logs, never in a chat.** | Release ops only |
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

## 5. Publishing a release: the checklist

Every step is required; each one has a failure mode that only shows up on an
operator's machine.

```bash
# 1. BUMP — one level, with the change that earned it (docs above).
scripts/bump-version.py minor
$EDITOR CHANGELOG.md                 # replace the TODOs; this text ships
scripts/bump-version.py --check      # all crates agree, version is strict semver

# 2. BUILD — from a CLEAN tree at the commit you will tag. The version and SHA
#    baked into the artifact come from here, not from the manifest command.
git status --porcelain               # must be empty — the stamp records the
                                     # COMMIT, so uncommitted changes would ship
                                     # under a SHA that does not contain them
cargo build --release --manifest-path gpn-node/Cargo.toml -p gpn-node
cargo build --release --manifest-path gpn-agent/Cargo.toml --bins
gpn-agent/target/release/gpn_node --version   # gpn_node 0.2.0 (spec 41, <sha7>)

#    Windows: build on the Windows box (run-windows-provider.ps1 /
#    windows-build-log.md) and copy the .exe into dist/. It cannot be built
#    cross-platform here, and it is stamped the same way.

# 3. BUNDLE — one archive per platform, flat at the root, the way
#    `.github/workflows/release-{unix,windows}.yml` builds it.
#    A bundle must contain BOTH gpn_node (the supervisor) and gpn-node.
stage=/tmp/gpn-linux-stage; rm -rf $stage; mkdir -p $stage/scripts
cp gpn-agent/target/release/gpn_* gpn-node/target/release/gpn-node $stage/
cp scripts/install-ollama*.sh $stage/scripts/
cp gpn-node.example.toml README-FIRST-RUN.txt $stage/
chmod +x $stage/gpn_* $stage/scripts/*.sh
(cd $stage && tar -czf gpn-linux.tar.gz .)      # flat: ./gpn_node, ./gpn-node, …

# 4. UPLOAD THE BUNDLE FIRST. The manifest must never point at a 404 — an
#    auto-updating fleet would all fail the download in the same minute.
#    Tag: linux-latest / windows-latest (rolling), or vX.Y.Z.
gpn-agent/target/release/gpn_node --version    # gpn_node 0.2.0 (spec 41, <sha7>)

# 5. MANIFEST — the digest is READ FROM GITHUB, never typed:
#      --bundle os/arch=URL   → sha256 + size from the release API
#      --bundle os/arch=PATH  → computed locally, and the build stamp is read
#                               out of the archive to cross-check --version
#    A remote bundle cannot be introspected, so --version is an assertion here;
#    the node still proves the version from the extracted stamps before switching.
scripts/gen-release-manifest.py \
    --bundle linux/x86_64=https://github.com/crewlyllc/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz \
    --bundle windows/x86_64=https://github.com/crewlyllc/gpn_coin/releases/download/windows-latest/gpn-windows.zip \
    --version 0.2.0 --min-version 0.1.0 \
    --key ~/.openclaw/credentials/gpn-release-key.txt \
    --out website/releases.json
#    `layout: "bundle"` is written to the JSON for consumers, but NOT into the
#    signature (no --sign-layout): a pre-bundle node has to be able to verify
#    this manifest, or the fleet can never update. Add --sign-layout from 0.3.0.
#    The script prints the digest it used and re-verifies the signature it just
#    produced against the key compiled into the node. If it says OK, publish.
#    Optional: --bundle-files os/arch=<unpacked dir> adds per-file hashes on
#    top of the build-stamp check (needs --sign-layout).

# 6. PUBLISH — artifacts FIRST, manifest SECOND.
#    commit website/releases.json to main; the Cloudflare deploy serves it at
#    https://gpncoin.ai/releases.json (worker/worker.js::releaseManifest)

# 7. VERIFY — read-only, from a machine that is NOT the release box.
gpn_node --check-updates
#    expect: manifest verified … latest=0.2.0 min=0.1.0
#            update: asset linux/x86_64 layout=bundle 331643208 bytes sha256=…
#            result: this node (0.2.0) is supported — it would mine
#                    — installed bundle: (none — not installed from a bundle)
curl -s https://gpncoin.ai/releases.json | head -3
#    and check the asset's sha256 matches the release page

# 8. WATCH ONE NODE do the real thing before announcing:
#    GPN_AUTO_UPDATE=1 GPN_UPDATE_CHECK_HOURS=1 journalctl --user -u gpn-node -f
#    "downloading 0.2.0 (linux/x86_64, layout=bundle)" → "downloaded 331/316 MiB"
#      → "staged bundle 0.2.0 at …/versions/0.2.0" → "restarting to apply"
#      → "0.2.0 verified and switched in; previous build kept at …/versions/0.1.0"
```

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

## 6. Opting in to auto-update

### At setup

`gpn_node --setup` asks once, after the ollama check:

```
  Keep this node auto-updated? [y/N]
    yes: verify + install GPN releases automatically (signed manifest,
         ed25519), restarting briefly when one is published. Recommended:
         the network drops nodes that fall below the minimum version, and an
         old node cannot mine.
    no : you update manually. This node will REFUSE TO MINE once its
         version is below the published minimum (it keeps syncing).
  Currently: off
  >
```

`y`/`yes` writes `auto_update = true` into the config file (the answer survives
restarts; no env var needed). Anything else — including just pressing Enter — is
**no**.

### Later

```bash
# env (a single run)
GPN_AUTO_UPDATE=1 gpn_node

# or the config file (~/.gpn/node.toml)
auto_update = true
```

### What it actually does

On start (after 45 s) and every `GPN_UPDATE_CHECK_HOURS` (default 6):

1. `GET https://gpncoin.ai/releases.json` — 15 s timeout, no retries.
2. **Verify the signature.** Wrong key, missing signature, tampered field, or
   unparseable JSON → log one line, do nothing else. There is no code path that
   downloads without a valid signature.
3. Compare `version` with this binary. Not newer → nothing to do (one log line
   only when it *is* newer).
4. Select the `os`/`arch` asset. None for this platform → log and stop.
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
[supervisor 03:14:07] update: auto-update ON — checking https://gpncoin.ai/releases.json every 6h (restart_to_apply=true)
[supervisor 03:14:09] update: downloading 0.2.0 (linux/x86_64, layout=bundle) sha256=… size=331643208
[supervisor 03:14:41] update: downloaded 320/316 MiB
[supervisor 03:14:44] update: staged bundle 0.2.0 at /home/you/.gpn/node/update/versions/0.2.0 — archive sha256 verified, build stamps matched, previous version kept for rollback
[supervisor 03:14:44] update: restarting to apply 0.2.0 — this is a SHORT OUTAGE: children stop cleanly, the binary is swapped atomically and the node comes back up (chain sync resumes)
[supervisor 03:14:47] update: 0.2.0 verified and switched in; previous build kept at /home/you/.gpn/node/update/versions/0.1.0 (rollback: point current at it and restart); re-exec into /home/you/.gpn/node/update/versions/0.2.0/gpn_node — short outage
```

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
  !! UPGRADE REQUIRED — this node is 0.1.0, the network requires >= 0.2.0
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

### Code

| Path | Role |
|---|---|
| `gpn-agent/src/update.rs` | manifest parse + signature verify, version compare, cutoff decision, **bundle extraction + stamp verification + versioned staging + atomic `current` switch + rollback**, cached state, the embedded public key |
| `gpn-agent/build.rs` | injects `GPN_RUNTIME_SPEC_VERSION`, `GPN_GIT_SHA` and the `GPN_BUILD_ID` stamp |
| `gpn-agent/src/firstrun.rs` | `version_line()` → `gpn_node <ver> (spec <n>, <sha>)` |
| `scripts/bump-version.py` | the only writer of the version (+ `CHANGELOG.md` stub) |
| `CHANGELOG.md` | one heading per released version, newest first |
| `gpn-agent/src/bin/gpn_node.rs` | opt-in policy, startup check, updater thread, restart, `--setup` question |
| `gpn-node/node/src/service.rs` | `GPN_STANDBY` → no proposer, no miner |
| `scripts/gen-release-manifest.py` | build + sign + verify the manifest (single binaries **and** bundles; digests read from the GitHub API, never typed) |
| `scripts/gpn-release-key.sh` | create/inspect the release key |
| `worker/worker.js` | `GET /releases.json` (headers, 503 when unpublished) |
| `docs/design/design-min-node-version.md` | the staged on-chain cutoff proposal |
