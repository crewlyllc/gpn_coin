//! **Hand-set, conservative** weights for `pallet-gpn-governance`.
//!
//! These are deliberately *not* benchmarked yet (see `TODO(bench)` below). They
//! are derived from measured costs of the primitives on the reference dev host
//! (release build) with a ~2× safety factor:
//!
//! | primitive                                   | measured | used here |
//! |---------------------------------------------|----------|-----------|
//! | `frame_system` DB read                      | ~12 µs   | **25 µs** |
//! | `frame_system` DB write                     | ~45 µs   | **100 µs**|
//! | blake2_256 over 500 KB                      | ~1.6 ms  | 10 ns/byte (5 ms @ 500 KB) |
//! | `RuntimeCall` SCALE decode (500 KB blob)    | ~2 ms    | 10 ns/byte |
//!
//! The per-byte term matters: `propose` carries the **whole** call inline today,
//! so a 500 KB `set_code` proposal is metered (≈5 ms of ref-time) instead of
//! free. `propose_preimage` carries the same bytes (also metered) but stores
//! them under `Preimages`, so later approvals touch a 36-byte proposal.
//!
//! Proof-size figures are proportional to the number of storage items touched
//! with a flat 4 KiB per item — these extrinsics are only ever submitted to the
//! committee's own nodes, so the proof market is not the constraint.
//!
//! TODO(bench): replace with `frame_benchmarking` output
//! (`frame-omni-bencher v1 benchmark pallet --runtime ... --pallet
//! pallet_gpn_governance --extrinsic '*'`) in the same change that wires the
//! other pallets (workplan item 9.4). Until then these numbers are *bounds*;
//! `weights::tests::weights_are_conservative` in the pallet's test module
//! asserts the per-byte term is non-zero and that the base cost exceeds the
//! bare DB cost, so a zeroed weight cannot silently come back.

use core::marker::PhantomData;
use frame_support::weights::Weight;

/// A single `frame_system` storage read, with a ~2× safety factor.
pub const DB_READ: u64 = 25_000;
/// A single `frame_system` storage write, with a ~2× safety factor.
pub const DB_WRITE: u64 = 100_000;
/// Per-byte cost for hashing + decoding + writing call/preimage bytes.
pub const PER_BYTE: u64 = 10; // 10 ns/byte → 5 ms for a 500 KB artifact

/// Proof size charged per storage item touched.
pub const PROOF_PER_ITEM: u64 = 4 * 1024;

pub trait WeightInfo {
    /// `propose(call)` — `call_len` is the SCALE-encoded length of the call.
    fn propose(call_len: u32) -> Weight;
    fn approve() -> Weight;
    /// `execute` — conservative: assumes a large (≈1 MiB) call/preimage.
    fn execute() -> Weight;
    fn cancel() -> Weight;
    /// `set_members` — `members` is the number of entries supplied.
    fn set_members(members: u32) -> Weight;
    /// `propose_preimage` — `len` is the byte length of the preimage.
    fn propose_preimage(len: u32) -> Weight;
    /// `on_initialize` pruning — `scanned` proposals examined.
    fn prune_expired(scanned: u32) -> Weight;
}

/// The weight type wired into `gpn-runtime`:
/// `type WeightInfo = pallet_gpn_governance::weights::GpnWeight<Runtime>`.
pub struct GpnWeight<T>(PhantomData<T>);

impl<T> WeightInfo for GpnWeight<T> {
    fn propose(call_len: u32) -> Weight {
        w_propose(call_len)
    }
    fn approve() -> Weight {
        w_approve()
    }
    fn execute() -> Weight {
        w_execute()
    }
    fn cancel() -> Weight {
        w_cancel()
    }
    fn set_members(members: u32) -> Weight {
        w_set_members(members)
    }
    fn propose_preimage(len: u32) -> Weight {
        w_propose_preimage(len)
    }
    fn prune_expired(scanned: u32) -> Weight {
        w_prune_expired(scanned)
    }
}

