//! Independent verification suite for the FFT-native prover work.
//!
//! These tests deliberately do NOT just re-run the same production functions
//! against themselves. Where possible they cross-check a *fast* path against an
//! *independent slow/reference* computation, exercise edge cases (padding rows,
//! non-power-of-two constraint counts, on/off-domain τ), and check the
//! invariants the FFT refactor relies on.
//!
//! Run: `cargo test -p gpn-shield` (fast subset) and the `#[ignore]`d
//! at-scale tests with `cargo test --release -p gpn-shield -- --ignored`.

#![cfg(test)]

use bls12_381::Scalar;
use ff::Field;

use crate::ntt;
use crate::poly::Poly;
use crate::qap::{self, Constraint, Domain, R1CS};
use crate::groth16;

/// Deterministic xorshift PRNG for reproducible pseudo-random test vectors.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self { Rng(seed | 1) }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn scalar(&mut self) -> Scalar {
        // Build a scalar from 3 random u64 limbs (well within the modulus).
        let a = self.next() & 0xFFFF_FFFF;
        let b = self.next() & 0xFFFF_FFFF;
        let c = self.next() & 0xFFFF_FFFF;
        Scalar::from(a) + Scalar::from(b) * Scalar::from(1u64 << 32)
            + Scalar::from(c) * Scalar::from(1u64 << 32) * Scalar::from(1u64 << 32)
    }
}

/// Naive schoolbook polynomial multiplication (reference).
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

/// Naive Horner evaluation (reference).
fn horner(coeffs: &[Scalar], x: Scalar) -> Scalar {
    let mut acc = Scalar::ZERO;
    for c in coeffs.iter().rev() {
        acc = acc * x + c;
    }
    acc
}

// ===========================================================================
// 1. NTT correctness (randomised, several sizes)
// ===========================================================================

#[test]
fn ntt_roundtrip_random_all_sizes() {
    for k in 0..=12u32 {
        let n = 1usize << k;
        let mut rng = Rng::new(0xABCD ^ n as u64);
        let a: Vec<Scalar> = (0..n).map(|_| rng.scalar()).collect();
        let mut b = a.clone();
        ntt::ntt(&mut b, false);
        ntt::ntt(&mut b, true);
        assert_eq!(a, b, "NTT roundtrip failed at n={n}");
    }
}

#[test]
fn ntt_mul_matches_schoolbook_random() {
    // Several sizes, including non-power-of-two lengths (so the NTT must pad).
    for &(la, lb) in &[(3usize, 5usize), (8, 8), (17, 33), (100, 57), (257, 129), (1000, 1000)] {
        let mut rng = Rng::new(0x1234 ^ ((la as u64) << 16) ^ lb as u64);
        let a: Vec<Scalar> = (0..la).map(|_| rng.scalar()).collect();
        let b: Vec<Scalar> = (0..lb).map(|_| rng.scalar()).collect();
        let fast = ntt::mul_ntt(&a, &b);
        let slow = schoolbook(&a, &b);
        assert_eq!(fast, slow, "mul_ntt != schoolbook for ({la},{lb})");
    }
}

#[test]
fn eval_interpolate_on_roots_roundtrip() {
    for k in 1..=10u32 {
        let n = 1usize << k;
        let mut rng = Rng::new(0xE0F ^ n as u64);
        // Random polynomial of degree < n/2 (so it fits).
        let deg = n / 2;
        let coeffs: Vec<Scalar> = (0..deg).map(|_| rng.scalar()).collect();
        let evals = ntt::eval_on_roots(&coeffs, n);
        // Each eval at ω^i must equal Horner.
        let w = ntt::root_of_unity(n);
        let mut om = Scalar::ONE;
        for &e in &evals {
            assert_eq!(e, horner(&coeffs, om), "eval mismatch at ω^{}", om);
            om *= w;
        }
        // Interpolation recovers the padded coefficients.
        let back = ntt::interpolate_on_roots(&evals);
        for i in 0..n {
            let want = coeffs.get(i).copied().unwrap_or(Scalar::ZERO);
            assert_eq!(back[i], want, "interp coeff {i} mismatch (n={n})");
        }
    }
}

#[test]
fn root_of_unity_has_exact_order() {
    for k in 0..=20u32 {
        let n = 1usize << k;
        let w = ntt::root_of_unity(n);
        // w^n == 1.
        assert_eq!(ntt::scalar_pow(w, n as u64), Scalar::ONE, "ω^n != 1 at n={n}");
        // Every lower power != 1 (exact order n), for n>1.
        if n > 1 {
            for j in 1..n {
                assert_ne!(ntt::scalar_pow(w, j as u64), Scalar::ONE, "ω^{j} == 1 (order < n={n})");
            }
        }
    }
}

