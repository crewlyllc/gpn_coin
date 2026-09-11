//! **Note-spend QAP** — binds the three note-spend gates (Merkle membership #1,
//! commitment #2, nullifier #3) plus value balance into a single R1CS.
//!
//! Public inputs (in order, after the implicit constant `1`):
//!   `[value_balance, cm_lo, cm_hi, nf_lo, nf_hi, merkle_root]`
//! Each 256-bit digest is exposed as **two 128-bit field-element halves**
//! (`lo` = bits 0..127, `hi` = bits 128..255). Since 2^128 ≪ r (BLS12-381
//! scalar field), each half maps exactly to a field element with **no modular
//! wrap** — so the binding is sound and fully general (no spendable-commitment
//! restriction). The prototype Merkle tree commits to the low half of the
//! commitment as its leaf (the tree combiner is the documented linear
//! `H(l,r)=l+r·K` prototype, not a cryptographic hash).
//!
//! The circuit proves: given private witnesses `rcm`, `addr`, note value `v`,
//! and a Merkle path, the public commitment `cm = H("gpn/cm00"||v||rcm||addr)`
//! has its low half present in the tree rooted at `merkle_root`, the public
//! nullifier `nf = H("gpn/nf00"||rcm||position)` is correctly derived, and the
//! note value balances `value_balance` against a recipient amount `v_out`.
//!
//! Correctness is checked with the fast O(constraints) [`constraints_hold`]
//! predicate. The full Groth16 key for this larger circuit (Powers-of-Tau
//! ceremony + on-chain key swap + faster QAP path) is a separate documented
//! milestone.

use bls12_381::Scalar;
use ff::Field;

use crate::blake2s::{alloc_bytes32, digest_to_bytes, note_commitment_gate, nullifier_gate};
use crate::gadget::{into_r1cs_with_domain, merkle_combine, merkle_membership_gate, Word, WordCircuit};
use crate::qap::R1CS;

/// A concrete note spend: the note being spent plus the Merkle path proving its
/// commitment is in the tree.
#[derive(Clone, Debug)]
pub struct NoteSpend {
    /// Note value being spent (u128 bytes go into the commitment preimage).
    pub value: u64,
    /// Recipient amount; `value_balance = value - value_out` is public.
    pub value_out: u64,
    /// 32-byte trapdoor witness.
    pub rcm: [u8; 32],
    /// 32-byte address (spending key) witness.
    pub addr: [u8; 32],
    /// Pool position (u64) → nullifier input.
    pub position: u64,
    /// Merkle path, root-ward: `(sibling_field_element, is_sibling_right)`.
    pub path: Vec<(Scalar, bool)>,
}

/// Number of public variables including the constant `1`.
/// Public inputs (excluding const): value_balance, cm_lo, cm_hi, nf_lo, nf_hi, merkle_root.
pub const NUM_PUBLIC: usize = 7;

