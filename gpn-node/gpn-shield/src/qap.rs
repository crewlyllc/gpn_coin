//! R1CS → QAP reduction and witness-to-`H(x)` computation.
//!
//! An R1CS instance has `num_variables` variables (index 0 is the constant `1`),
//! of which the first `num_public` (including the constant) are public and the
//! rest are private witnesses. Each constraint is `(A·z)·(B·z) = (C·z)` where
//! `z` is the full assignment `[1, pub..., priv...]`.
//!
//! QAP: for each variable `k`, interpolate `A_k(x), B_k(x), C_k(x)` through the
//! per-constraint coefficients evaluated on the domain `r_0..r_{m-1}`. Then a
//! satisfying assignment gives `A(x)·B(x) - C(x)` divisible by
//! `T(x) = ∏(x - r_j)`, and `H(x)` is that quotient — used by the Groth16 prover.

use bls12_381::Scalar;
use ff::Field;

use crate::poly::{interpolate, Poly};

/// A single R1CS constraint, sparse: `(Σ aᵢ·zᵢ)·(Σ bᵢ·zᵢ) = (Σ cᵢ·zᵢ)`.
/// Each entry is `(variable_index, coefficient)`.
#[derive(Clone, Debug)]
pub struct Constraint {
    pub a: Vec<(usize, Scalar)>,
    pub b: Vec<(usize, Scalar)>,
    pub c: Vec<(usize, Scalar)>,
}

/// An R1CS instance.
#[derive(Clone, Debug)]
pub struct R1CS {
    pub num_variables: usize,
    pub num_public: usize,
    pub constraints: Vec<Constraint>,
}

impl R1CS {
    /// Number of constraints.
    pub fn num_constraints(&self) -> usize {
        self.constraints.len()
    }

    /// The evaluation domain `r_j = j` for `j in 0..m` (a valid distinct set).
    fn domain(&self) -> Vec<Scalar> {
        (0..self.num_constraints()).map(|j| Scalar::from(j as u64)).collect()
    }

    /// Target polynomial `T(x) = ∏_j (x - r_j)`.
    pub fn target_poly(&self) -> Poly {
        let mut t = Poly::constant(Scalar::ONE);
        for r in self.domain() {
            t = t.mul(&Poly(vec![-(r), Scalar::ONE])); // (x - r)
        }
        t
    }

    /// For each variable `k`, the QAP polynomials `A_k, B_k, C_k`
    /// (each a `Poly` interpolating that variable's coefficients over the domain).
    fn column_polys(&self) -> (Vec<Poly>, Vec<Poly>, Vec<Poly>) {
        let m = self.num_constraints();
        let n = self.num_variables;
        let xs = self.domain();
        // Dense per-variable coefficient columns.
        let mut col_a = vec![vec![Scalar::ZERO; m]; n];
        let mut col_b = vec![vec![Scalar::ZERO; m]; n];
        let mut col_c = vec![vec![Scalar::ZERO; m]; n];
        for (j, c) in self.constraints.iter().enumerate() {
            for &(k, v) in &c.a {
                col_a[k][j] += v;
            }
            for &(k, v) in &c.b {
                col_b[k][j] += v;
            }
            for &(k, v) in &c.c {
                col_c[k][j] += v;
            }
        }
        let mut a = Vec::with_capacity(n);
        let mut b = Vec::with_capacity(n);
        let mut c = Vec::with_capacity(n);
        for k in 0..n {
            a.push(interpolate(&xs, &col_a[k]));
            b.push(interpolate(&xs, &col_b[k]));
            c.push(interpolate(&xs, &col_c[k]));
        }
        (a, b, c)
    }

    /// Build the QAP for this instance: the column polynomials and target poly.
    pub fn qap(&self) -> Qap {
        let (a, b, c) = self.column_polys();
        Qap {
            a,
            b,
            c,
            t: self.target_poly(),
            num_public: self.num_public,
            num_variables: self.num_variables,
            constraints: self.constraints.clone(),
        }
    }