// ===========================================================================
// 2. Domain invariants
// ===========================================================================

fn roots_r1cs(nvars: usize, ncons: usize, seed: u64) -> R1CS {
    let mut rng = Rng::new(seed);
    let mut constraints = Vec::with_capacity(ncons);
    for j in 0..ncons {
        let k0 = (3 * j) % nvars;
        let k1 = (k0 + 1) % nvars;
        let k2 = (k0 + 2) % nvars;
        constraints.push(Constraint {
            a: vec![(k0, rng.scalar()), (k1, Scalar::ONE)],
            b: vec![(k2, Scalar::ONE), (k0, Scalar::from(2u64))],
            c: vec![(k1, Scalar::ONE), (k2, Scalar::ONE), (k0, Scalar::ONE)],
        });
    }
    R1CS { num_variables: nvars, num_public: 1, constraints, domain: Domain::RootsOfUnity }
}

#[test]
fn roots_domain_size_is_next_pow2() {
    for m in [1usize, 2, 3, 4, 5, 7, 8, 9, 100, 1024, 1025] {
        let r1cs = roots_r1cs(8, m, 0xD00 ^ m as u64);
        let expected = ntt::next_pow2(m.max(1));
        assert_eq!(r1cs.domain_size(), expected, "domain_size wrong for m={m}");
        assert!(r1cs.domain_size().is_power_of_two());
        assert!(r1cs.domain_size() >= m);
    }
}

#[test]
fn target_poly_is_roots_vanishing() {
    // For RootsOfUnity, T(x) must equal x^n - 1.
    for m in [3usize, 8, 17, 100] {
        let r1cs = roots_r1cs(8, m, 0x10 ^ m as u64);
        let n = r1cs.domain_size();
        let t = r1cs.target_poly();
        assert_eq!(t.deg(), n, "deg T should be n={n} for m={m}");
        // Evaluate T at a domain node → 0.
        let w = ntt::root_of_unity(n);
        assert_eq!(t.eval(&Scalar::ONE), Scalar::ZERO, "T(1) must be 0");
        assert_eq!(t.eval(&w), Scalar::ZERO, "T(ω) must be 0");
        // T(x) = x^n - 1 exactly.
        for i in 0..=n {
            let want = if i == 0 { -Scalar::ONE } else if i == n { Scalar::ONE } else { Scalar::ZERO };
            assert_eq!(t.0[i], want, "T coeff {i} wrong for n={n}");
        }
    }
}

// ===========================================================================
// 3. Roots-domain column evaluator vs dense interpolation (independent check)
// ===========================================================================

#[test]
fn eval_columns_at_roots_matches_dense_random_tau() {
    for (nv, m) in [(16usize, 16usize), (24, 40), (10, 7)] {
        let r1cs = roots_r1cs(nv, m, 0xC01 ^ ((nv as u64) << 8) ^ m as u64);
        let qap = r1cs.qap(); // dense column polys over the roots domain
        let mut rng = Rng::new(0x7A0 ^ m as u64);
        for _ in 0..4 {
            let tau = rng.scalar();
            let (a_fast, b_fast, c_fast) = qap.eval_columns_at(&tau);
            for k in 0..qap.num_variables {
                assert_eq!(a_fast[k], qap.a[k].eval(&tau), "A_{k} at tau (nv={nv},m={m})");
                assert_eq!(b_fast[k], qap.b[k].eval(&tau), "B_{k} at tau (nv={nv},m={m})");
                assert_eq!(c_fast[k], qap.c[k].eval(&tau), "C_{k} at tau (nv={nv},m={m})");
            }
        }
        // On-domain τ (a node) must equal that constraint's own coefficients.
        let w = ntt::root_of_unity(qap.domain_size);
        let mut om = Scalar::ONE;
        for j in 0..qap.constraints.len() {
            let (a, b, c) = qap.eval_columns_at(&om);
            let con = &qap.constraints[j];
            for &(k, v) in &con.a { assert_eq!(a[k], v, "on-domain A row {j} var {k}"); }
            for &(k, v) in &con.b { assert_eq!(b[k], v, "on-domain B row {j} var {k}"); }
            for &(k, v) in &con.c { assert_eq!(c[k], v, "on-domain C row {j} var {k}"); }
            om *= w;
        }
        // Padding rows (m..n) must be all-zero (constraint absent).
        for j in qap.constraints.len()..qap.domain_size {
            let (a, b, c) = qap.eval_columns_at(&om);
            assert!(a.iter().all(|x| *x == Scalar::ZERO), "padding A row {j} must be 0");
            assert!(b.iter().all(|x| *x == Scalar::ZERO), "padding B row {j} must be 0");
            assert!(c.iter().all(|x| *x == Scalar::ZERO), "padding C row {j} must be 0");
            om *= w;
        }
    }
}

