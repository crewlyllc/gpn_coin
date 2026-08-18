//! R1CS gadget foundation for the note-spend circuit (Feature 2, step 5b).
//!
//! The note-spend proof must enforce three things about a shielded transfer
//! without revealing the note:
//!   1. **Merkle membership** — the spent commitment `cm` is really in the
//!      on-chain pool (prove a path to a published root).
//!   2. **Commitment check** — `cm = NoteCommitment(v, rcm, addr)`.
//!   3. **Nullifier PRF** — `nf = Nullifier(rcm, position)`.
//!
//! This module provides the low-level R1CS *gadgets* (boolean, bit
//! decomposition, AND/XOR, equality) plus a working **Merkle-membership gate**
//! — gate #1 of the three. The blake2s commitment/nullifier gates (#2, #3) are
//! built on top of these same primitives in a later cycle (blake2s-in-field
//! arithmetic is the largest single effort).
//!
//! ## Honest scope
//! Merkle membership is enforced here with an **R1CS-friendly linear
//! combiner** `parent = H(child, sibling) = child + sibling·K` (fixed domain
//! constant `K`), which is exactly representable in the constraint system and
//! gives a correct, verifiable membership proof. This is a deliberate stand-in:
//! the real note scheme hashes with blake2s. Swapping the combiner to the
//! blake2s-in-field compression (next cycle) keeps this gate's structure — only
//! the `combine` constraints change.

use bls12_381::Scalar;
use ff::Field;

use crate::qap::{Constraint, R1CS};

/// Domain-separation constant for the Merkle combiner `H(l,r) = l + r·K`.
/// A fixed, non-zero scalar so `H` is injective in each child and differs from
/// the plain sum. (Stand-in for blake2s compression, see module docs.)
pub const MERKLE_K: u64 = 0x5bd1_e995; // xxHash-style prime, arbitrary fixed K

/// Accumulates R1CS constraints and allocates witness variables.
pub struct CircuitBuilder {
    pub constraints: Vec<Constraint>,
    pub num_variables: usize,
}

impl Default for CircuitBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl CircuitBuilder {
    /// Index 0 is reserved for the constant `1` (QAP convention).
    pub fn new() -> Self {
        CircuitBuilder { constraints: Vec::new(), num_variables: 1 }
    }

    /// Allocate a fresh private witness variable, returning its index.
    pub fn alloc(&mut self) -> usize {
        let idx = self.num_variables;
        self.num_variables += 1;
        idx
    }

    /// Add a constraint `(Σ aᵢ zᵢ)·(Σ bᵢ zᵢ) = Σ cᵢ zᵢ`.
    pub fn add(&mut self, a: Vec<(usize, Scalar)>, b: Vec<(usize, Scalar)>, c: Vec<(usize, Scalar)>) {
        self.constraints.push(Constraint { a, b, c });
    }

    /// Enforce `x * (1 - x) = 0`, i.e. `x ∈ {0,1}`.
    pub fn bool_gate(&mut self, x: usize) {
        // x·x = x
        self.add(vec![(x, Scalar::ONE)], vec![(x, Scalar::ONE)], vec![(x, Scalar::ONE)]);
    }

    /// Enforce `x = y` via `(1)·(x - y) = 0`.
    pub fn assert_equal(&mut self, x: usize, y: usize) {
        self.add(
            vec![(0, Scalar::ONE)],
            vec![(x, Scalar::ONE), (y, -(Scalar::ONE))],
            vec![(0, Scalar::ZERO)],
        );
    }

    /// Enforce `out = x XOR y` for boolean `x,y`: `out = x + y - 2xy`.
    /// R1CS form: `(2x)·(y) = x + y - out`.
    pub fn xor_gate(&mut self, x: usize, y: usize, out: usize) {
        self.add(
            vec![(x, Scalar::from(2))],
            vec![(y, Scalar::ONE)],
            vec![(x, Scalar::ONE), (y, Scalar::ONE), (out, -(Scalar::ONE))],
        );
    }

    /// Enforce `out = x AND y` for boolean `x,y`.
    pub fn and_gate(&mut self, x: usize, y: usize, out: usize) {
        self.add(vec![(x, Scalar::ONE)], vec![(y, Scalar::ONE)], vec![(out, Scalar::ONE)]);
    }

    /// Enforce that `value` equals the field element whose little-endian bit
    /// representation is `bits` (each bit allocated + boolean-constrained),
    /// returning the bit variable indices. `num_bits` bounds the value (< 2^n).
    pub fn bit_decompose(&mut self, value: usize, num_bits: usize) -> Vec<usize> {
        let bits: Vec<usize> = (0..num_bits).map(|_| self.alloc()).collect();
        for &b in &bits {
            self.bool_gate(b);
        }
        // value = Σ bits[i]·2^i  ⟺  (value)·(1) = Σ 2^i·bit_i
        let mut rhs = vec![(0, Scalar::ZERO)];
        for (i, &b) in bits.iter().enumerate() {
            rhs.push((b, Scalar::from(1u64 << i)));
        }
        self.add(vec![(value, Scalar::ONE)], vec![(0, Scalar::ONE)], rhs);
        bits
    }
}

