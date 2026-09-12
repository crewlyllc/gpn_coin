//! Property-based **differential / fuzz** tests (proptest) — item 6 of the
//! production-readiness plan.
//!
//! These deliberately cross-check each *fast/native* path against an
//! independent *naive/reference* implementation on randomly generated inputs,
//! and fuzz the Groth16 verifier with malformed proofs/points. They are encoded
//! as `#[test]` property tests (rather than a `cargo-fuzz` target) so they run
//! inside the normal `cargo test` gate on every OS/CI runner.
//!
//! Coverage:
//! 1. `mul_ntt` (NTT-based) vs schoolbook polynomial multiplication.
//! 2. `eval_on_roots` (forward NTT) vs Horner evaluation at ωⁱ.
//! 3. `msm_g1`/`msm_g2` (Pippenger) vs naive sum of scalar mults.
//! 4. `Qap::eval_columns_at` (barycentric/Lagrange) vs dense column-poly
//!    evaluation, on both the `RootsOfUnity` and `Consecutive` domains.
//! 5. Constant-time fixed-base keygen == fast fixed-base keygen (byte-identical).
//! 6. Verifier soundness fuzz: tampered proofs, arbitrary points, and
//!    wrong-length public inputs are all rejected (and `verify` never panics).

#![cfg(test)]

use bls12_381::{G1Affine, G1Projective, G2Affine, G2Projective, Scalar};
use ff::Field;
use proptest::prelude::*;

use crate::ec_ops;
use crate::qap::{Constraint, Domain, R1CS};
use crate::{groth16, ntt};

// ===========================================================================
// Strategies
// ===========================================================================

/// Field scalars with broad coverage: 96-bit values plus the usual edges
/// (0, 1, −1 = the high-bit / near-modulus representative).
fn scalar_strategy() -> impl Strategy<Value = Scalar> {
    prop_oneof![
        8 => any::<[u32; 3]>().prop_map(|[a, b, c]| {
            Scalar::from(a as u64)
                + Scalar::from(b as u64) * Scalar::from(1u64 << 32)
                + Scalar::from(c as u64) * Scalar::from(1u64 << 32) * Scalar::from(1u64 << 32)
        }),
        1 => Just(Scalar::ZERO),
        1 => Just(Scalar::ONE),
        1 => Just(-Scalar::ONE),
    ]
}

fn scalar_vec(max: usize) -> impl Strategy<Value = Vec<Scalar>> {
    prop::collection::vec(scalar_strategy(), 0..=max)
}

/// A random point/scalar MSM instance for G1: points are honest generator
/// multiples (always on the curve), scalars are independent.
fn g1_case(max: usize) -> impl Strategy<Value = (Vec<G1Affine>, Vec<Scalar>)> {
    prop::collection::vec((scalar_strategy(), scalar_strategy()), 0..=max).prop_map(|v| {
        let g = G1Projective::generator();
        let pts = v.iter().map(|(e, _)| G1Affine::from(g * e)).collect();
        let sc = v.iter().map(|(_, s)| *s).collect();
        (pts, sc)
    })
}

fn g2_case(max: usize) -> impl Strategy<Value = (Vec<G2Affine>, Vec<Scalar>)> {
    prop::collection::vec((scalar_strategy(), scalar_strategy()), 0..=max).prop_map(|v| {
        let g = G2Projective::generator();
        let pts = v.iter().map(|(e, _)| G2Affine::from(g * e)).collect();
        let sc = v.iter().map(|(_, s)| *s).collect();
        (pts, sc)
    })
}

/// A sparse constraint row over `nv` variables (0..=3 terms).
fn sparse_row(nv: usize) -> impl Strategy<Value = Vec<(usize, Scalar)>> {
    prop::collection::vec((0usize..nv, scalar_strategy()), 0..=3)
}

fn constraint_strategy(nv: usize) -> impl Strategy<Value = Constraint> {
    (sparse_row(nv), sparse_row(nv), sparse_row(nv)).prop_map(|(a, b, c)| Constraint { a, b, c })
}

fn r1cs_strategy(domain: Domain) -> impl Strategy<Value = R1CS> {
    (2usize..=6, 1usize..=6).prop_flat_map(move |(nv, m)| {
        (
            Just(nv),
            prop::collection::vec(constraint_strategy(nv), m..=m),
        )
            .prop_map(move |(nv, constraints)| R1CS {
                num_variables: nv,
                num_public: 1,
                constraints,
                domain,
            })
    })
}