// ===========================================================================
// 4. FFT witness path vs dense witness path (independent reference)
// ===========================================================================

/// Build a *satisfiable* random R1CS over the roots domain:
/// pick z, then make each constraint hold by setting the c-side from a·z, b·z.
fn satisfiable_roots_r1cs(nvars: usize, ncons: usize, seed: u64) -> (R1CS, Vec<Scalar>) {
    let mut rng = Rng::new(seed);
    // z[0] = 1; rest random.
    let mut z: Vec<Scalar> = vec![Scalar::ONE];
    for _ in 1..nvars {
        z.push(rng.scalar());
    }
    let mut constraints = Vec::with_capacity(ncons);
    for j in 0..ncons {
        let k0 = (3 * j) % nvars;
        let k1 = (k0 + 1) % nvars;
        // a: x_k0 ; b: x_k1 → c must equal a*b = z_k0 * z_k1
        let a = vec![(k0, Scalar::ONE)];
        let b = vec![(k1, Scalar::ONE)];
        let prod = z[k0] * z[k1];
        // Represent prod as a linear combo of a couple of vars: define c using
        // an extra free variable we control. Simplest: c: x_(k2) with
        // z_(k2)=prod — but k2 may be shared. Instead, add c = prod·x_0 (=prod).
        let c = vec![(0, prod)]; // x_0 = 1 ⇒ c-side = prod
        constraints.push(Constraint { a, b, c });
    }
    let r1cs = R1CS { num_variables: nvars, num_public: 1, constraints, domain: Domain::RootsOfUnity };
    (r1cs, z)
}

#[test]
fn fft_witness_matches_dense_witness_satisfiable() {
    for (nv, m) in [(16usize, 16usize), (16, 10), (32, 20), (9, 3), (40, 33)] {
        let (r1cs, z) = satisfiable_roots_r1cs(nv, m, 0xF00 ^ ((nv as u64) << 8) ^ m as u64);
        let qap = r1cs.qap();
        let dense = qap::witness_polys(&qap, &z);
        let fft = qap::witness_polys_fft(&qap, &z);
        let eq = |p: &Poly, q: &Poly| {
            let d = p.deg().max(q.deg());
            (0..=d).all(|i| {
                p.0.get(i).copied().unwrap_or(Scalar::ZERO) == q.0.get(i).copied().unwrap_or(Scalar::ZERO)
            })
        };
        assert!(eq(&dense.a, &fft.a), "A mismatch (nv={nv},m={m})");
        assert!(eq(&dense.b, &fft.b), "B mismatch (nv={nv},m={m})");
        assert!(eq(&dense.c, &fft.c), "C mismatch (nv={nv},m={m})");
        assert!(eq(&dense.ht, &fft.ht), "HT mismatch (nv={nv},m={m})");
        assert!(eq(&dense.h, &fft.h), "H mismatch (nv={nv},m={m})");
        // And the fundamental identity HT = A·B − C must hold.
        let lhs = fft.a.mul(&fft.b).sub(&fft.c);
        assert!(eq(&lhs, &fft.ht), "identity HT == A·B−C failed");
    }
}

// ===========================================================================
// 5. Full Groth16 roundtrip on random satisfiable circuits (roots domain)
// ===========================================================================

#[test]
fn groth16_roundtrip_random_satisfiable_roots() {
    for (nv, m) in [(8usize, 8usize), (16, 16), (16, 5), (32, 33), (24, 20)] {
        let (r1cs, z) = satisfiable_roots_r1cs(nv, m, 0xBEE ^ ((nv as u64) << 8) ^ m as u64);
        // The circuit has 1 public var (index 0 = const, so num_public=1 means no
        // additional public inputs; set num_public=1 to keep it simple).
        let qap = r1cs.qap();
        let pk = groth16::setup(&qap, "verify-seed");
        let (proof, pub_in) = groth16::prove(&pk, &qap, &z);
        assert_eq!(groth16::verify(&pk.vk, &pub_in, &proof), Ok(()),
            "roundtrip must verify (nv={nv},m={m})");
    }
}

#[test]
fn groth16_rejects_tampered_proof_and_witness() {
    let (r1cs, z) = satisfiable_roots_r1cs(16, 12, 0x5EED);
    let qap = r1cs.qap();
    let pk = groth16::setup(&qap, "verify-seed");
    let (proof, pub_in) = groth16::prove(&pk, &qap, &z);
    assert_eq!(groth16::verify(&pk.vk, &pub_in, &proof), Ok(()));

    // Tamper the proof's A point.
    let mut bad = proof.clone();
    bad.a = (bls12_381::G1Projective::generator() * Scalar::from(12345u64)).into();
    assert!(groth16::verify(&pk.vk, &pub_in, &bad).is_err(), "tampered proof must fail");

    // Tamper the public inputs.
    let mut bad_in = pub_in.clone();
    if bad_in.is_empty() {
        bad_in.push(Scalar::ONE);
    } else {
        bad_in[0] += Scalar::ONE;
    }
    assert!(groth16::verify(&pk.vk, &bad_in, &proof).is_err(), "tampered public input must fail");

    // A different CRS must reject.
    let pk2 = groth16::setup(&qap, "other-seed");
    assert!(groth16::verify(&pk2.vk, &pub_in, &proof).is_err(), "wrong CRS must fail");
}