/// Off-chain Merkle combiner mirroring the gate: `H(l, r) = l + r·K`.
pub fn merkle_combine(l: &Scalar, r: &Scalar) -> Scalar {
    *l + *r * Scalar::from(MERKLE_K)
}

/// Merkle membership gate: prove `leaf` is in a binary tree whose root is
/// `root`, given the sibling at each level and the position bit (0 = left,
/// 1 = right). Returns the intermediate parent-node variable indices (root-ward)
/// so callers can assign them in the witness.
///
/// `path` is ordered root-ward: `path[i]` is the sibling and side at the level
/// where the running node is at height `i` (i.e. `path[0]` pairs with `leaf`).
pub fn merkle_membership_gate(
    b: &mut CircuitBuilder,
    leaf: usize,
    root: usize,
    path: &[(usize, bool)], // (sibling_var, is_sibling_right)
) -> Vec<usize> {
    let mut nodes = Vec::with_capacity(path.len() + 1);
    nodes.push(leaf);
    let mut node = leaf;
    for (sibling, sibling_is_right) in path {
        // parent = combine(node, sibling) where node is on one side.
        let parent = b.alloc();
        if *sibling_is_right {
            // node is the LEFT child: parent = node + sibling·K
            // (1)·(node + sibling·K) = parent
            b.add(
                vec![(0, Scalar::ONE)],
                vec![(node, Scalar::ONE), (*sibling, Scalar::from(MERKLE_K))],
                vec![(parent, Scalar::ONE)],
            );
        } else {
            // node is the RIGHT child: parent = sibling + node·K
            // (1)·(sibling + node·K) = parent
            b.add(
                vec![(0, Scalar::ONE)],
                vec![(*sibling, Scalar::ONE), (node, Scalar::from(MERKLE_K))],
                vec![(parent, Scalar::ONE)],
            );
        }
        node = parent;
        nodes.push(parent);
    }
    // The final running node must equal the claimed root.
    b.assert_equal(node, root);
    nodes
}

