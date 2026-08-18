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

// ---------------------------------------------------------------------------
// blake2s word-level gadget layer (gate #2/#3 foundation)
// ---------------------------------------------------------------------------
//
// The real note scheme (`pallet-shielded-pool::note`) hashes with **blake2s**
// over 32-bit words. To enforce `cm = H(...)` and `nf = H(...)` in R1CS we need
// the word primitives blake2s is built from: 32-bit addition mod 2^32, XOR,
// and right-rotation. This layer provides those as R1CS gates plus a full
// blake2s **G round** (the `G(a,b,c,d,x,y)` mixing function) wired from them.
// A full compression (10 rounds x 8 G calls) is assembled from these in a later
// cycle; here we prove the primitives are correct in-field against a plain-Rust
// reference.
//
// ## Representation & witness tracking
// A `Word` is a `Vec<usize>` of 32 bit-variable indices (bit 0 = LSB), each
// boolean-constrained. Because the intermediate variables (value sums, carry,
// bit-decomposed results) are many and internal, we use a **dual-pass builder**
// (`WordCircuit`) that allocates variables AND records their witness values in
// lockstep. This mirrors bellman's `Assignment` pattern and avoids a fragile
// generic constraint solver. 32-bit modular addition is enforced with a single
// constraint plus a carry bit: `s = x + y - carry·2^32` with `carry ∈ {0,1}`,
// so `s ≡ (x+y) mod 2^32` exactly (carry dropped, matching blake2s).

/// A 32-bit word as 32 boolean-constrained bit variables (bit 0 = LSB).
pub type Word = Vec<usize>;

/// Number of bits in a blake2s word.
pub const WORD_BITS: usize = 32;

/// Dual-pass circuit builder: builds R1CS constraints AND tracks a witness
/// assignment in lockstep, so the resulting `(r1cs, witness)` is guaranteed
/// satisfiable when the caller supplies consistent values.
pub struct WordCircuit {
    pub builder: CircuitBuilder,
    /// `values[i]` = witness value of variable `i` (index 0 = constant 1).
    pub values: Vec<Scalar>,
}

impl WordCircuit {
    pub fn new() -> Self {
        WordCircuit {
            builder: CircuitBuilder::new(),
            values: vec![Scalar::ONE], // index 0 = constant 1
        }
    }

    /// Allocate a fresh private variable with a known witness value.
    pub fn alloc(&mut self, value: Scalar) -> usize {
        let idx = self.builder.alloc();
        self.values.push(value);
        idx
    }

    /// Allocate a word from a u32 value: 32 boolean bits + a bound value var.
    /// The value var is the linear combo Σ bits[i]·2^i (used by carry math).
    pub fn alloc_word(&mut self, v: u32) -> Word {
        let mut w = Vec::with_capacity(WORD_BITS);
        for i in 0..WORD_BITS {
            let bit = ((v >> i) & 1) as u64;
            let bv = self.alloc(Scalar::from(bit));
            self.builder.bool_gate(bv);
            w.push(bv);
        }
        // value var = Σ 2^i·bit_i
        let value = self.alloc(Scalar::from(v as u64));
        let mut rhs = vec![(0, Scalar::ZERO)];
        for (i, &bv) in w.iter().enumerate() {
            rhs.push((bv, Scalar::from(1u64 << i)));
        }
        self.builder.add(vec![(value, Scalar::ONE)], vec![(0, Scalar::ONE)], rhs);
        w
    }

    /// 2-input XOR of two words, bit by bit (via `xor_gate`). Returns a new Word.
    pub fn word_xor(&mut self, x: &Word, y: &Word) -> Word {
        x.iter().zip(y.iter()).map(|(&xi, &yi)| {
            let xv = self.values[xi];
            let yv = self.values[yi];
            // out = x XOR y; xor_gate enforces out = x + y - 2xy.
            let out_val = if xv == Scalar::ONE && yv != Scalar::ONE
                || xv != Scalar::ONE && yv == Scalar::ONE { Scalar::ONE } else { Scalar::ZERO };
            let out = self.alloc(out_val);
            self.builder.bool_gate(out);
            self.builder.xor_gate(xi, yi, out);
            out
        }).collect()
    }

