# Fast node onboarding — snapshots, checkpoints, and why fast-sync won't help

**Task:** GPN-TASKS #23 · **Status:** snapshot tooling SHIPPED + live-verified; checkpoint DESIGNED (gated).

A fresh GPN node re-verifies every block, including recomputing its **Ethash seal**
(`hashimoto_light` on the light cache) for each imported block. So the time to
reach the tip grows with chain length (~**123k blocks** at the time of writing,
~+1 min of sync per ~14k blocks). The fix that actually works on this chain is a
**database snapshot**: hand the new node a copy of an already-synced RocksDB and
let it fetch only the last few blocks.

---

## 1. Findings — the node's own fast-sync modes don't work here

| Mode | Result on GPN |
| --- | --- |
| `--sync fast` / `fast-unsafe` | Bursts (~270 blk/s) then **stalls early (~#4097)**. The state proof needs a **finalized** block; GPN has no finality gadget. |
| `--sync warp` | Same problem — **requires finality** to build the warp proof. |
| default (`full`) | Reliable, but re-verifies every Ethash seal from genesis → grows with chain length. |

So the fastest *reliable* path is to not re-sync at all: copy a synced DB.

## 2. Recommended onboarding: **DB snapshot**

`gpn_snapshot` (in `gpn-agent`) exports a node's RocksDB plus a **manifest**, and
imports it into a fresh base-path:

```bash
# 1) capture block metadata while the node is UP (a stopped node has no RPC):
gpn_snapshot info --rpc http://127.0.0.1:9944 --out meta.json

# 2) export. Two safe options:
#    a) STOP the node, then:
gpn_snapshot export --base-path ~/.gpn/node --out gpn-testnet-123670.tar.gz --meta meta.json
#    b) or snapshot it LIVE with a consistent hot copy (SIGSTOP → copy → SIGCONT):
gpn_snapshot export --base-path ~/.gpn/node --out gpn-testnet-123670.tar.gz --freeze

# 3) on the NEW machine / fresh dir:
gpn_snapshot import --snapshot gpn-testnet-123670.tar.gz --base-path /new/node
gpn-node --chain testnet --base-path /new/node --port 30333 --rpc-port 9944 ...
```

Wrappers: `scripts/gpn-snapshot.sh` (Linux) and `scripts/gpn-snapshot.ps1`
(Windows). On Windows the flow is identical; only `--freeze` is Unix-only (stop
the node first).

### Safety — you MUST stop the node, or freeze it

RocksDB copied while a node is writing to it can be **torn/inconsistent**. The
tool detects a `gpn-node` bound to the same `--base-path` and refuses unless you
pass:

* **`--freeze`** — SIGSTOP the node, copy, SIGCONT. This is a *consistent* hot
  copy (files can't change while the process is frozen) and — unlike a stop — it
  **survives a supervisor** (`gpn_fleet`) that would otherwise restart the node
  mid-copy. `SIGCONT` is guaranteed even on error/panic.
* **`--allow-live`** — a best-effort copy of a running DB. *Unsafe*; use only as
  a last resort.

Because a **stopped** node has no RPC, capture the block metadata *before*
stopping (`gpn_snapshot info --out meta.json`) and pass `--meta`. `export` will
also read RPC directly if one is reachable.

### Tradeoffs — trust

A snapshot is only as trustworthy as its **source**:

* Verify the **genesis hash** in the manifest matches the chain you expect
  (import prints it).
* For anything security-sensitive, compare a **recent block hash** to an
  independent node — a snapshot lets you *skip work*, but it also means you
  adopt the exporter's view of history.
* The archive carries a `sha256` in its manifest for **integrity** (detect a
  corrupt transfer), not **authenticity** (it does not prove who made it).

### Measured (this node, 2026-10-12)

| Step | Value |
| --- | --- |
| DB size (`chains/gpn_testnet/db/full`) | **155 MB** |
| Snapshot (`.tar.zst`, recommended) | **~81 MiB**, 71 files, export **~1 s** (`zstd` binary) |
| Snapshot (`.tar.gz`, pure-Rust fallback) | ~124 MiB, export ~4.5 s |
| Import time | ~3–5 s |
| New node → tip | starts at **#123670** (snapshot tip), catches up to head in **~5 s**, then idle-following |
| Full re-sync (measured, fast local peer) | **~235 blk/s** → ~9 min for 123,676 blocks (network/CPU-bound; longer over a WAN or on slower hardware) |

Net: onboarding drops from *minutes-and-growing* (a full re-sync) to *seconds*
(the copy + import + the last handful of blocks). For a slower box (e.g. the
Windows provider) the win is larger, since a full sync there is network-bound.

## 3. Assume-valid checkpoint (stretch) — DESIGN only, gated

A Bitcoin `assumevalid`-style checkpoint would skip the expensive seal
verification for blocks *before* a trusted block, while fully verifying from the
checkpoint forward. This touches block-import verification, so it is
**review-first** — the design + gated plan live in
[`design/design-assume-valid-checkpoint.md`](design/design-assume-valid-checkpoint.md).
It is **not implemented** (no consensus change without explicit go-ahead).

## 4. Why not just ship the DB in the repo?

The DB is ~81–124 MiB compressed and **changes every block** — it would bloat the
repo and go stale immediately. Publish it out-of-band (an object store / HTTP
endpoint) and pin the manifest hash; the tool is agnostic to where the file
lives.

## 5. `.tar.zst` vs `.tar.gz`

Pick the format from the `--out` extension:

* **`.tar.zst`** (recommended) — uses the external `zstd` binary: ~81 MiB and
  ~1 s (multi-threaded). Needs `zstd` installed (`apt install zstd` / `choco
  install zstd`).
* **`.tar.gz`** — pure-Rust (no external tools), so it always works (including
  the Windows box), at ~124 MiB / ~4.5 s.

## 6. Snapshot pruning mode matters — export from an **archive** DB

Substrate **locks the state-pruning mode into the DB at creation**, and a
snapshot carries that mode with it. The node's legacy default
`--state-pruning 256` (+ `--blocks-pruning archive-canonical`) prunes *relative
to finality* — which GPN has none of — so a DB created with it **cannot serve
state** and will **wedge** (see the "node connected but not syncing" entry in
[`run-a-node.md`](run-a-node.md)).

So when you export a snapshot, take it from a node that was started with
`--state-pruning archive --blocks-pruning archive`:

* importing a **legacy `256`** snapshot and then launching with the (default)
  `archive` flags **hard-fails**:
  `Incompatible pruning modes [stored: Constrained { max_blocks: Some(256) };
  requested: ArchiveAll]`;
* importing an **`archive`** snapshot and launching with the defaults just works,
  and the node is stall-proof.

> **How `gpn_node` decides.** `gpn_snapshot export` records the source DB's
> pruning mode in the manifest and `gpn_snapshot import` re-materialises it as a
> `.gpn-pruning-archive` marker next to the DB. So an **archive** snapshot
> imported from a `gpn_node`-managed archive node is correctly marked, and
> `gpn_node` then applies `--state-pruning archive --blocks-pruning archive` on
> the imported DB just as it does for a fresh one. A legacy `256` snapshot is
> left unmarked (and is honestly *not* what you want — re-export from an archive
> node). If a snapshot's pruning mode is unknown/absent (e.g. from an older
> tool), `gpn_node` conservatively leaves pruning untouched and logs a warning —
> it never forces `archive` onto an unknown DB, so it can't crash-loop on
> `Incompatible pruning modes`. (To force it, set
> `GPN_NODE_ARGS="--state-pruning archive --blocks-pruning archive"`.)

`gpn_snapshot export` records the **genesis hash**, **best block** and a
`sha256` in the manifest, so an importer can check the snapshot is for the right
chain and the file isn't corrupt. (The pruning mode isn't a manifest field; it's
determined by how the exporting node was started — so make sure you export from
an `archive` node.) Recommended snapshot URLs are listed in
[`downloads.md`](downloads.md).