/// Assemble an `R1CS` from the builder. `num_public` = 1 (only the constant is
/// public) unless the caller has marked additional public inputs; callers
/// building a spend circuit should set it explicitly after wiring public inputs.
pub fn into_r1cs(b: &CircuitBuilder, num_public: usize) -> R1CS {
    R1CS {
        num_variables: b.num_variables,
        num_public,
        constraints: b.constraints.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn satisfiable(r1cs: &R1CS, z: &[Scalar]) -> bool {
        use crate::poly::Poly;
        let qap = r1cs.qap();
        // A(x)B(x) - C(x) must be divisible by T(x) with zero remainder.
        let mut a = Poly::zero();
        let mut b = Poly::zero();
        let mut c = Poly::zero();
        for (k, zk) in z.iter().enumerate() {
            if *zk == Scalar::ZERO {
                continue;
            }
            a = a.add(&qap.a[k].scalar_mul(zk));
            b = b.add(&qap.b[k].scalar_mul(zk));
            c = c.add(&qap.c[k].scalar_mul(zk));
        }
        let f = a.mul(&b).sub(&c);
        let (_, rem) = f.div_rem(&qap.t);
        rem == Poly::zero()
    }

    #[test]
    fn bool_gate_enforces_binary() {
        let mut b = CircuitBuilder::new();
        let x = b.alloc();
        b.bool_gate(x);
        let r1cs = into_r1cs(&b, 1);
        // x=1 satisfies
        let mut z = vec![Scalar::ZERO; r1cs.num_variables];
        z[0] = Scalar::ONE;
        z[x] = Scalar::ONE;
        assert!(satisfiable(&r1cs, &z));
        // x=2 violates
        let mut z2 = vec![Scalar::ZERO; r1cs.num_variables];
        z2[0] = Scalar::ONE;
        z2[x] = Scalar::from(2);
        assert!(!satisfiable(&r1cs, &z2));
    }

    #[test]
    fn xor_and_gates() {
        let mut b = CircuitBuilder::new();
        let x = b.alloc();
        let y = b.alloc();
        let xo = b.alloc();
        let ao = b.alloc();
        for v in [x, y, xo, ao] {
            b.bool_gate(v);
        }
        b.xor_gate(x, y, xo);
        b.and_gate(x, y, ao);
        let r1cs = into_r1cs(&b, 1);
        let mut z = vec![Scalar::ZERO; r1cs.num_variables];
        z[0] = Scalar::ONE;
        // x=1,y=1 → xor=0, and=1
        z[x] = Scalar::ONE;
        z[y] = Scalar::ONE;
        z[xo] = Scalar::ZERO;
        z[ao] = Scalar::ONE;
        assert!(satisfiable(&r1cs, &z));
        // x=1,y=0 → xor=1, and=0
        let mut z2 = vec![Scalar::ZERO; r1cs.num_variables];
        z2[0] = Scalar::ONE;
        z2[x] = Scalar::ONE;
        z2[y] = Scalar::ZERO;
        z2[xo] = Scalar::ONE;
        z2[ao] = Scalar::ZERO;
        assert!(satisfiable(&r1cs, &z2));
    }

    #[test]
    fn bit_decompose_binds_value_to_bits() {
        let mut b = CircuitBuilder::new();
        let value = b.alloc();
        let bits = b.bit_decompose(value, 8);
        assert_eq!(bits.len(), 8);
        let r1cs = into_r1cs(&b, 1);
        let mut z = vec![Scalar::ZERO; r1cs.num_variables];
        z[0] = Scalar::ONE;
        z[value] = Scalar::from(0b1011_0010u64); // 178
        for (i, &bi) in bits.iter().enumerate() {
            z[bi] = Scalar::from(((178u64 >> i) & 1) as u64);
        }
        assert!(satisfiable(&r1cs, &z));
        // Wrong bits (value 178 but bits sum to 179) → violates.
        let mut z2 = vec![Scalar::ZERO; r1cs.num_variables];
        z2[0] = Scalar::ONE;
        z2[value] = Scalar::from(178u64);
        for (i, &bi) in bits.iter().enumerate() {
            z2[bi] = Scalar::from(((179u64 >> i) & 1) as u64);
        }
        assert!(!satisfiable(&r1cs, &z2));
    }

    #[test]
    fn merkle_membership_proves_path() {
        // 4-leaf tree. Leaves l0..l3.
        let leaves = (0u64..4).map(Scalar::from).collect::<Vec<_>>();
        // Build root off-chain with the same combiner.
        let n01 = merkle_combine(&leaves[0], &leaves[1]);
        let n23 = merkle_combine(&leaves[2], &leaves[3]);
        let root = merkle_combine(&n01, &n23);

        // Prove membership of leaf[1] (position bits: at level0 leaf[1] is the
        // RIGHT child of n01 → sibling leaves[0] on the left; at level1 n01 is
        // the LEFT child of root → sibling n23 on the right).
        let mut b = CircuitBuilder::new();
        let leaf = b.alloc();
        let root_v = b.alloc();
        let sib0 = b.alloc(); // leaves[0] (left of leaf[1])
        let sib1 = b.alloc(); // n23 (right of n01)
        let nodes = merkle_membership_gate(&mut b, leaf, root_v, &[(sib0, false), (sib1, true)]);
        let r1cs = into_r1cs(&b, 1);
        let mut z = vec![Scalar::ZERO; r1cs.num_variables];
        z[0] = Scalar::ONE;
        z[leaf] = leaves[1];
        z[root_v] = root;
        z[sib0] = leaves[0];
        z[sib1] = n23;
        // nodes = [leaf, n01, root]
        z[nodes[1]] = n01;
        z[nodes[2]] = root;
        assert!(satisfiable(&r1cs, &z));

        // Wrong sibling (claim leaves[3] instead of leaves[0]) → path breaks.
        let mut z2 = vec![Scalar::ZERO; r1cs.num_variables];
        z2[0] = Scalar::ONE;
        z2[leaf] = leaves[1];
        z2[root_v] = root;
        z2[sib0] = leaves[3];
        z2[sib1] = n23;
        z2[nodes[1]] = n01;
        z2[nodes[2]] = root;
        assert!(!satisfiable(&r1cs, &z2));
    }

    #[test]
    fn merkle_gate_rejects_wrong_root() {
        let leaves = (0u64..4).map(Scalar::from).collect::<Vec<_>>();
        let n01 = merkle_combine(&leaves[0], &leaves[1]);
        let n23 = merkle_combine(&leaves[2], &leaves[3]);
        let root = merkle_combine(&n01, &n23);
        let mut b = CircuitBuilder::new();
        let leaf = b.alloc();
        let root_v = b.alloc();
        let sib0 = b.alloc();
        let sib1 = b.alloc();
        let nodes = merkle_membership_gate(&mut b, leaf, root_v, &[(sib0, false), (sib1, true)]);
        let r1cs = into_r1cs(&b, 1);
        let mut z = vec![Scalar::ZERO; r1cs.num_variables];
        z[0] = Scalar::ONE;
        z[leaf] = leaves[1];
        z[root_v] = root + Scalar::ONE; // wrong root
        z[sib0] = leaves[0];
        z[sib1] = n23;
        z[nodes[1]] = n01;
        z[nodes[2]] = root;
        assert!(!satisfiable(&r1cs, &z));
    }
}
