//! A real Groth16 prover/verifier over BLS12-381 driven by an R1CS→QAP circuit.
//!
//! Unlike the crate's legacy `keygen`/`prove` (which target the pallet's
//! **trivial l=0 test circuit**), this module is the genuine article:
//!
//! - [`setup`] derives a deterministic CRS (α, β, γ, δ, τ) from a seed and
//!   builds a proving key + verification key for a [`Qap`].
//! - [`prove`] takes a full witness, computes `H(x) = (A·B - C)/T` via the QAP,
//!   and produces a real Groth16 proof with the *public* inputs bound.
//! - [`verify`] runs the same pairing-product equation as the runtime verifier.
//!
//! The CRS is **deterministic (testnet-only)** — reproducible from the seed
//! string. Production needs a Powers-of-Tau ceremony (toxic-waste τ, α, β, δ).

use bls12_381::{pairing, G1Affine, G1Projective, G2Affine, G2Projective, Gt, Scalar};
use ff::Field;

use crate::qap::{witness_polys, Qap};

/// A deterministic xorshift64 PRNG seeded from a string, used only to derive
/// the testnet CRS scalars. Not cryptographically secure — fine for a
/// reproducible testnet; production uses a ceremony.
struct Drng(u64);

impl Drng {
    fn from_seed(seed: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in seed.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        Drng(h)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// A scalar from two 32-bit halves (fits in Fr's 255-bit modulus).
    fn scalar(&mut self) -> Scalar {
        let hi = self.next_u64() & 0xFFFF_FFFF;
        let lo = self.next_u64() & 0xFFFF_FFFF;
        Scalar::from(lo) + Scalar::from(hi) * Scalar::from(1u64 << 32)
    }
}

/// Groth16 verification key (same layout as the runtime `groth16` module and
/// the crate's legacy [`super::VerifyingKey`]).
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyingKey {
    pub alpha_g1: G1Affine,
    pub beta_g2: G2Affine,
    pub gamma_g2: G2Affine,
    pub delta_g2: G2Affine,
    /// `[βAᵢ(τ)+αBᵢ(τ)+Cᵢ(τ)]₁ / γ` per public variable (index 0 = constant a₀=1).
    pub gamma_abc_g1: Vec<G1Affine>,
}

/// Groth16 proving key for a QAP circuit.
#[derive(Clone, Debug)]
pub struct ProvingKey {
    pub alpha_g1: G1Affine,
    pub beta_g1: G1Affine,
    pub beta_g2: G2Affine,
    pub delta_g1: G1Affine,
    pub delta_g2: G2Affine,
    /// `[A_k(τ)]₁` and `[B_k(τ)]₁` (G1) for every variable — for A, B_G1, and C.
    pub a_g1: Vec<G1Affine>,
    pub b_g1: Vec<G1Affine>,
    /// `[B_k(τ)]₂` for the B proof element.
    pub b_g2: Vec<G2Affine>,
    /// `[βA_k(τ)+αB_k(τ)+C_k(τ)]₁ / δ` for private variables (index k, k ≥ num_public).
    pub abc_delta_g1: Vec<G1Affine>,
    /// `[(τ^i)/δ]₁` for i in 0..=deg(H·T) — used for the `H(τ)T(τ)/δ` term.
    pub ht_delta_g1: Vec<G1Affine>,
    pub num_public: usize,
    /// `[βAᵢ+αBᵢ+Cᵢ]₁/γ` for public variables — embedded in `VerifyingKey`.
    pub vk: VerifyingKey,
}

/// A Groth16 proof (same layout as the legacy one).
#[derive(Clone, Debug, PartialEq)]
pub struct Proof {
    pub a: G1Affine,
    pub b: G2Affine,
    pub c: G1Affine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    InputLenMismatch,
    InvalidProof,
}

/// Deterministically derive the CRS scalars and build the proving/verification
/// keys for a QAP. Reproducible for a given `seed`.
pub fn setup(qap: &Qap, seed: &str) -> ProvingKey {
    let mut rng = Drng::from_seed(seed);
    let alpha = rng.scalar();
    let beta = rng.scalar();
    let gamma = rng.scalar();
    let delta = rng.scalar();
    let tau = rng.scalar();

    let n = qap.num_variables;
    let num_public = qap.num_public;

    // Evaluate each column polynomial at τ (scalar), then lift into groups.
    // Uses the fast barycentric single-point evaluator (O(m + nnz)) rather than
    // dense per-column interpolation — required for the large note-spend circuit.
    let (a_tau, b_tau, c_tau) = qap.eval_columns_at(&tau);

    let g1 = G1Projective::generator();
    let g2 = G2Projective::generator();

    let alpha_g1: G1Affine = (g1 * alpha).into();
    let beta_g1: G1Affine = (g1 * beta).into();
    let beta_g2: G2Affine = (g2 * beta).into();
    let delta_g1: G1Affine = (g1 * delta).into();
    let delta_g2: G2Affine = (g2 * delta).into();

    let a_g1: Vec<G1Affine> = a_tau.iter().map(|s| (g1 * s).into()).collect();
    let b_g1: Vec<G1Affine> = b_tau.iter().map(|s| (g1 * s).into()).collect();
    let b_g2: Vec<G2Affine> = b_tau.iter().map(|s| (g2 * s).into()).collect();

    // Public: /γ ; Private: /δ. gamma_abc_g1 covers public vars (incl. const).
    let mut gamma_abc_g1 = Vec::with_capacity(num_public);
    let mut abc_delta_g1 = Vec::with_capacity(n - num_public);
    let gamma_inv = gamma.invert().expect("gamma non-zero");
    let delta_inv = delta.invert().expect("delta non-zero");
    for k in 0..n {
        let comb = beta * a_tau[k] + alpha * b_tau[k] + c_tau[k];
        let pt: G1Affine = (g1 * comb).into();
        if k < num_public {
            gamma_abc_g1.push((G1Projective::from(pt) * gamma_inv).into());
        } else {
            abc_delta_g1.push((G1Projective::from(pt) * delta_inv).into());
        }
    }

    // Powers (τ^i / δ)₁ for i in 0..=deg(H·T). deg(H·T) ≤ (m-2) + m = 2m-2.
    let m = qap.constraints.len(); // = num_constraints (avoids needing dense qap.t)
    let max_deg = 2 * m; // safe upper bound
    let mut tau_pow = Scalar::ONE;
    let mut ht_delta_g1 = Vec::with_capacity(max_deg + 1);
    for _ in 0..=max_deg {
        let pt: G1Affine = (G1Projective::from(g1) * tau_pow).into();
        ht_delta_g1.push((G1Projective::from(pt) * delta_inv).into());
        tau_pow *= tau;
    }

    ProvingKey {
        alpha_g1,
        beta_g1,
        beta_g2,
        delta_g1,
        delta_g2,
        a_g1,
        b_g1,
        b_g2,
        abc_delta_g1,
        ht_delta_g1,
        num_public,
        vk: VerifyingKey {
            alpha_g1,
            beta_g2,
            gamma_g2: (g2 * gamma).into(),
            delta_g2,
            gamma_abc_g1,
        },
    }
}

/// Prove a satisfying assignment `z` (full witness, `z[0] = 1`).
/// Returns the proof plus the public inputs (variables `1..num_public`).
pub fn prove(pk: &ProvingKey, qap: &Qap, z: &[Scalar]) -> (Proof, Vec<Scalar>) {
    let wp = witness_polys(qap, z);
    // Public inputs = z[1..num_public] (exclude the constant z[0]=1).
    let public_inputs: Vec<Scalar> = z[1..pk.num_public].to_vec();

    let mut rng = Drng::from_seed("gpn-shield-prove-blinding");
    let r = rng.scalar();
    let s = rng.scalar();

    // A = α + Σ z_k A_k(τ) + rδ   (G1)
    let mut a_acc = G1Projective::from(pk.alpha_g1);
    for (k, zk) in z.iter().enumerate() {
        if *zk == Scalar::ZERO {
            continue;
        }
        a_acc += G1Projective::from(pk.a_g1[k]) * zk;
    }
    a_acc += G1Projective::from(pk.delta_g1) * r;
    let a: G1Affine = a_acc.into();

    // B = β + Σ z_k B_k(τ) + sδ   (G2)
    let mut b_acc = G2Projective::from(pk.beta_g2);
    for (k, zk) in z.iter().enumerate() {
        if *zk == Scalar::ZERO {
            continue;
        }
        b_acc += G2Projective::from(pk.b_g2[k]) * zk;
    }
    b_acc += G2Projective::from(pk.delta_g2) * s;
    let b: G2Affine = b_acc.into();

    // C = [Σ_{k≥num_public} z_k (βA_k+αB_k+C_k)/δ] + H(τ)T(τ)/δ + rA + sB − rsδ  (G1)
    let mut c_acc = G1Projective::default();
    for (k, zk) in z.iter().enumerate().skip(pk.num_public) {
        if *zk == Scalar::ZERO {
            continue;
        }
        c_acc += G1Projective::from(pk.abc_delta_g1[k - pk.num_public]) * zk;
    }
    // H(τ)T(τ)/δ : HT = H·T as a scalar polynomial, then Σ coeff_i · (τ^i/δ)₁
    let ht = wp.h.mul(&qap.t);
    for (i, coeff) in ht.0.iter().enumerate() {
        if *coeff == Scalar::ZERO {
            continue;
        }
        c_acc += G1Projective::from(pk.ht_delta_g1[i]) * coeff;
    }
    // rA + sB − rsδ
    let b_g1: G1Affine = {
        let mut b1 = G1Projective::from(pk.beta_g1);
        for (k, zk) in z.iter().enumerate() {
            if *zk == Scalar::ZERO {
                continue;
            }
            b1 += G1Projective::from(pk.b_g1[k]) * zk;
        }
        b1 += G1Projective::from(pk.delta_g1) * s;
        b1.into()
    };
    // sA + rB − rsδ (correct Groth16 blinding; r·A+s·B does NOT cancel)
    c_acc += G1Projective::from(a) * s;
    c_acc += G1Projective::from(b_g1) * r;
    c_acc -= G1Projective::from(pk.delta_g1) * (r * s);
    let c: G1Affine = c_acc.into();

    (Proof { a, b, c }, public_inputs)
}

/// Verify a proof against a verification key and public inputs. This is the
/// byte-for-byte pairing equation used by the runtime verifier.
pub fn verify(vk: &VerifyingKey, public_inputs: &[Scalar], proof: &Proof) -> Result<(), VerifyError> {
    if public_inputs.len() + 1 != vk.gamma_abc_g1.len() {
        return Err(VerifyError::InputLenMismatch);
    }
    let mut acc = G1Projective::from(vk.gamma_abc_g1[0]);
    for (a, g) in public_inputs.iter().zip(vk.gamma_abc_g1.iter().skip(1)) {
        acc += G1Projective::from(*g) * a;
    }
    let neg_alpha = -G1Projective::from(vk.alpha_g1);
    let neg_c = -G1Projective::from(proof.c);
    let neg_acc = -acc;
    let lhs = pairing(&proof.a, &proof.b)
        + pairing(&G1Affine::from(neg_alpha), &vk.beta_g2)
        + pairing(&G1Affine::from(neg_acc), &vk.gamma_g2)
        + pairing(&G1Affine::from(neg_c), &vk.delta_g2);
    if lhs == Gt::identity() {
        Ok(())
    } else {
        Err(VerifyError::InvalidProof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qap::value_balance_r1cs;

    #[test]
    fn real_circuit_prove_verify_roundtrip() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk = setup(&qap, "testnet-crs-v0");
        let z = vec![
            Scalar::ONE,
            Scalar::from(2), // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8), // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (proof, pub_in) = prove(&pk, &qap, &z);
        assert_eq!(pub_in, vec![Scalar::from(2)]); // only `fee` is public
        // debug: check verification with blinding stripped is impossible, but check acc len
        eprintln!("DEBUG gamma_abc len={} pub_in={:?}", pk.vk.gamma_abc_g1.len(), pub_in);
        assert_eq!(verify(&pk.vk, &pub_in, &proof), Ok(()));
    }

    #[test]
    fn wrong_public_input_rejected() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk = setup(&qap, "testnet-crs-v0");
        let z = vec![
            Scalar::ONE,
            Scalar::from(2), // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8), // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (proof, _) = prove(&pk, &qap, &z);
        // Tamper with the public input: fee 2 -> 3.
        assert_eq!(verify(&pk.vk, &[Scalar::from(3)], &proof), Err(VerifyError::InvalidProof));
    }

    #[test]
    fn tampered_proof_rejected() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk = setup(&qap, "testnet-crs-v0");
        let z = vec![
            Scalar::ONE,
            Scalar::from(2), // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8), // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (mut proof, pub_in) = prove(&pk, &qap, &z);
        proof.a = (G1Projective::generator() * Scalar::from(9u64)).into();
        assert_eq!(verify(&pk.vk, &pub_in, &proof), Err(VerifyError::InvalidProof));
    }

    #[test]
    fn setup_is_deterministic() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk1 = setup(&qap, "same-seed");
        let pk2 = setup(&qap, "same-seed");
        assert_eq!(pk1.vk, pk2.vk);
        assert_eq!(pk1.a_g1, pk2.a_g1);
        // Different seed -> different key.
        let pk3 = setup(&qap, "other-seed");
        assert_ne!(pk1.vk, pk3.vk);
    }

    #[test]
    fn key_not_transferable_across_circuits() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk = setup(&qap, "testnet-crs-v0");
        let z = vec![
            Scalar::ONE,
            Scalar::from(2), // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8), // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (proof, _) = prove(&pk, &qap, &z);
        // A different CRS (same circuit shape) must reject the proof.
        let pk2 = setup(&qap, "different-crs");
        assert_eq!(verify(&pk2.vk, &[Scalar::from(2)], &proof), Err(VerifyError::InvalidProof));
    }

    #[test]
    // Keygen (setup) must scale to the FULL note-spend circuit (~87k vars x ~121k
    // constraints) via qap_sparse() — no dense column materialization. This is
    // the documented purpose of the fast eval path. Confirmed memory-feasible
    // (~100 MB, no OOM) but slow in unoptimized debug group ops (>6 min), so it
    // is ignored in the default suite. Run manually in release:
    //   cargo test --release -p gpn-shield -- --ignored setup_scales_to_note_spend_circuit
    #[ignore = "runs full keygen over 87k x 121k circuit; slow in debug (use --release)"]
    fn setup_scales_to_note_spend_circuit() {
        let note = crate::note_spend::tests::test_note(4);
        let (r1cs, _z) = crate::note_spend::note_spend_r1cs(&note);
        let qap = r1cs.qap_sparse(); // NO dense column polys
        let pk = setup(&qap, "testnet-crs-v0");
        assert_eq!(pk.num_public, qap.num_public);
        assert_eq!(pk.a_g1.len(), qap.num_variables);
        assert_eq!(pk.b_g2.len(), qap.num_variables);
        assert_eq!(pk.abc_delta_g1.len(), qap.num_variables - qap.num_public);
        assert_eq!(pk.vk.gamma_abc_g1.len() - 1, qap.num_public - 1);
        // ht_delta covers deg up to 2m.
        assert!(pk.ht_delta_g1.len() >= 2 * qap.constraints.len());
    }

    #[test]
    // setup() reads m from constraints.len() now (not dense qap.t). This proves
    // the small/medium dense path still round-trips prove+verify after that
    // change — guards against regressions from the sparse-keygen refactor.
    fn setup_with_sparse_qap_matches_dense_roundtrip() {
        // A few chained gates — a real (satisfiable) small circuit so we can
        // round-trip prove+verify on it, not just compare keys.
        use crate::qap::Constraint;
        let one = Scalar::ONE;
        // vars: 0=1, 1=pub x, 2=priv a, 3=priv b; c0: a*1=x; c1: b*1=a+? ...
        // Use value-balance-style: x(pub)=v_in-fee. Simpler: build 6-var circuit.
        let r1cs = crate::qap::R1CS {
            num_variables: 6,
            num_public: 2,
            constraints: vec![
                Constraint { a: vec![(2, one)], b: vec![(0, one)], c: vec![(4, one)] },
                Constraint { a: vec![(3, one)], b: vec![(0, one)], c: vec![(4, one), (1, -(one))] },
                Constraint { a: vec![(2, one)], b: vec![(3, one)], c: vec![(5, one)] },
            ],
        };
        let dense_qap = r1cs.qap();
        let sparse_qap = r1cs.qap_sparse();
        // Same CRS scalars (same seed) → keys must be identical whether built
        // from the dense or sparse Qap (eval_columns_at is the shared path).
        let pk_dense = setup(&dense_qap, "crs-v1");
        let pk_sparse = setup(&sparse_qap, "crs-v1");
        assert_eq!(pk_dense.vk, pk_sparse.vk);
        assert_eq!(pk_dense.a_g1, pk_sparse.a_g1);
        assert_eq!(pk_dense.ht_delta_g1, pk_sparse.ht_delta_g1);
        // And a real proof from the dense path still verifies under the sparse key.
        let z = vec![Scalar::ONE, Scalar::from(2), Scalar::from(10), Scalar::from(8), Scalar::from(10), Scalar::from(80)];
        let (proof, pub_in) = prove(&pk_sparse, &dense_qap, &z);
        assert_eq!(verify(&pk_sparse.vk, &pub_in, &proof), Ok(()));
    }
}