## 7. Recovering a wedged node (Windows provider, "box #2")

A node whose best block is frozen at an old height while peers stay connected
(the "silent wedge") is fixed by giving it a **fresh, archive** DB and starting
it with the robust defaults. On a Windows box, one copy-paste PowerShell line
(adjust `$URL` to the current snapshot from `downloads.md`; it downloads from
the AP at `gpn-testnet.duckdns.org:8083`, stops every `gpn*` process, swaps out
only the `chains` DB dir — keeping the node identity/keystore — imports the
fresh **archive** DB, and starts `gpn_node`):

```powershell
$URL='http://gpn-testnet.duckdns.org:8083/gpn-testnet-snapshot.tar.gz'; $BP="$env:USERPROFILE\.gpn\node"; Get-Process gpn* -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep 3; Invoke-WebRequest $URL -OutFile "$env:TEMP\gpn-snap.tar.gz"; if (Test-Path "$BP\chains") { Rename-Item "$BP\chains" "$BP\chains.bak-$(Get-Date -f yyyyMMddHHmmss)" }; New-Item -ItemType Directory -Force -Path "$BP\chains" | Out-Null; & "$env:USERPROFILE\.local\bin\gpn_snapshot.exe" import --snapshot "$env:TEMP\gpn-snap.tar.gz" --base-path $BP --force; & "$env:USERPROFILE\.local\bin\gpn_node.exe"
```

Only the `chains\` subdir is swapped, so the node's network identity
(`network/secret_ed25519`) and keystore are preserved (a new peer-id would
otherwise reset its on-chain full-node identity/uptime). `gpn_node` then
defaults to `--sync full --state-pruning archive --blocks-pruning archive` and
catches up only the last few blocks.

> **Windows base path.** The deployed `C:\gpn\start-node.cmd` pins
> `GPN_BASE_PATH=C:\gpn\data`, so import into `C:\gpn\data` (not the old
> OS-default `%LOCALAPPDATA%\gpn-node.exe\data`). `gpn_node` still auto-detects a
> DB left at the OS-default path and uses it if the pinned path has none, so an
> existing box keeps working; but a *fresh* archive import should target
> `C:\gpn\data` for a clean, consistent layout.

**If no snapshot is available**, the equivalent manual recovery is to *wipe* the
chain dir and let it re-sync from scratch
(`Rename-Item "$BP\chains" "$BP\chains.old"` and restart) — slower, but with
the new defaults it syncs all the way to the tip without stalling.