    /// Build a QAP for the **fast single-point evaluator only** (`eval_columns_at`),
    /// skipping the dense per-variable column polynomials. The dense `a/b/c`
    /// polys are O(n·m) and infeasible for large circuits (e.g. the ~87k × ~121k
    /// note-spend circuit would need a ~10.5B-entry matrix). `eval_columns_at`
    /// needs only the sparse `constraints` + counts, so a large circuit can build
    /// a proving key without ever materializing the dense columns.
    ///
    /// ⚠️ The returned Qap has **empty `a/b/c`** and a **zero `t` placeholder** —
    /// it is valid for `eval_columns_at` ONLY. `setup` needs `t` (powers of τ up
    /// to 2m); `witness_polys` / `prove` need the dense `a/b/c`. Those callers
    /// must use the full `qap()` (feasible only for small/medium circuits).
    pub fn qap_sparse(&self) -> Qap {
        Qap {
            a: Vec::new(),
            b: Vec::new(),
            c: Vec::new(),
            t: Poly::zero(), // placeholder — not used by eval_columns_at
            num_public: self.num_public,
            num_variables: self.num_variables,
            constraints: self.constraints.clone(),
        }
    }
}

/// A QAP derived from an R1CS instance.
#[derive(Clone, Debug)]
pub struct Qap {
    /// `a[k] = A_k(x)` for variable `k`.
    pub a: Vec<Poly>,
    /// `b[k] = B_k(x)` for variable `k`.
    pub b: Vec<Poly>,
    /// `c[k] = C_k(x)` for variable `k`.
    pub c: Vec<Poly>,
    /// Target polynomial `T(x)`.
    pub t: Poly,
    /// Number of public variables (including the constant at index 0).
    pub num_public: usize,
    pub num_variables: usize,
    /// Sparse constraints (kept for the fast single-point evaluator; also used
    /// by [`Qap::eval_columns_at`] instead of the dense column polys).
    pub constraints: Vec<Constraint>,
}