// ===========================================================================
// Naive references (independent implementations)
// ===========================================================================

fn schoolbook(a: &[Scalar], b: &[Scalar]) -> Vec<Scalar> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![Scalar::ZERO; a.len() + b.len() - 1];
    for (i, &x) in a.iter().enumerate() {
        for (j, &y) in b.iter().enumerate() {
            out[i + j] += x * y;
        }
    }
    out
}

fn horner(coeffs: &[Scalar], x: Scalar) -> Scalar {
    let mut acc = Scalar::ZERO;
    for c in coeffs.iter().rev() {
        acc = acc * x + c;
    }
    acc
}

fn naive_msm_g1(points: &[G1Affine], scalars: &[Scalar]) -> G1Projective {
    let mut acc = G1Projective::identity();
    for (p, s) in points.iter().zip(scalars.iter()) {
        acc += G1Projective::from(*p) * s;
    }
    acc
}

fn naive_msm_g2(points: &[G2Affine], scalars: &[Scalar]) -> G2Projective {
    let mut acc = G2Projective::identity();
    for (p, s) in points.iter().zip(scalars.iter()) {
        acc += G2Projective::from(*p) * s;
    }
    acc
}

/// A tiny satisfiable circuit (4 vars, 3 constraints) over the roots domain,
/// plus the honest Groth16 proof against a fixed CRS. Cheap enough to rebuild
/// inside each verifier-fuzz case.
fn tiny_proof() -> (groth16::VerifyingKey, Vec<Scalar>, groth16::Proof) {
    let z = [
        Scalar::ONE,
        Scalar::from(2u64),
        Scalar::from(3u64),
        Scalar::from(5u64),
    ];
    // (A·z)(B·z) = C·z:
    //   x0 · x1      = 1·2   = 2  → c: x0·2
    //   x2 · x0      = 3·1   = 3  → c: x2
    //   x3 · x3      = 5·5   = 25 → c: x0·25
    let constraints = vec![
        Constraint {
            a: vec![(0, Scalar::ONE)],
            b: vec![(1, Scalar::ONE)],
            c: vec![(0, z[1])],
        },
        Constraint {
            a: vec![(2, Scalar::ONE)],
            b: vec![(0, Scalar::ONE)],
            c: vec![(2, Scalar::ONE)],
        },
        Constraint {
            a: vec![(3, Scalar::ONE)],
            b: vec![(3, Scalar::ONE)],
            c: vec![(0, z[3] * z[3])],
        },
    ];
    let r1cs = R1CS {
        num_variables: 4,
        num_public: 1,
        constraints,
        domain: Domain::RootsOfUnity,
    };
    let qap = r1cs.qap();
    let pk = groth16::setup(&qap, "fuzz-crs");
    let (proof, pub_in) = groth16::prove(&pk, &qap, &z);
    (pk.vk, pub_in, proof)
}