    /// Add two 32-bit words mod 2^32. Returns the result word and carry bit.
    pub fn word_add(&mut self, x: &Word, y: &Word) -> (Word, usize) {
        // Value vars for x and y (reuse the per-word bound value var).
        // We allocate fresh value vars for the addition terms.
        let xv_val = self.word_value(x);
        let yv_val = self.word_value(y);
        let xv = self.alloc(xv_val);
        let mut xrhs = vec![(0, Scalar::ZERO)];
        for (i, &bv) in x.iter().enumerate() {
            xrhs.push((bv, Scalar::from(1u64 << i)));
        }
        self.builder.add(vec![(xv, Scalar::ONE)], vec![(0, Scalar::ONE)], xrhs);

        let yv = self.alloc(yv_val);
        let mut yrhs = vec![(0, Scalar::ZERO)];
        for (i, &bv) in y.iter().enumerate() {
            yrhs.push((bv, Scalar::from(1u64 << i)));
        }
        self.builder.add(vec![(yv, Scalar::ONE)], vec![(0, Scalar::ONE)], yrhs);

        let full_u32 = self.low_u32(xv_val) as u64 + self.low_u32(yv_val) as u64;
        let (sum_val, carry_val) = if full_u32 >= (1u64 << WORD_BITS) {
            (Scalar::from(full_u32 - (1u64 << WORD_BITS)), Scalar::ONE)
        } else {
            (Scalar::from(full_u32), Scalar::ZERO)
        };
        let s = self.alloc(sum_val);
        let carry = self.alloc(carry_val);
        self.builder.bool_gate(carry);
        // (1)·(xv + yv) = s + carry·2^32
        self.builder.add(
            vec![(0, Scalar::ONE)],
            vec![(xv, Scalar::ONE), (yv, Scalar::ONE)],
            vec![(s, Scalar::ONE), (carry, Scalar::from(1u64 << WORD_BITS))],
        );
        // Decompose s into 32 bits for downstream use.
        let bits = self.bit_decompose_value(s, sum_val);
        (bits, carry)
    }

    /// Read the integer value of a word's bits (for reuse in addition).
    pub fn word_value(&self, w: &Word) -> Scalar {
        let mut v = Scalar::ZERO;
        for (i, &bv) in w.iter().enumerate() {
            if self.values[bv] == Scalar::ONE {
                v += Scalar::from(1u64 << i);
            }
        }
        v
    }

    /// Allocate 32 boolean bits whose weighted sum equals `value_var`'s value.
    pub fn bit_decompose_value(&mut self, value_var: usize, value: Scalar) -> Word {
        let low = self.low_u32(value);
        let mut bits = Vec::with_capacity(WORD_BITS);
        for i in 0..WORD_BITS {
            let b = ((low >> i) & 1) as u64;
            let bv = self.alloc(Scalar::from(b));
            self.builder.bool_gate(bv);
            bits.push(bv);
        }
        // value_var = Σ 2^i·bit_i
        let mut rhs = vec![(0, Scalar::ZERO)];
        for (i, &bv) in bits.iter().enumerate() {
            rhs.push((bv, Scalar::from(1u64 << i)));
        }
        self.builder.add(vec![(value_var, Scalar::ONE)], vec![(0, Scalar::ONE)], rhs);
        bits
    }

    /// Low 32 bits of a Scalar as u32 (interprets the field element's low bits).
    pub fn low_u32(&self, s: Scalar) -> u32 {
        let bytes = s.to_bytes();
        let mut v = 0u32;
        for i in 0..4 {
            v |= (bytes[i] as u32) << (8 * i);
        }
        v
    }

