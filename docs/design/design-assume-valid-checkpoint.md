# Design — assume-valid checkpoint (skip deep PoW before a trusted block)

**Task:** GPN-TASKS #23 (stretch) · **Status:** DESIGN ONLY — **not implemented**.
**Review-first:** this weakens block verification, so it needs an explicit
go-ahead before any code lands. Nothing here changes the runtime, genesis, or
the canonical WASM.

Related: `docs/fast-sync.md` (the snapshot tooling that *is* shipped),
`design-fork-choice-heaviest.md` (finding #17 — longest vs heaviest).

## 1. Problem

A fresh GPN node imports and **verifies the Ethash seal of every block** from
genesis. The expensive step is, per block, in `gpn-node/node/src/service.rs`:

```rust
fn verify_seal(pre_hash, seal, target) -> bool {
    // ...
    let (computed_mix, result) = ethash::hashimoto_light(header_hash, nonce,
                                                          ethash_full_size(),
                                                          ethash_cache());
    computed_mix == mix && U256::from_little_endian(result) < target
}

impl PowAlgorithm<Block> for GPNPowAlgorithm {
    fn verify(&self, _parent, pre_hash, _pre_digest, seal, difficulty) -> ... {
        Ok(verify_seal(pre_hash, seal, difficulty))   // service.rs ~:179-187
    }
}
```

`PowBlockImport` (service.rs ~:273) calls `verify` for every imported block.
This is what makes full sync scale with chain length.

The **snapshot** path (shipped) sidesteps this by not re-syncing at all. The
**checkpoint** path is the complementary technique for operators who *do* sync
from peers: trust a recent block, verify nothing before it, verify everything
from it forward.

## 2. Analogy

Bitcoin Core's `-assumevalid=<hash>`: signature/script validation is skipped for
blocks **below** a hard-coded checkpoint height while **header** (PoW) and all
validation from the checkpoint forward stays on. GPN has no scripts — the
expensive per-block work *is* the PoW seal — so "assume valid" here means
*skip the seal recomputation below the checkpoint*, not the header chain.

## 3. Proposed change (node-only, forkless, reversible)

Add an **optional** constructor field to `GPNPowAlgorithm`:

```rust
struct Checkpoint { number: u64, hash: H256 }
struct GPNPowAlgorithm { client: Arc<FullClient>, assume_valid: Option<Checkpoint> }
```

and gate `verify`:

```rust
fn verify(&self, _parent, pre_hash, _digest, seal, difficulty) -> ... {
    Ok(match &self.assume_valid {
        // Below the checkpoint: trust the seal (the header hash is still
        // checked by the client; the checkpoint hash is enforced below).
        Some(cp) if header_number_of(pre_hash) < cp.number => true,
        _ => verify_seal(pre_hash, seal, difficulty),
    })
}
```

Because `verify` only receives `pre_hash` (not the block number directly), the
cleanest variant is a thin **`BlockImport` wrapper** around `PowBlockImport`
that decides per block using the header's number, rather than inspecting
`pre_hash`:

```
VerifyFromCheckpoint<Inner> {
   inner: PowBlockImport<..>,
   checkpoint: Option<(u64, H256)>,
}
impl BlockImport for VerifyFromCheckpoint {
  fn import_block(block) {
     if before_checkpoint(block) { /* skip inner's pow verify */ }
     if block.number == cp.number && block.hash != cp.hash { return Err(...) } // fail closed
     inner.import_block(block)   // full verify from cp.number+1 forward
  }
}
```

Either shape is acceptable; the wrapper is preferred because the block number
and hash are directly available and the "exactly at the checkpoint" equality
check is trivial.

### Guarantees we keep

* **Header linkage is unchanged** — the client still rejects a block whose
  `parent_hash` doesn't match the stored parent, so the header chain is intact
  all the way down to the trusted checkpoint.
* **Fail closed at the checkpoint** — when the node imports the block at
  `cp.number`, its hash **must equal** `cp.hash`; otherwise the import errors
  and the node will not accept the chain. This is the one hard anchor.
* **Full verification from the checkpoint forward** — every block at/after the
  checkpoint is verified exactly as today.
* **Default OFF** — without the flag, behaviour is byte-for-byte unchanged.

### What we give up (residual risk — must be stated)

* Blocks **below** the checkpoint are accepted without seal verification. A
  malicious peer could feed a fabricated sub-checkpoint history that happens to
  hash-link to the checkpoint — but it cannot produce a *different* block at
  `cp.number` (that would change the hash and fail the anchor), and it cannot
  forge anything at/after the checkpoint. This is the same trust transfer as a
  snapshot: you trust the *source of the checkpoint hash*.
* **Fork interplay (finding #17):** GPN's fork choice is by height, not
  cumulative work. A checkpoint must be on the **canonical** chain; a competing
  branch below the checkpoint would be accepted unverified. Mitigations:
  (a) publish the checkpoint **close to the tip** so the trusted span is short;
  (b) pin the checkpoint hash to a value an operator obtained out-of-band
  (explorer / another node / the snapshot manifest) — never from the peer you
  are syncing from; (c) ideally gate this behind the eventual work-based fork
  choice.

## 4. Gated implementation plan (do NOT start without go-ahead)

1. **Flag + plumbing** — `--assume-valid <block-number>:<0xhash>` on the node
   CLI (`node/src/cli.rs` / `command.rs`), threaded into the service and the new
   wrapper. Parse/validate the hex; reject a malformed or absent hash.
2. **Wrapper** — implement `VerifyFromCheckpoint` around `PowBlockImport` using
   `sc_consensus::{BlockImport, ImportResult}`; wire it in `new_partial` (both
   the full and the validating/parachain paths, service.rs ~:273 and ~:624).
3. **Unit tests** (`node/src/service.rs` or a small module):
   * a block below the checkpoint passes import even with a **bad seal**;
   * a block at the checkpoint with the **wrong hash** is **rejected**;
   * a block at/after the checkpoint with a **bad seal** is **rejected**
     (verification is still on);
   * with no flag, a bad seal below any height is still rejected.
4. **E2E (live, on a throwaway base-path):**
   * `gpn-node --assume-valid <N>:<hash>` from genesis against a peer →
     reaches the tip; confirm it rejected nothing below N and that a
     deliberately-wrong checkpoint hash makes the node **stall/error** at N.
   * regression: no-flag run still full-verifies (unchanged).
5. **Docs** — extend `docs/fast-sync.md`; add the flag to the node's `--help`
   and the Windows run script.
6. **Review gate** — land only on explicit approval, since it is a
   verification-weakening change. Prefer to pair it with (or defer it until)
   the work-based fork choice (#17).

## 5. Recommendation

The **snapshot** already removes the re-sync cost for the common case (a new
node / the Windows box after a restart). Ship that now (done). Treat the
assume-valid checkpoint as a **shipped-on-request** follow-up — it only helps
the peer-sync-from-genesis case and carries a small trust/fork-choice cost, so
it should ride alongside the #17 fork-choice work rather than a one-off.