/// Build the note-spend R1CS plus the full witness assignment.
///
/// Returns `(r1cs, witness z)` where `z[0] = 1` and `z[1..7]` are the public
/// inputs. Satisfiability is validated with `gadget::constraints_hold`.
pub fn note_spend_r1cs(note: &NoteSpend) -> (R1CS, Vec<Scalar>) {
    let mut c = WordCircuit::new();

    // ---- Public variables first (indices 1..7). Values filled in after the
    // digest gates run (the digest determines cm/nf/root witness values).
    let vb_idx = c.alloc(Scalar::ZERO); // 1 value_balance
    let cm_lo_idx = c.alloc(Scalar::ZERO); // 2 cm low 128 bits
    let cm_hi_idx = c.alloc(Scalar::ZERO); // 3 cm high 128 bits
    let nf_lo_idx = c.alloc(Scalar::ZERO); // 4 nf low 128 bits
    let nf_hi_idx = c.alloc(Scalar::ZERO); // 5 nf high 128 bits
    let root_idx = c.alloc(Scalar::ZERO); // 6 merkle_root

    // ---- Private witnesses.
    let v_fe = c.alloc(Scalar::from(note.value));
    let v_out_fe = c.alloc(Scalar::from(note.value_out));
    let w = c.alloc(Scalar::from(note.value as u64 * note.value_out as u64));
    let rcm_bytes = alloc_bytes32(&mut c, &note.rcm);
    let addr_bytes = alloc_bytes32(&mut c, &note.addr);

    // ---- Gate #2: commitment  cm = H("gpn/cm00" || v || rcm || addr).
    let cm_words: [Word; 8] = note_commitment_gate(&mut c, note.value as u128, &rcm_bytes, &addr_bytes);
    // ---- Gate #3: nullifier   nf = H("gpn/nf00" || rcm || position).
    let nf_words: [Word; 8] = nullifier_gate(&mut c, &rcm_bytes, note.position);

    // ---- Bind public cm/nf halves (lo/hi 128-bit field elements, no wrap).
    bind_digest_halves(&mut c, &cm_words, cm_lo_idx, cm_hi_idx);
    bind_digest_halves(&mut c, &nf_words, nf_lo_idx, nf_hi_idx);

    // ---- Value balance:  v = v_out + value_balance  (mirrors value_balance_r1cs).
    let spent = c.alloc(Scalar::from(note.value as u64));
    c.builder.add(
        vec![(spent, Scalar::ONE)],
        vec![(0, Scalar::ONE)],
        vec![(v_fe, Scalar::ONE)],
    );
    c.builder.add(
        vec![(spent, Scalar::ONE)],
        vec![(0, Scalar::ONE)],
        vec![(v_out_fe, Scalar::ONE), (vb_idx, Scalar::ONE)],
    );
    c.builder.add(
        vec![(v_fe, Scalar::ONE)],
        vec![(v_out_fe, Scalar::ONE)],
        vec![(w, Scalar::ONE)],
    );

    // ---- Gate #1: Merkle membership. leaf = cm low half (public), root public.
    // Compute the digest-derived public values FIRST so the merkle backfill
    // reads the real cm_lo (not the placeholder ZERO).
    let cm_digest = digest_to_bytes(&c.values, &cm_words);
    let nf_digest = digest_to_bytes(&c.values, &nf_words);
    let (cm_lo, cm_hi) = digest_halves(&cm_digest);
    let (nf_lo, nf_hi) = digest_halves(&nf_digest);
    c.values[vb_idx] = Scalar::from(note.value as u64 - note.value_out as u64);
    c.values[cm_lo_idx] = cm_lo;
    c.values[cm_hi_idx] = cm_hi;
    c.values[nf_lo_idx] = nf_lo;
    c.values[nf_hi_idx] = nf_hi;

    let path_vars: Vec<(usize, bool)> = note
        .path
        .iter()
        .map(|(sib, is_right)| (c.alloc(*sib), *is_right))
        .collect();
    // Returns every intermediate parent var (root-ward); witness values for
    // these are NOT auto-filled (merkle_membership_gate allocs via
    // `builder.alloc`), so backfill them from the off-chain combiner.
    let merkle_nodes =
        merkle_membership_gate(&mut c.builder, cm_lo_idx, root_idx, &path_vars);
    backfill_merkle_witness(&mut c, cm_lo_idx, &note.path, &merkle_nodes);

    // Root witness = top of the (already-backfilled) path chain.
    let root = merkle_root_from_path(cm_lo, &note.path);
    c.values[root_idx] = root;

    // Note-spend is the large circuit (~87k vars × ~121k constraints), so use
    // the FFT-native domain: the Groth16 prover then runs in O(n log n) via the
    // NTT instead of O(n²). Nothing on-chain bakes this circuit's key, so the
    // domain choice is free to optimize.
    let r1cs = into_r1cs_with_domain(&c.builder, NUM_PUBLIC, crate::qap::Domain::RootsOfUnity);
    (r1cs, c.values)
}