impl Qap {
    /// Fast single-point evaluation of **all** column polynomials `A_k, B_k, C_k`
    /// at `tau` using the barycentric Lagrange formula for the consecutive-integer
    /// domain `r_j = j` (`j in 0..m`).
    ///
    /// This is O(m + nnz) in time and O(m + n) in memory, versus the dense
    /// [`R1CS::column_polys`] path (O(n·m) memory + O(n·m²) interpolation).
    /// Keygen needs exactly `A_k(τ), B_k(τ), C_k(τ)` for every variable, so this
    /// is the bottleneck that lets the large note-spend circuit build a proving key.
    ///
    /// Returns `(a_tau, b_tau, c_tau)`, each `Vec<Scalar>` of length `num_variables`.
    pub fn eval_columns_at(&self, tau: &Scalar) -> (Vec<Scalar>, Vec<Scalar>, Vec<Scalar>) {
        let m = self.constraints.len(); // number of constraints
        let n = self.num_variables;
        if m == 0 {
            return (vec![Scalar::ZERO; n], vec![Scalar::ZERO; n], vec![Scalar::ZERO; n]);
        }
        // Fast path — τ is a domain node r_t (integer 0..m-1). Then the
        // barycentric basis is a Kronecker delta (ℓ_t=1, others 0), so each
        // column value is exactly that constraint's own coefficient — no O(m)
        // inversions needed. This matters at scale (e.g. on-domain checks over
        // a 121k-constraint circuit would otherwise run 121k field inversions).
        // Canonical small scalars (Scalar::from(u64), u64 < m << field modulus)
        // have their high three 64-bit words zero in little-endian bytes.
        {
            let b = tau.to_bytes();
            let hi0 = u64::from_le_bytes(b[8..16].try_into().unwrap());
            let hi1 = u64::from_le_bytes(b[16..24].try_into().unwrap());
            let hi2 = u64::from_le_bytes(b[24..32].try_into().unwrap());
            if hi0 == 0 && hi1 == 0 && hi2 == 0 {
                let t = u64::from_le_bytes(b[0..8].try_into().unwrap()) as usize;
                if t < m {
                    // Accumulate only constraint t's coefficients.
                    let mut a_tau = vec![Scalar::ZERO; n];
                    let mut b_tau = vec![Scalar::ZERO; n];
                    let mut c_tau = vec![Scalar::ZERO; n];
                    let con = &self.constraints[t];
                    for &(k, v) in &con.a {
                        a_tau[k] += v;
                    }
                    for &(k, v) in &con.b {
                        b_tau[k] += v;
                    }
                    for &(k, v) in &con.c {
                        c_tau[k] += v;
                    }
                    return (a_tau, b_tau, c_tau);
                }
            }
        }
        // Barycentric weights for nodes r_j = j (consecutive integers 0..m-1):
        //   w_j = 1 / ∏_{k≠j}(j - k) = (-1)^(m-1-j) / (j! · (m-1-j)!)
        // Compute 1/i! for i in 0..m in O(m): inv_fact[m-1] = 1/(m-1)! then walk down.
        let mut fact = vec![Scalar::ONE; m]; // fact[i] = i!
        for i in 1..m {
            fact[i] = fact[i - 1] * Scalar::from(i as u64);
        }
        let mut inv_fact = vec![Scalar::ZERO; m]; // inv_fact[i] = 1/i!
        inv_fact[m - 1] = fact[m - 1].invert().expect("(m-1)! non-zero in Fr");
        for i in (1..m).rev() {
            inv_fact[i - 1] = inv_fact[i] * Scalar::from(i as u64);
        }
        // w_j, then the barycentric basis ℓ_j(τ) = (w_j/(τ-r_j)) / Σ_k(w_k/(τ-r_k)).
        let mut l = vec![Scalar::ZERO; m];
        let mut any_at_node = false;
        for j in 0..m {
            let sign = if (m - 1 - j) % 2 == 0 { Scalar::ONE } else { -Scalar::ONE };
            let w_j = sign * inv_fact[j] * inv_fact[m - 1 - j];
            let r_j = Scalar::from(j as u64);
            if tau == &r_j {
                any_at_node = true;
                l[j] = w_j; // placeholder; handled below
            } else {
                l[j] = w_j * (tau - r_j).invert().expect("tau != r_j here");
            }
        }
        if any_at_node {
            // tau equals a domain node r_j → ℓ_j(τ) = 1, all others 0.
            for j in 0..m {
                l[j] = if Scalar::from(j as u64) == *tau { Scalar::ONE } else { Scalar::ZERO };
            }
        } else {
            let sum: Scalar = l.iter().fold(Scalar::ZERO, |a, b| a + b);
            let sum_inv = sum.invert().expect("Σ w_k/(τ-r_k) non-zero");
            for v in l.iter_mut() {
                *v *= sum_inv;
            }
        }
        // Accumulate per-variable column evaluations from the sparse constraints.
        let mut a_tau = vec![Scalar::ZERO; n];
        let mut b_tau = vec![Scalar::ZERO; n];
        let mut c_tau = vec![Scalar::ZERO; n];
        for (j, con) in self.constraints.iter().enumerate() {
            let lj = l[j];
            for &(k, v) in &con.a {
                a_tau[k] += v * lj;
            }
            for &(k, v) in &con.b {
                b_tau[k] += v * lj;
            }
            for &(k, v) in &con.c {
                c_tau[k] += v * lj;
            }
        }
        (a_tau, b_tau, c_tau)
    }
}

/// The polynomials `A(x), B(x), C(x), H(x)` for a specific satisfying assignment.
/// `H(x) = (A(x)B(x) - C(x)) / T(x)`, with exact (zero) remainder.
pub struct WitnessPoly {
    pub a: Poly,
    pub b: Poly,
    pub c: Poly,
    pub h: Poly,
}

/// Compute `A(x), B(x), C(x), H(x)` for a full assignment `z` (length
/// `num_variables`, with `z[0] = 1`). Panics (via debug assert) if `z` does
/// not satisfy the circuit — the caller is expected to pass a valid witness.
pub fn witness_polys(qap: &Qap, z: &[Scalar]) -> WitnessPoly {
    debug_assert_eq!(z.len(), qap.num_variables);
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
    // F = A·B - C, divisible by T for a satisfying assignment.
    let f = a.mul(&b).sub(&c);
    let (h, rem) = f.div_rem(&qap.t);
    debug_assert_eq!(rem, Poly::zero(), "assignment does not satisfy the R1CS");
    WitnessPoly { a, b, c, h }
}

