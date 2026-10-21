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

## 1. The release manifest

`https://gpncoin.ai/releases.json`

```json
{
  "version": "0.2.0",
  "min_version": "0.1.5",
  "published_at": "2026-10-21T17:00:00Z",
  "assets": [
    {
      "os": "linux",
      "arch": "x86_64",
      "url": "https://gpncoin.ai/dl/gpn_node-0.2.0-linux-x86_64",
      "sha256": "<64 hex>",
      "size": 31457280
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
| `signing_key` | The key that produced `signature`. Must equal the key compiled into the node, otherwise the manifest is rejected **before anything is downloaded**. |
| `signature` | ed25519 over the canonical text below. |

### What exactly is signed

**Not the JSON.** The signature covers a canonical, line-oriented text blob, so
the signer (Python/PyNaCl) and the verifier (Rust/ed25519-dalek) cannot disagree
about JSON formatting, key order or whitespace:

```text
gpn-release-v1
version=0.2.0
min_version=0.1.5
published_at=2026-10-21T17:00:00Z
signing_key=<64 hex, lower-case>
asset=linux/x86_64 https://gpncoin.ai/dl/gpn_node-0.2.0-linux-x86_64 <sha256> 31457280
asset=windows/x86_64 https://gpncoin.ai/dl/gpn_node-0.2.0-win-x86_64.exe <sha256> 32112640
```

Rules (both sides implement them; `gpn-agent/src/update.rs::canonical_text` is
the reference implementation):

* assets are sorted by `(os, arch, url)` — file order does not matter;
* every line ends with `\n`, including the last;
* the two hex fields are lower-cased before hashing;
* the field names and `gpn-release-v1` prefix are load-bearing. Changing them is
  a manifest format change and needs a node release.

This binds **everything**: the version, the cutoff, the assets and their hashes.
Editing a single character of `min_version` or of an asset's `sha256` invalidates
the signature.

---

## 2. Key custody

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

## 3. Publishing a release (release ops)

```bash
# 1. build the artifacts (per platform; do NOT regenerate the canonical
#    gpn_runtime.compact.compressed.wasm — the embedded genesis WASM is immutable)

# 2. sign + publish the manifest
scripts/gen-release-manifest.py \
    --version 0.2.0 \
    --min-version 0.1.5 \
    --asset linux/x86_64=dist/gpn_node \
    --asset windows/x86_64=dist/gpn_node.exe \
    --url-base https://gpncoin.ai/dl \
    --key ~/.openclaw/credentials/gpn-release-key.txt \
    --out website/releases.json

# 3. verify what you just wrote (what every node will do)
scripts/gen-release-manifest.py --verify website/releases.json
```

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

## 4. Opting in to auto-update

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
5. Download it, hash it **in memory**, compare with the manifest's `sha256`
   (and `size` when declared). Mismatch → discard, log, stop.
6. Stage it as `<gpn_node>.new.tmp-<pid>` → `rename` → `<gpn_node>.new`
   (same directory as the binary, so the final swap is an atomic rename and a
   half-written file is never visible under a name we would exec).
7. Hash the staged file **from disk** and verify again.
8. One line per action in the log. Then, when `GPN_UPDATE_RESTART` is on
   (default): stop the children cleanly, swap the binary, re-exec.

The log looks like this:

```
[supervisor 03:14:07] update: auto-update ON — checking https://gpncoin.ai/releases.json every 6h (restart_to_apply=true)
[supervisor 03:14:09] update: downloading 0.2.0 (linux/x86_64) sha256=… size=31457280
[supervisor 03:14:14] update: staged 0.2.0 at /home/you/.local/bin/gpn_node.new — signature OK, sha256 verified twice
[supervisor 03:14:14] update: restarting to apply 0.2.0 — this is a SHORT OUTAGE: children stop cleanly, the binary is swapped atomically and the node comes back up (chain sync resumes)
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
| Never auto-update | leave `auto_update` unset (default) or `GPN_AUTO_UPDATE=0` |
| Pause for a while | the same — it is checked at start, so `GPN_AUTO_UPDATE=0` disables the thread |
| Air-gapped / no manifest access | `GPN_UPDATE_CHECK=0` — disables *all* manifest fetches. ⚠ the cutoff cannot be evaluated either, so the node trusts its ≤ last verified state |
| Keep checking but never restart | `GPN_UPDATE_RESTART=0` |

---

## 5. The minimum-version cutoff

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

## 6. Rollback

Every swap keeps the previous binary: `<gpn_node>.bak`.

```bash
# stop the service first (or the .bak may be replaced by the next swap)
systemctl --user stop gpn-node        # or: Ctrl-C in the terminal

mv ~/.local/bin/gpn_node      ~/.local/bin/gpn_node.broken   # keep the evidence
mv ~/.local/bin/gpn_node.bak  ~/.local/bin/gpn_node
chmod +x ~/.local/bin/gpn_node

# make sure it stays: pin the version until the next release is fixed
#   auto_update = false   (~/.gpn/node.toml)
```

Then start the node again. Practical notes:

* `.bak` is **one step deep** — it holds the version that was running before the
  last swap, not a full history. If you need an older one, download it.
* If a release was bad, GPN will publish a new `releases.json` with a fixed
  `version` (and leave `min_version` alone) — auto-updaters recover on their next
  check, which is why rollback is a stopgap rather than a strategy.
* Rolling back below `min_version` puts the node in standby (sync-only). That is
  intentional: roll back to a version that is still supported, or accept
  sync-only until the fix ships.

### Diagnosing: `--check-updates`

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

## 7. Reference

### Env vars

| Var | Default | Meaning |
|---|---|---|
| `GPN_AUTO_UPDATE` | `0` | `1`/`true`/`yes`/`on` = opt **in** to automatic updates |
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
| `<base>/update/pending-update.json` | the staged marker: version, sha256, size, paths |
| `<base>/update/update-state.json` | last verified `version`/`min_version`, last check time (drives the offline cutoff) |
| `<bin>/gpn_node.new` | the staged binary (only between staging and swap) |
| `<bin>/gpn_node.bak` | the previous binary, for rollback |

### Code

| Path | Role |
|---|---|
| `gpn-agent/src/update.rs` | manifest parse + signature verify, version compare, cutoff decision, staging/swap, cached state, the embedded public key |
| `gpn-agent/src/bin/gpn_node.rs` | opt-in policy, startup check, updater thread, restart, `--setup` question |
| `gpn-node/node/src/service.rs` | `GPN_STANDBY` → no proposer, no miner |
| `scripts/gen-release-manifest.py` | build + sign + verify the manifest |
| `scripts/gpn-release-key.sh` | create/inspect the release key |
| `worker/worker.js` | `GET /releases.json` (headers, 503 when unpublished) |
| `docs/design/design-min-node-version.md` | the staged on-chain cutoff proposal |
