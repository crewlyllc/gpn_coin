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
    #[should_panic]
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