/// Public inputs `[value_balance, cm_lo, cm_hi, nf_lo, nf_hi, merkle_root]`
/// for a note spend, recomputed off-chain from the reference blake2s.
pub fn public_inputs(note: &NoteSpend) -> Vec<Scalar> {
    let cm_pre: Vec<u8> = {
        let mut p = Vec::with_capacity(8 + 16 + 32 + 32);
        p.extend_from_slice(crate::blake2s::CM_DOMAIN);
        p.extend_from_slice(&(note.value as u128).to_be_bytes()); // u128, matches gate
        p.extend_from_slice(&note.rcm);
        p.extend_from_slice(&note.addr);
        p
    };
    let nf_pre: Vec<u8> = {
        let mut p = Vec::with_capacity(8 + 32 + 8);
        p.extend_from_slice(crate::blake2s::NF_DOMAIN);
        p.extend_from_slice(&note.rcm);
        p.extend_from_slice(&note.position.to_be_bytes());
        p
    };
    let (cm_lo, cm_hi) = digest_halves(&crate::blake2s::blake2s_256_ref(&cm_pre));
    let (nf_lo, nf_hi) = digest_halves(&crate::blake2s::blake2s_256_ref(&nf_pre));
    vec![
        Scalar::from(note.value as u64 - note.value_out as u64),
        cm_lo,
        cm_hi,
        nf_lo,
        nf_hi,
        merkle_root_from_path(cm_lo, &note.path),
    ]
}

/// Constrain `lo_idx` = bits 0..127 and `hi_idx` = bits 128..255 of the digest,
/// each as an exact field element (128 bits < r, so no wrap and no extra
/// top-bit constraints needed).
fn bind_digest_halves(c: &mut WordCircuit, words: &[Word; 8], lo_idx: usize, hi_idx: usize) {
    let bit = |i: usize| words[i / 32][i % 32];
    let mut lo_rhs = vec![(0, Scalar::ZERO)];
    let mut hi_rhs = vec![(0, Scalar::ZERO)];
    let mut pow = Scalar::ONE; // 2^i, i in 0..128
    for i in 0..128 {
        lo_rhs.push((bit(i), pow));
        hi_rhs.push((bit(i + 128), pow));
        pow *= Scalar::from(2u64);
    }
    c.builder.add(vec![(lo_idx, Scalar::ONE)], vec![(0, Scalar::ONE)], lo_rhs);
    c.builder.add(vec![(hi_idx, Scalar::ONE)], vec![(0, Scalar::ONE)], hi_rhs);
}

/// Split a 32-byte digest (little-endian bit order, byte 0 = LSB) into two
/// exact 128-bit field-element halves: `lo` = bits 0..127, `hi` = bits 128..255.
fn digest_halves(d: &[u8; 32]) -> (Scalar, Scalar) {
    let mut lo = Scalar::ZERO;
    let mut hi = Scalar::ZERO;
    let mut pow = Scalar::ONE;
    for (k, &byte) in d.iter().enumerate() {
        let target = if k < 16 { &mut lo } else { &mut hi };
        *target += Scalar::from(byte as u64) * pow;
        pow *= Scalar::from(256u64);
        if k == 15 {
            pow = Scalar::ONE; // reset for the high half
        }
    }
    (lo, hi)
}

/// Compute the Merkle root from a leaf and a root-ward path (off-chain mirror
/// of the `merkle_membership_gate` combiner).
pub fn merkle_root_from_path(leaf: Scalar, path: &[(Scalar, bool)]) -> Scalar {
    let mut node = leaf;
    for (sib, is_right) in path {
        node = if *is_right {
            merkle_combine(&node, sib)
        } else {
            merkle_combine(sib, &node)
        };
    }
    node
}