// ===========================================================================
// 6. Value-balance key must be BYTE-IDENTICAL to the on-chain baked key
// ===========================================================================

#[test]
fn value_balance_vk_unchanged_after_fft_refactor() {
    // The pallet bakes this key; it MUST NOT change when we add the FFT domain.
    let r1cs = qap::value_balance_r1cs();
    assert_eq!(r1cs.domain, Domain::Consecutive, "value-balance must stay on the consecutive domain");
    let qap = r1cs.qap();
    let pk = groth16::setup(&qap, "testnet-crs-v0");
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    // Values baked in pallet-shielded-pool::groth16::value_balance_key().
    assert_eq!(hex(&pk.vk.alpha_g1.to_compressed()),
        "8b1beb9eaa46f84617b8a487edc7d8430713e5860594857c511c5dd80e4a8c79d89d382955b0ccfd04264efe005f6678");
    assert_eq!(hex(&pk.vk.gamma_abc_g1[0].to_compressed()),
        "af8dd62de36279f1a7d53eb0d0d97dde599e9b142be8ab849cda3f77f2fe2a08c9e7cc795ae9b852eba83c40d176d56b");
    assert_eq!(hex(&pk.vk.gamma_abc_g1[1].to_compressed()),
        "aeae8329b9b6d1d1231ff3c1aef5e86fc629057a7e62a0a69dee016638d49a27a034209c612feac2de59b700a78e3762");
    assert_eq!(hex(&pk.vk.gamma_g2.to_compressed()),
        "810ae3861a991a8ee80a763542e7b7e27711403f088f501c08ae675644a6764c54f1f1e03c4ae3d79a3ca2305a5937c21092ab265bf216c1d65f90164b44ed6093d4a567af4ff52ce8a489729b576094fd91e73ba492b45c30912c482ab4974a");
    assert_eq!(hex(&pk.vk.delta_g2.to_compressed()),
        "858fa473094c8957eda90c0f3a7e8d1424849268c463c8d6d8ecd1a70575768d13861ebf010f604fd2f7bc154d4f7fb004cdeec875473c342f9320144e64a1ede91f7bd82f67d92dbbbb860d78dfd94cf4e9ba6c2c69e4b57cd476693dfdf0db");
    assert_eq!(hex(&pk.vk.beta_g2.to_compressed()),
        "af71521ffa3621e20b79e99a5c1ac5afbe6cf6e4d7008c5c6f77b7065264579af863ac70998d9cde287bab614a8407e807a89a7638059e365914797401405eb4cec1995e3c84dfa23a115c55b832347f7ef8d2d2336f6301f9c4e3903b65b275");
}

// ===========================================================================
// 7. Note-spend circuit: FFT-domain correctness (deep, ignored by default)
// ===========================================================================

#[test]
#[ignore = "full note-spend circuit; run with --release"]
fn note_spend_roundtrip_depth_variants() {
    // NB: every depth has ~the same cost — the blake2s commitment/nullifier hash
    // gates dominate (~121k constraints regardless of merkle depth), so we run a
    // representative depth here. Depth 4 is covered by `note_spend_full_roundtrip`.
    for depth in [0usize] {
        let note = crate::note_spend::tests::test_note(depth);
        let (r1cs, z) = crate::note_spend::note_spend_r1cs(&note);
        assert_eq!(r1cs.domain, Domain::RootsOfUnity);
        assert!(crate::gadget::constraints_hold(&r1cs, &z), "witness must satisfy (depth {depth})");
        let qap = r1cs.qap_sparse();
        let pk = groth16::setup(&qap, "testnet-crs-v0");
        let (proof, pub_in) = groth16::prove(&pk, &qap, &z);
        assert_eq!(groth16::verify(&pk.vk, &pub_in, &proof), Ok(()), "must verify (depth {depth})");
        // Soundness: a tampered public input must be rejected at scale too.
        let mut bad = pub_in.clone();
        if !bad.is_empty() {
            bad[0] += Scalar::ONE;
            assert!(groth16::verify(&pk.vk, &bad, &proof).is_err(), "tampered input must fail (depth {depth})");
        }
    }
}