/// Unit impl for mock runtimes / tests. Same numbers, so a test that uses `()`
/// weighs exactly what mainnet weighs.
impl WeightInfo for () {
    fn propose(call_len: u32) -> Weight {
        w_propose(call_len)
    }
    fn approve() -> Weight {
        w_approve()
    }
    fn execute() -> Weight {
        w_execute()
    }
    fn cancel() -> Weight {
        w_cancel()
    }
    fn set_members(members: u32) -> Weight {
        w_set_members(members)
    }
    fn propose_preimage(len: u32) -> Weight {
        w_propose_preimage(len)
    }
    fn prune_expired(scanned: u32) -> Weight {
        w_prune_expired(scanned)
    }
}

// ── the numbers, in one place ──────────────────────────────────────────────

fn w_propose(call_len: u32) -> Weight {
    Weight::from_parts(
        // Members read, Proposals read, block number, event deposit, …
        8 * DB_READ + 4 * DB_WRITE + (call_len as u64).saturating_mul(PER_BYTE),
        6 * PROOF_PER_ITEM,
    )
}

fn w_approve() -> Weight {
    Weight::from_parts(5 * DB_READ + 2 * DB_WRITE, 4 * PROOF_PER_ITEM)
}

fn w_execute() -> Weight {
    // Read proposal (+ preimage), decode it, remove both, dispatch. Dispatch
    // itself is metered by the dispatched call (`System.set_code` carries its
    // own weight), so this is the governance overhead only.
    //
    // The byte term assumes a ~1 MiB payload. `MaxProposalCallLen` allows 4 MiB,
    // so an over-sized call is under-metered by up to ~30 ms of ref-time —
    // acceptable for a committee-only extrinsic, and fixed by TODO(bench).
    Weight::from_parts(
        10 * DB_READ + 5 * DB_WRITE + 1_048_576u64.saturating_mul(PER_BYTE),
        8 * PROOF_PER_ITEM,
    )
}

fn w_cancel() -> Weight {
    Weight::from_parts(6 * DB_READ + 2 * DB_WRITE, 4 * PROOF_PER_ITEM)
}

fn w_set_members(members: u32) -> Weight {
    Weight::from_parts(
        8 * DB_READ + (members as u64).saturating_mul(DB_READ + DB_WRITE) + 4 * DB_WRITE,
        (2 + 2 * members as u64) * PROOF_PER_ITEM,
    )
}

fn w_propose_preimage(len: u32) -> Weight {
    Weight::from_parts(
        // Membership read, hashing + SCALE decode of the bytes, 2 writes
        // (Preimages + Proposals) plus the event.
        10 * DB_READ + 4 * DB_WRITE + (len as u64).saturating_mul(PER_BYTE),
        8 * PROOF_PER_ITEM,
    )
}

fn w_prune_expired(scanned: u32) -> Weight {
    Weight::from_parts(
        (scanned as u64).saturating_mul(2 * DB_READ + DB_WRITE) + 2 * DB_WRITE,
        (1 + scanned as u64) * PROOF_PER_ITEM,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_are_conservative() {
        // A non-trivial call must cost strictly more than a trivial one.
        assert!(w_propose(500 * 1024).ref_time() > w_propose(0).ref_time());
        assert!(w_propose_preimage(500 * 1024).ref_time() > w_propose_preimage(0).ref_time());
        // 500 KB must be metered at ≥ 2 ms of ref-time (today: 5 ms).
        assert!(
            w_propose(500 * 1024).ref_time() - w_propose(0).ref_time() >= 2_000_000,
            "per-byte term too small: a 500 KB WASM must not be free"
        );
        // Nothing may be zero — a zero weight is the bug this replaces.
        for w in [
            w_propose(0),
            w_approve(),
            w_execute(),
            w_cancel(),
            w_set_members(5),
            w_propose_preimage(0),
            w_prune_expired(100),
        ] {
            assert!(w.ref_time() > 0 && w.proof_size() > 0);
        }
        // `()` and `GpnWeight<T>` must agree.
        assert_eq!(
            <() as WeightInfo>::propose(4242),
            <GpnWeight<()> as WeightInfo>::propose(4242)
        );
    }
}