// ===========================================================================
// Property tests
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    // -- 1. NTT multiplication vs schoolbook --------------------------------
    #[test]
    fn prop_mul_ntt_matches_schoolbook(a in scalar_vec(128), b in scalar_vec(128)) {
        prop_assert_eq!(ntt::mul_ntt(&a, &b), schoolbook(&a, &b));
    }

    // -- 2. Forward NTT (eval on roots) vs Horner ---------------------------
    #[test]
    fn prop_eval_on_roots_matches_horner(
        (k, coeffs) in (0usize..=6usize).prop_flat_map(|k| (
            Just(k),
            scalar_vec(1usize << k),
        )),
    ) {
        let n = 1usize << k;
        prop_assume!(n >= coeffs.len());
        let evals = ntt::eval_on_roots(&coeffs, n);
        let w = ntt::root_of_unity(n);
        let mut om = Scalar::ONE;
        for e in evals {
            prop_assert_eq!(e, horner(&coeffs, om));
            om *= w;
        }
    }

    // -- 3. Pippenger MSM vs naive sum --------------------------------------
    #[test]
    fn prop_msm_g1_matches_naive((pts, sc) in g1_case(48)) {
        prop_assert_eq!(ec_ops::msm_g1(&pts, &sc), naive_msm_g1(&pts, &sc));
    }

    #[test]
    fn prop_msm_g2_matches_naive((pts, sc) in g2_case(40)) {
        prop_assert_eq!(ec_ops::msm_g2(&pts, &sc), naive_msm_g2(&pts, &sc));
    }

    // -- 4. Single-point column evaluator vs dense polys --------------------
    #[test]
    fn prop_qap_eval_columns_matches_dense_roots(
        r1cs in r1cs_strategy(Domain::RootsOfUnity),
        tau in scalar_strategy(),
    ) {
        let qap = r1cs.qap();
        let (a, b, c) = qap.eval_columns_at(&tau);
        for k in 0..qap.num_variables {
            prop_assert_eq!(a[k], qap.a[k].eval(&tau));
            prop_assert_eq!(b[k], qap.b[k].eval(&tau));
            prop_assert_eq!(c[k], qap.c[k].eval(&tau));
        }
    }

    #[test]
    fn prop_qap_eval_columns_matches_dense_consecutive(
        r1cs in r1cs_strategy(Domain::Consecutive),
        tau in scalar_strategy(),
    ) {
        let qap = r1cs.qap();
        let (a, b, c) = qap.eval_columns_at(&tau);
        for k in 0..qap.num_variables {
            prop_assert_eq!(a[k], qap.a[k].eval(&tau));
            prop_assert_eq!(b[k], qap.b[k].eval(&tau));
            prop_assert_eq!(c[k], qap.c[k].eval(&tau));
        }
    }

    // -- 5. Constant-time keygen == fast keygen (byte-identical) ------------
    #[test]
    fn prop_ct_matches_fast_fixed_base(
        scalars in scalar_vec(48),
        inv in scalar_strategy(),
    ) {
        prop_assert_eq!(
            ec_ops::fixed_base_g1_gen(&scalars),
            ec_ops::fixed_base_g1_gen_ct(&scalars)
        );
        prop_assert_eq!(
            ec_ops::fixed_base_g2_gen(&scalars),
            ec_ops::fixed_base_g2_gen_ct(&scalars)
        );
        prop_assert_eq!(
            ec_ops::fixed_base_g1_gen_scaled(&scalars, &inv),
            ec_ops::fixed_base_g1_gen_scaled_ct(&scalars, &inv)
        );
    }

    // -- 6. Verifier soundness fuzz -----------------------------------------
    /// Any single-coordinate tamper of an honest proof must be rejected.
    #[test]
    fn prop_verifier_rejects_tampered_proof(kind in 0u8..4, s in scalar_strategy()) {
        let (vk, pub_in, proof) = tiny_proof();
        prop_assume!(s != Scalar::ZERO);
        let g1 = G1Projective::generator();
        let g2 = G2Projective::generator();
        let bad = match kind {
            0 => {
                let mut p = proof.clone();
                p.a = G1Affine::from(G1Projective::from(p.a) + g1 * s);
                p
            }
            1 => {
                let mut p = proof.clone();
                p.b = G2Affine::from(G2Projective::from(p.b) + g2 * s);
                p
            }
            2 => {
                let mut p = proof.clone();
                p.c = G1Affine::from(G1Projective::from(p.c) + g1 * s);
                p
            }
            _ => {
                let mut p = proof.clone();
                p.a = G1Affine::from(g1 * s);
                p
            }
        };
        prop_assert!(groth16::verify(&vk, &pub_in, &bad).is_err());
    }

    /// A random (valid-curve) proof triple must be rejected — unless it happens
    /// to be exactly the honest proof (the only accepted witness for this VK).
    #[test]
    fn prop_verifier_rejects_random_points(
        sa in scalar_strategy(),
        sb in scalar_strategy(),
        sc in scalar_strategy(),
    ) {
        let (vk, pub_in, honest) = tiny_proof();
        let bad = groth16::Proof {
            a: G1Affine::from(G1Projective::generator() * sa),
            b: G2Affine::from(G2Projective::generator() * sb),
            c: G1Affine::from(G1Projective::generator() * sc),
        };
        if groth16::verify(&vk, &pub_in, &bad).is_ok() {
            // The only proof that verifies for this VK is the honest one.
            prop_assert_eq!(bad, honest);
        }
    }

    /// Wrong-length public input vectors must be rejected as `InputLenMismatch`.
    #[test]
    fn prop_verifier_rejects_wrong_input_len(n in 0usize..=4usize) {
        let (vk, _pub_in, proof) = tiny_proof();
        let pub_in: Vec<Scalar> =
            (0..n).map(|i| Scalar::from(i as u64 + 1)).collect();
        if pub_in.len() + 1 != vk.gamma_abc_g1.len() {
            prop_assert_eq!(
                groth16::verify(&vk, &pub_in, &proof),
                Err(groth16::VerifyError::InputLenMismatch)
            );
        }
    }
}