/// The value-balance circuit used across the crate and its tests.
/// Convention: index 0 = constant 1; the next `num_public-1` indices are public.
/// variables: 0=1(const), 1=fee(PUBLIC), 2=v_in(priv), 3=v_out(priv), 4=spent(priv), 5=w(priv).
///  c0: (v_in)·(1) = spent        → spent = v_in
///  c1: (v_out)·(1) = spent - fee → v_out = v_in - fee   (value balance)
///  c2: (v_in)·(v_out) = w        → w = v_in·v_out        (real mult gate)
pub fn value_balance_r1cs() -> R1CS {
    let one = Scalar::ONE;
    R1CS {
        num_variables: 6,
        num_public: 2, // indices 0 (const) and 1 (fee)
        constraints: vec![
            Constraint {
                a: vec![(2, one)], // v_in
                b: vec![(0, one)], // 1
                c: vec![(4, one)], // spent
            },
            Constraint {
                a: vec![(3, one)], // v_out
                b: vec![(0, one)], // 1
                c: vec![(4, one), (1, -(one))], // spent - fee
            },
            Constraint {
                a: vec![(2, one)], // v_in
                b: vec![(3, one)], // v_out
                c: vec![(5, one)], // w
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a deterministic sparse R1CS with `n` variables and `m` constraints.
    /// Each constraint is a local arithmetic relation over a sliding window of
    /// variables (realistic density), with distinct integer coefficients so a
    /// mismatch between fast and dense evaluation is easy to catch.
    fn synthetic_r1cs(n: usize, m: usize) -> R1CS {
        let one = Scalar::ONE;
        let two = Scalar::from(2u64);
        let mut constraints = Vec::with_capacity(m);
        for j in 0..m {
            // Variables within a window of the constraint index (guaranteed < n).
            let k0 = (3 * j) % n;
            let k1 = (k0 + 1) % n;
            let k2 = (k0 + 2) % n;
            // Distinct, constraint-dependent coefficients.
            let c_j = Scalar::from((j as u64) + 7);
            constraints.push(Constraint {
                // a: c_j·x_{k0} + x_{k1}
                a: vec![(k0, c_j), (k1, one)],
                // b: x_{k2} + 2·x_{k0}
                b: vec![(k2, one), (k0, two)],
                // c: x_{k1} + x_{k2} + x_{k0}
                c: vec![(k1, one), (k2, one), (k0, one)],
            });
        }
        R1CS { num_variables: n, num_public: 1, constraints }
    }

    #[test]
    fn qap_target_degree_equals_constraints() {
        let r1cs = value_balance_r1cs();
        assert_eq!(r1cs.target_poly().deg(), 3);
    }

    #[test]
    fn satisfying_witness_gives_zero_remainder() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        // fee=2 (pub), v_in=10, v_out=8, spent=10, w=80
        let z = vec![
            Scalar::ONE,
            Scalar::from(2),
            Scalar::from(10),
            Scalar::from(8),
            Scalar::from(10),
            Scalar::from(80),
        ];
        let wp = witness_polys(&qap, &z);
        // H should have degree < m-1 = 2 (here degree ≤ 1).
        assert!(wp.h.deg() < r1cs.num_constraints() - 1);
        // The core identity A(x)B(x)-C(x) = H(x)T(x) must hold as polynomials.
        let lhs = wp.a.mul(&wp.b).sub(&wp.c);
        let rhs = wp.h.mul(&qap.t);
        assert_eq!(lhs, rhs);
    }

    #[test]
    fn eval_columns_at_matches_dense_interpolation() {
        // The fast barycentric single-point evaluator must agree with the dense
        // per-column interpolation used by `column_polys`, for a tau not on the
        // domain and (separately) for a tau exactly on a domain node.
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let n = qap.num_variables;
        for tau in [Scalar::from(12345u64), Scalar::from(2u64)] {
            let (a_fast, b_fast, c_fast) = qap.eval_columns_at(&tau);
            let a_dense: Vec<Scalar> = qap.a.iter().map(|p| p.eval(&tau)).collect();
            let b_dense: Vec<Scalar> = qap.b.iter().map(|p| p.eval(&tau)).collect();
            let c_dense: Vec<Scalar> = qap.c.iter().map(|p| p.eval(&tau)).collect();
            for k in 0..n {
                assert_eq!(a_fast[k], a_dense[k], "A_{k} at tau={tau:?}");
                assert_eq!(b_fast[k], b_dense[k], "B_{k} at tau={tau:?}");
                assert_eq!(c_fast[k], c_dense[k], "C_{k} at tau={tau:?}");
            }
        }
    }

    #[test]
    // Fast path (barycentric) must agree with dense per-column interpolation on
    // a MODERATE synthetic circuit — larger than the 3-constraint value-balance
    // case (which only exercises m=3). This validates the barycentric
    // accumulation across many constraints while keeping dense `column_polys`
    // feasible: the dense interpolate is O(m²) per variable in unoptimized test
    // builds, so we keep it at ~2.4M ops (60 constraints) to run fast. The REAL
    // at-scale coverage of the fast path on the ~121k-constraint note-spend
    // circuit is provided by `eval_columns_at_on_domain_matches_sparse_at_note_spend_scale`
    // below (no dense polys needed).
    fn eval_columns_at_matches_dense_moderate_circuit() {
        let r1cs = synthetic_r1cs(/*vars=*/16, /*constraints=*/60);
        let qap = r1cs.qap(); // dense polys feasible at this size
        let n = qap.num_variables;
        for tau in [Scalar::from(1234567u64), Scalar::from(77u64), Scalar::from(0u64)] {
            let (a_fast, b_fast, c_fast) = qap.eval_columns_at(&tau);
            for k in 0..n {
                assert_eq!(a_fast[k], qap.a[k].eval(&tau), "A_{k} at tau={tau:?}");
                assert_eq!(b_fast[k], qap.b[k].eval(&tau), "B_{k} at tau={tau:?}");
                assert_eq!(c_fast[k], qap.c[k].eval(&tau), "C_{k} at tau={tau:?}");
            }
        }
    }

    #[test]
    // The SCALABLE on-domain check: for τ equal to a constraint's domain node
    // r_j, the barycentric basis reduces to ℓ_j(r_j)=1 / others 0, so
    // `eval_columns_at(r_j)` must return EXACTLY the sparse coefficients of that
    // constraint in each column — checkable in O(1) per constraint against the
    // sparse data, with NO dense QAP. This exercises the full fast path
    // (weights + accumulation) on the REAL note-spend circuit at full scale
    // (~87k vars × ~121k constraints) without O(n·m) memory.
    fn eval_columns_at_on_domain_matches_sparse_at_note_spend_scale() {
        let note = crate::note_spend::tests::test_note(4);
        let (r1cs, _z) = crate::note_spend::note_spend_r1cs(&note);
        let qap = r1cs.qap_sparse(); // NO dense polys — feasible at full scale
        let m = qap.constraints.len();
        // Sample a spread of constraint nodes (start, middle, near-end, end).
        let samples = [0usize, 1, m / 4, m / 2, 3 * m / 4, m - 2, m - 1];
        for &j in &samples {
            let r_j = Scalar::from(j as u64);
            let (a_fast, b_fast, c_fast) = qap.eval_columns_at(&r_j);
            let con = &qap.constraints[j];
            // At r_j, each column value must equal the constraint's own coeff.
            for &(k, v) in &con.a {
                assert_eq!(a_fast[k], v, "A column at constraint {j}, var {k}");
            }
            for &(k, v) in &con.b {
                assert_eq!(b_fast[k], v, "B column at constraint {j}, var {k}");
            }
            for &(k, v) in &con.c {
                assert_eq!(c_fast[k], v, "C column at constraint {j}, var {k}");
            }
            // Variables NOT in this constraint must evaluate to zero at r_j.
            let in_con = |k: usize, side: &[(usize, Scalar)]| side.iter().any(|(kk, _)| *kk == k);
            for k in 0..qap.num_variables {
                if !in_con(k, &con.a) {
                    assert_eq!(a_fast[k], Scalar::ZERO, "A_{k} must be 0 at r_{j} (var absent)");
                }
                if !in_con(k, &con.b) {
                    assert_eq!(b_fast[k], Scalar::ZERO, "B_{k} must be 0 at r_{j} (var absent)");
                }
                if !in_con(k, &con.c) {
                    assert_eq!(c_fast[k], Scalar::ZERO, "C_{k} must be 0 at r_{j} (var absent)");
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "does not satisfy the R1CS")]
    fn invalid_witness_panics() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        // Violate value balance: fee=2, v_in=10, v_out=9 → 9 != 10-2.
        let z = vec![
            Scalar::ONE,
            Scalar::from(2),
            Scalar::from(10),
            Scalar::from(9),
            Scalar::from(10),
            Scalar::from(90),
        ];
        let _ = witness_polys(&qap, &z);
    }
}