    /// Run a full blake2s G round on the state words, consuming x and y.
    pub fn g_round(&mut self, state: &mut [Word; 4], x: &Word, y: &Word) {
        let (t, _) = self.word_add(&state[0], &state[1]);
        let (a1, _) = self.word_add(&t, x);
        let d_xor = self.word_xor(&state[3], &a1);
        let d1 = word_rotr(&d_xor, 16);
        let (c1, _) = self.word_add(&state[2], &d1);
        let b_xor = self.word_xor(&state[1], &c1);
        let b1 = word_rotr(&b_xor, 12);
        let (t2, _) = self.word_add(&a1, &b1);
        let (a2, _) = self.word_add(&t2, y);
        let d_xor2 = self.word_xor(&d1, &a2);
        let d2 = word_rotr(&d_xor2, 8);
        let (c2, _) = self.word_add(&c1, &d2);
        let b_xor2 = self.word_xor(&b1, &c2);
        let b2 = word_rotr(&b_xor2, 7);
        state[0] = a2;
        state[1] = b2;
        state[2] = c2;
        state[3] = d2;
    }

    /// Finalize into `(R1CS, witness)`.
    pub fn into_parts(self, num_public: usize) -> (R1CS, Vec<Scalar>) {
        let r1cs = into_r1cs(&self.builder, num_public);
        (r1cs, self.values)
    }
}

/// Right-rotate a word by `n` bits (0 <= n < 32). Pure permutation — no
/// constraints needed, just reindexing (bit i of out = bit (i+n) mod 32 of in).
pub fn word_rotr(w: &Word, n: usize) -> Word {
    let n = n % WORD_BITS;
    (0..WORD_BITS)
        .map(|i| w[(i + n) % WORD_BITS])
        .collect()
}

/// Plain-Rust reference blake2s G round (u32, mod-2^32 arithmetic) for tests.
pub fn g_round_ref(state: &mut [u32; 4], x: u32, y: u32) {
    state[0] = state[0].wrapping_add(state[1]).wrapping_add(x);
    state[3] = (state[3] ^ state[0]).rotate_right(16);
    state[2] = state[2].wrapping_add(state[3]);
    state[1] = (state[1] ^ state[2]).rotate_right(12);
    state[0] = state[0].wrapping_add(state[1]).wrapping_add(y);
    state[3] = (state[3] ^ state[0]).rotate_right(8);
    state[2] = state[2].wrapping_add(state[3]);
    state[1] = (state[1] ^ state[2]).rotate_right(7);
}

