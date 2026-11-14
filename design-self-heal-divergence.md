# Self-heal on chain divergence (#157)

Owner requirement (2026-11-10): **a node whose block table is wrong on a fork must
repair itself**, instead of being banned by its peers and left stranded. Three forks in
five days produced three "healthy" dashboards and one outsider — the missing piece was
never detection (#156 shipped the signed checkpoint), it was a bounded recovery.

## Inputs

- `snapshot.json` — the signed snapshot document (`gpn-snapshot-v2`), verified against the
  release ed25519 key compiled into the node. It carries the checkpoint `block`,
  `block_hash`, `genesis`, and the archive's `url` / `size` / `sha256`.
- This node's own state, read over JSON-RPC: height, hash at the checkpoint height,
  genesis.

## Decision (pure, `heal_decision`)

| verdict | last recovery | decision |
|---|---|---|
| `Match` | — | `Nothing` (nothing to do) |
| `NotYet` / unreadable height | — | `Nothing` — *behind is not forked; unknown is not a verdict* |
| v1 metadata (no checkpoint) | — | `Nothing` — no claim to compare |
| `WrongChain` | none | `Recover` |
| `WrongChain` | < 24 h ago (or in the future) | `Refused` — exit 13, stop and explain |
| `WrongChain` | ≥ 24 h ago | `Recover` |

Genesis mismatch is **not** a fork: the node is pointed at another network, and no
automatic repair is correct. It exits 12 and touches nothing.

## Recovery sequence (`heal --apply`)

1. Resolve the archive (`--snapshot`, or download `url` into `--download-dir`) and verify
   **size + sha256 against the signed document** — before anything is stopped.
2. Write `last-recovery.json` with `completed: false`. The window is consumed *before* the
   destructive step, so a crash mid-repair can never loop.
3. `systemctl --user stop <unit>` (fatal on failure: importing under a live node corrupts).
4. `chains/` → `chains-fork-<ts>` (rename, never delete, never reuse an existing name — the
   forked DB is the only evidence of what this node was on).
5. Import the archive **without `--force`**, so a rename that did not happen is caught
   rather than papered over.
6. Mark the record complete, `systemctl --user start <unit>`.

If the import fails the unit is left **stopped** and the record says why: a node that comes
back on the fork looks healthier than it is.

## Exit codes

- `0` — nothing to do, or healed
- `12` — wrong chain; with no `--apply` this is a dry run (plan printed, nothing touched)
- `13` — a recovery was already attempted inside the 24 h window (or the ledger is corrupt:
  fail closed, never "no record")
- `1` — could not decide / I/O error

## Safety properties (each one is a test in `gpn_snapshot.rs`)

- A forked DB is never overwritten or deleted.
- One automatic recovery per 24 h; a clock that jumped backwards does not re-arm it.
- A corrupt or unreadable ledger refuses instead of licensing a loop.
- Unknown local state is never a fork, and a genesis mismatch never triggers a repair.

## Not in this change

- Wiring into a schedule (a `gpn-heal.timer` / the supervisor) — deliberately left to the
  owner's call: this stops the node and moves a chain DB.
- `recovered_from_fork` on `/status` — the observable half; the ledger it should read now
  exists.
- Handshake announcement of `genesis:hash@height` (#159), which lets both sides heal without
  a signed document in the loop.

## D8 — a peer *behind* on a branch we do not have (2026-11-13)

`classify` used to call a peer diverged only when both nodes were at the **same
height** with different hashes. Box 1 sat 16 000 blocks behind on its own fork
(`#280580` = `0xbbca…599d`, our `#280580` = `0xaa9e…a17f`, `chain_getHeader`
returns `null` for its hash), so it was classified *behind*, given no credit,
never protected — and our own block-request handler killed it 2 s after every
connect with a **fatal** `Same block request multiple times`
(`-(1 << 31)`, `sc-network-sync`'s repeat-request guard). It could never reorg,
because reorging needs us and we kept banning it.

Now: `classify_with_tip` promotes `PeerBehind` to `Diverged` when the client
says the peer's best hash is **not** one of ours (`classify` stays pure; the
membership lookup lives in `service.rs` and is skipped for every non-behind
shape). `decide_repair` is the task's one decision point — credit + keep the
link. The task also records the peer in `DivergenceProtection`, and the ranked
re-dial loop dials a protected peer **every tick, without backoff**, instead of
letting a 70 s ban accumulate into an 8-minute hole.

Honest limit: reputation credit **cannot** stop the fatal report — the
disconnect happens inside `sc-network-sync`'s block server before any credit
applies, and a banned peer disappears from `peers_info()`, so the 30 s scan
usually never sees it. Keeping the link therefore depends on the ban expiring
(~70 s of peerstore decay) and the re-dial succeeding; the 2-second window in
which the peer must make import progress is still too short for a 16 000-block
gap. The complete cure is a block server that caps repeated-request reputation
instead of failing fatally (see the D8 report); this change makes the node stop
classifying such a peer as healthy-behind and stop evicting it.
