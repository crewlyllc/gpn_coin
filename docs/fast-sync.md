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