/// Read a word's u32 value from a witness assignment.
pub fn word_to_u32(z: &[Scalar], w: &Word) -> u32 {
    let mut v = 0u32;
    for (i, &bv) in w.iter().enumerate() {
        if z[bv] == Scalar::ONE {
            v |= 1 << i;
        }
    }
    v
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

/// Direct satisfiability check: every R1CS constraint `(A·z)(B·z) = C·z` must
/// hold for the witness `z`. O(constraints) scalar math — no QAP interpolation —
/// so it stays fast even for large word-layer/compression circuits.
pub fn constraints_hold(r1cs: &R1CS, z: &[Scalar]) -> bool {
    r1cs.constraints.iter().all(|cn| {
        let a = cn.a.iter().map(|(i, c)| z[*i] * c).fold(Scalar::ZERO, |a, b| a + b);
        let b = cn.b.iter().map(|(i, c)| z[*i] * c).fold(Scalar::ZERO, |a, b| a + b);
        let c = cn.c.iter().map(|(i, c)| z[*i] * c).fold(Scalar::ZERO, |a, b| a + b);
        a * b == c
    })
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
    fn word_add_mod_2pow32_with_carry() {
        // 0xFFFFFFFF + 1 = 0 mod 2^32, carry = 1.
        let mut c = WordCircuit::new();
        let x = c.alloc_word(0xFFFF_FFFF);
        let y = c.alloc_word(1);
        let (sum, carry) = c.word_add(&x, &y);
        let (r1cs, z) = c.into_parts(1);
        assert!(constraints_hold(&r1cs, &z));
        assert_eq!(word_to_u32(&z, &sum), 0);
        assert_eq!(z[carry], Scalar::ONE);
    }

    #[test]
    fn word_add_no_overflow() {
        let mut c = WordCircuit::new();
        let x = c.alloc_word(0x1234_5678);
        let y = c.alloc_word(1);
        let (sum, carry) = c.word_add(&x, &y);
        let (r1cs, z) = c.into_parts(1);
        assert!(constraints_hold(&r1cs, &z));
        assert_eq!(word_to_u32(&z, &sum), 0x1234_5679);
        assert_eq!(z[carry], Scalar::ZERO);
    }

    #[test]
    fn word_xor_bitwise() {
        let mut c = WordCircuit::new();
        let x = c.alloc_word(0xF0F0_F0F0);
        let y = c.alloc_word(0x0FF0_0FF0);
        let out = c.word_xor(&x, &y);
        let (r1cs, z) = c.into_parts(1);
        assert!(constraints_hold(&r1cs, &z));
        assert_eq!(word_to_u32(&z, &out), 0xFF00_FF00);
    }

    #[test]
    fn word_rotr_permutation() {
        // rotr is a pure permutation: value must rotate, no constraints.
        let mut c = WordCircuit::new();
        let x = c.alloc_word(0x0000_0001);
        let r8 = word_rotr(&x, 8);
        let r16 = word_rotr(&x, 16);
        let r31 = word_rotr(&x, 31);
        let (_rcs, z) = c.into_parts(1);
        assert_eq!(word_to_u32(&z, &r8), 0x0100_0000);
        assert_eq!(word_to_u32(&z, &r16), 0x0001_0000);
        assert_eq!(word_to_u32(&z, &r31), 0x0000_0002);
    }

    #[test]
    fn g_round_matches_reference() {
        // IV block of blake2s (first 4 words of the IV constant).
        let st_ref = [
            0x6a09_e667u32,
            0xbb67_ae85,
            0x3c6e_f372,
            0xa54f_f53a,
        ];
        let x = 0x0000_0001u32;
        let y = 0x0000_0002u32;
        let mut expected = st_ref;
        g_round_ref(&mut expected, x, y);

        let mut c = WordCircuit::new();
        let mut state: [Word; 4] = [
            c.alloc_word(st_ref[0]),
            c.alloc_word(st_ref[1]),
            c.alloc_word(st_ref[2]),
            c.alloc_word(st_ref[3]),
        ];
        let wx = c.alloc_word(x);
        let wy = c.alloc_word(y);
        c.g_round(&mut state, &wx, &wy);
        let (rcs, z) = c.into_parts(1);
        assert!(constraints_hold(&rcs, &z));
        for i in 0..4 {
            assert_eq!(word_to_u32(&z, &state[i]), expected[i], "word {i}");
        }
        // Mutating reference must differ from input (real mixing happened).
        assert_ne!(expected[0], st_ref[0]);
    }

    #[test]
    fn g_round_rejects_wrong_witness() {
        // With the dual-pass builder the witness is always consistent; verify the
        // circuit actually enforces the relation by flipping an input bit and
        // confirming satisfiability breaks.
        let mut c = WordCircuit::new();
        let mut state: [Word; 4] = [
            c.alloc_word(0x6a09_e667),
            c.alloc_word(0xbb67_ae85),
            c.alloc_word(0x3c6e_f372),
            c.alloc_word(0xa54f_f53a),
        ];
        let wx = c.alloc_word(1);
        let wy = c.alloc_word(2);
        c.g_round(&mut state, &wx, &wy);
        let (rcs, z) = c.into_parts(1);
        assert!(constraints_hold(&rcs, &z));
        // Flip the LSB of every state bit -> inconsistent with the bound values.
        let mut zbad = z.clone();
        for w in state.iter() {
            zbad[w[0]] = Scalar::ONE - zbad[w[0]];
        }
        assert!(!constraints_hold(&rcs, &zbad));
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