/// Backfill the witness values of the Merkle parent vars that
/// `merkle_membership_gate` allocated directly via `builder.alloc` (so they are
/// missing from `WordCircuit.values`). `merkle_nodes[0]` is the leaf; node `i+1`
/// is the parent at path level `i`.
fn backfill_merkle_witness(
    c: &mut WordCircuit,
    leaf_idx: usize,
    path: &[(Scalar, bool)],
    merkle_nodes: &[usize],
) {
    let mut node_val = c.values[leaf_idx];
    for (i, (sib, is_right)) in path.iter().enumerate() {
        node_val = if *is_right {
            merkle_combine(&node_val, sib)
        } else {
            merkle_combine(sib, &node_val)
        };
        // The parent vars are appended past the current end of `values`
        // (merkle_membership_gate allocs them via builder.alloc), in the same
        // level-0..n-1 order — so push rather than index-assign.
        assert_eq!(merkle_nodes[i + 1], c.values.len(), "merkle parents must append in order");
        c.values.push(node_val);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::gadget::constraints_hold;
    use crate::qap::value_balance_r1cs;

    pub(crate) fn test_note(depth: usize) -> NoteSpend {
        let mut note = NoteSpend {
            value: 1000,
            value_out: 990, // value_balance = 10
            rcm: [7u8; 32],
            addr: [9u8; 32],
            position: 42,
            path: Vec::new(),
        };
        // Build the merkle tree over the commitment leaf (low half) + siblings.
        let cm_pre: Vec<u8> = {
            let mut p = Vec::with_capacity(8 + 16 + 32 + 32);
            p.extend_from_slice(crate::blake2s::CM_DOMAIN);
            p.extend_from_slice(&(note.value as u128).to_be_bytes()); // u128, matches gate
            p.extend_from_slice(&note.rcm);
            p.extend_from_slice(&note.addr);
            p
        };
        let (cm_lo, _cm_hi) = digest_halves(&crate::blake2s::blake2s_256_ref(&cm_pre));
        let mut node = cm_lo;
        let mut path = Vec::new();
        for i in 0..depth {
            let sib = Scalar::from(1000 + i as u64);
            let is_right = i % 2 == 0;
            node = if is_right {
                merkle_combine(&node, &sib)
            } else {
                merkle_combine(&sib, &node)
            };
            path.push((sib, is_right));
        }
        note.path = path;
        note
    }

    #[test]
    fn note_spend_satisfiable() {
        let note = test_note(4);
        let (r1cs, z) = note_spend_r1cs(&note);
        assert!(constraints_hold(&r1cs, &z), "honest witness must satisfy");
    }

    #[test]
    fn note_spend_public_inputs_match_witness() {
        let note = test_note(4);
        let (_r1cs, z) = note_spend_r1cs(&note);
        let pub_in = public_inputs(&note);
        assert_eq!(pub_in.len(), NUM_PUBLIC - 1);
        for (i, p) in pub_in.iter().enumerate() {
            assert_eq!(*p, z[1 + i], "public input {i} must match witness var");
        }
    }

    #[test]
    fn wrong_public_value_balance_rejected() {
        let note = test_note(4);
        let (r1cs, mut z) = note_spend_r1cs(&note);
        // v=1000, v_out=990, but lie that value_balance=0 → 1000 != 990+0.
        z[1] = Scalar::ZERO;
        assert!(!constraints_hold(&r1cs, &z), "value-balance lie must fail");
    }

    #[test]
    fn wrong_public_cm_rejected() {
        let note = test_note(4);
        let (r1cs, mut z) = note_spend_r1cs(&note);
        z[2] = z[2] + Scalar::ONE; // wrong cm_lo
        assert!(!constraints_hold(&r1cs, &z), "wrong cm_lo must fail");
    }

    #[test]
    fn wrong_public_nf_rejected() {
        let note = test_note(4);
        let (r1cs, mut z) = note_spend_r1cs(&note);
        z[4] = z[4] + Scalar::ONE; // wrong nf_lo
        assert!(!constraints_hold(&r1cs, &z), "wrong nf_lo must fail");
    }

    #[test]
    fn tamper_merkle_path_rejected() {
        let note = test_note(4);
        let (r1cs, z) = note_spend_r1cs(&note);
        assert!(constraints_hold(&r1cs, &z));
        let mut z2 = z.clone();
        let last = z2.len() - 1; // last alloc = last path sibling
        z2[last] = z2[last] + Scalar::ONE;
        assert!(!constraints_hold(&r1cs, &z2), "wrong sibling must fail");
    }

    #[test]
    fn depth_0_and_depth_8_both_satisfiable() {
        for depth in [0usize, 8usize] {
            let note = test_note(depth);
            let (r1cs, z) = note_spend_r1cs(&note);
            assert!(constraints_hold(&r1cs, &z), "depth {depth} must satisfy");
        }
    }

    #[test]
    fn circuit_much_larger_than_value_balance() {
        let vb = value_balance_r1cs();
        let note = test_note(4);
        let (r1cs, _z) = note_spend_r1cs(&note);
        assert!(
            r1cs.constraints.len() > 10 * vb.constraints.len(),
            "note-spend should dwarf value-balance in constraint count"
        );
    }
}

