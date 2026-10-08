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
use rayon::prelude::*;

use crate::ec_ops;

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

/// Key-generation strategy for [`setup`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeygenMode {
    /// Fast fixed-base windowed tables. Fine for `prove` (the witness is the
    /// prover's own secret), but the shared `mul` path skips zero digits and
    /// indexes its table by secret bits — a timing/cache side channel on the
    /// scalar. **Do not** use this for the CRS secret scalars
    /// (α, β, γ, δ, τ) when those must stay confidential.
    Fast,
    /// Branchless constant-time fixed-base path ([`ec_ops::fixed_base_g1_gen_ct`]
    /// et al.). Use for keygen/setup where the CRS toxic-waste scalars must not
    /// leak through control flow or memory-access patterns.
    /// Produces **byte-identical** keys to [`KeygenMode::Fast`].
    ConstantTime,
}

/// Deterministically derive the CRS scalars and build the proving/verification
/// keys for a QAP. Reproducible for a given `seed`.
///
/// Uses the fast fixed-base path ([`KeygenMode::Fast`]). For a ceremony / any
/// run where the secret CRS scalars must not leak via side channels, call
/// [`setup_constant_time`] instead (same output, constant-time control flow).
pub fn setup(qap: &Qap, seed: &str) -> ProvingKey {
    setup_with_mode(qap, seed, KeygenMode::Fast)
}

/// Constant-time keygen. Byte-identical keys to [`setup`], but every scalar
/// multiplication that touches a **secret** CRS scalar uses the branchless
/// fixed-base path (full-table constant-time lookup, fixed group-op count).
///
/// Recommended for the production Powers-of-Tau / ceremony setup, where the
/// toxic waste α, β, γ, δ, τ would otherwise be exposed to a co-located
/// timing/cache attacker.
pub fn setup_constant_time(qap: &Qap, seed: &str) -> ProvingKey {
    setup_with_mode(qap, seed, KeygenMode::ConstantTime)
}

fn setup_with_mode(qap: &Qap, seed: &str, mode: KeygenMode) -> ProvingKey {
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

    // Fixed-base lifts, routed through the constant-time path when requested.
    // Both modes yield byte-identical group elements.
    let fb_g1 = |s: &[Scalar]| -> Vec<G1Affine> {
        match mode {
            KeygenMode::Fast => ec_ops::fixed_base_g1_gen(s),
            KeygenMode::ConstantTime => ec_ops::fixed_base_g1_gen_ct(s),
        }
    };
    let fb_g1_scaled = |s: &[Scalar], inv: &Scalar| -> Vec<G1Affine> {
        match mode {
            KeygenMode::Fast => ec_ops::fixed_base_g1_gen_scaled(s, inv),
            KeygenMode::ConstantTime => ec_ops::fixed_base_g1_gen_scaled_ct(s, inv),
        }
    };
    let fb_g2 = |s: &[Scalar]| -> Vec<G2Affine> {
        match mode {
            KeygenMode::Fast => ec_ops::fixed_base_g2_gen(s),
            KeygenMode::ConstantTime => ec_ops::fixed_base_g2_gen_ct(s),
        }
    };
    let single_g1 = |s: Scalar| -> G1Affine {
        match mode {
            KeygenMode::Fast => (g1 * s).into(),
            KeygenMode::ConstantTime => fb_g1(&[s])[0],
        }
    };
    let single_g2 = |s: Scalar| -> G2Affine {
        match mode {
            KeygenMode::Fast => (g2 * s).into(),
            KeygenMode::ConstantTime => fb_g2(&[s])[0],
        }
    };

    let alpha_g1: G1Affine = single_g1(alpha);
    let beta_g1: G1Affine = single_g1(beta);
    let beta_g2: G2Affine = single_g2(beta);
    let delta_g1: G1Affine = single_g1(delta);
    let delta_g2: G2Affine = single_g2(delta);

    // These are all *fixed-base* multiplications (base = g1/g2). Use a shared
    // windowed precomputation table — ~8× faster than per-element `g*s`, and
    // byte-identical output. Each helper parallelizes across cores.
    let a_g1: Vec<G1Affine> = fb_g1(&a_tau);
    let b_g1: Vec<G1Affine> = fb_g1(&b_tau);
    let b_g2: Vec<G2Affine> = fb_g2(&b_tau);

    // Public: /γ ; Private: /δ. `comb_k = β·a_k + α·b_k + c_k`, then a single
    // fixed-base multiply with the /γ or /δ scalar folded in.
    let gamma_inv = gamma.invert().expect("gamma non-zero");
    let delta_inv = delta.invert().expect("delta non-zero");
    let combs: Vec<Scalar> = (0..n)
        .into_par_iter()
        .map(|k| beta * a_tau[k] + alpha * b_tau[k] + c_tau[k])
        .collect();
    let gamma_abc_g1: Vec<G1Affine> = fb_g1_scaled(&combs[..num_public], &gamma_inv);
    let abc_delta_g1: Vec<G1Affine> = fb_g1_scaled(&combs[num_public..], &delta_inv);

    // Powers (τ^i / δ)₁ for i in 0..=deg(H·T). deg(H·T) ≤ 2·domain_size − 2.
    // `domain_size` = constraints for the consecutive domain; next-pow2 ≥ m for
    // RootsOfUnity (padding rows included). Use 2·domain_size as a safe bound.
    // Scalar powers are cheap and sequential; the point lifts use the base table.
    let max_deg = 2 * qap.domain_size.max(1);
    let mut tau_pows = Vec::with_capacity(max_deg + 1);
    let mut tau_pow = Scalar::ONE;
    for _ in 0..=max_deg {
        tau_pows.push(tau_pow);
        tau_pow *= tau;
    }
    let ht_delta_g1: Vec<G1Affine> = fb_g1_scaled(&tau_pows, &delta_inv);

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
            gamma_g2: single_g2(gamma),
            delta_g2,
            gamma_abc_g1,
        },
    }
}

/// Prove a satisfying assignment `z` (full witness, `z[0] = 1`).
/// Returns the proof plus the public inputs (variables `1..num_public`).
pub fn prove(pk: &ProvingKey, qap: &Qap, z: &[Scalar]) -> (Proof, Vec<Scalar>) {
    // Pick the witness-H path: FFT-native for large RootsOfUnity circuits,
    // dense for the default consecutive-integer domain (small circuits).
    let wp = match qap.domain {
        crate::qap::Domain::RootsOfUnity => crate::qap::witness_polys_fft(qap, z),
        crate::qap::Domain::Consecutive => witness_polys(qap, z),
    };
    // Public inputs = z[1..num_public] (exclude the constant z[0]=1).
    let public_inputs: Vec<Scalar> = z[1..pk.num_public].to_vec();

    let mut rng = Drng::from_seed("gpn-shield-prove-blinding");
    let r = rng.scalar();
    let s = rng.scalar();

    // A = α + Σ z_k A_k(τ) + rδ   (G1)
    // The Σ z_k [A_k(τ)]₁ term is a multi-scalar product — use Pippenger MSM
    // (≈10× fewer group ops); bit-identical to the naive sum.
    let a_sum: G1Projective = ec_ops::msm_g1(&pk.a_g1, z);
    let mut a_acc = G1Projective::from(pk.alpha_g1) + a_sum;
    a_acc += G1Projective::from(pk.delta_g1) * r;
    let a: G1Affine = a_acc.into();

    // B = β + Σ z_k B_k(τ) + sδ   (G2)
    let b_sum: G2Projective = ec_ops::msm_g2(&pk.b_g2, z);
    let mut b_acc = G2Projective::from(pk.beta_g2) + b_sum;
    b_acc += G2Projective::from(pk.delta_g2) * s;
    let b: G2Affine = b_acc.into();

    // C = [Σ_{k≥num_public} z_k (βA_k+αB_k+C_k)/δ] + H(τ)T(τ)/δ + rA + sB − rsδ  (G1)
    let c_priv: G1Projective = ec_ops::msm_g1(&pk.abc_delta_g1, &z[pk.num_public..]);
    let mut c_acc = c_priv;
    // H(τ)T(τ)/δ : HT = H·T as a scalar polynomial, then Σ coeff_i · (τ^i/δ)₁
    // Use the precomputed HT (= A·B − C) so no O(m²) product is needed.
    let ht = &wp.ht;
    let ht_len = ht.0.len();
    let ht_sum: G1Projective = ec_ops::msm_g1(&pk.ht_delta_g1[..ht_len], &ht.0);
    c_acc += ht_sum;
    // rA + sB − rsδ
    let b1_sum: G1Projective = ec_ops::msm_g1(&pk.b_g1, z);
    let b_g1: G1Affine =
        (G1Projective::from(pk.beta_g1) + b1_sum + G1Projective::from(pk.delta_g1) * s).into();
    // sA + rB − rsδ (correct Groth16 blinding; r·A+s·B does NOT cancel)
    c_acc += G1Projective::from(a) * s;
    c_acc += G1Projective::from(b_g1) * r;
    c_acc -= G1Projective::from(pk.delta_g1) * (r * s);
    let c: G1Affine = c_acc.into();

    (Proof { a, b, c }, public_inputs)
}

/// Verify a proof against a verification key and public inputs. This is the
/// byte-for-byte pairing equation used by the runtime verifier.
pub fn verify(
    vk: &VerifyingKey,
    public_inputs: &[Scalar],
    proof: &Proof,
) -> Result<(), VerifyError> {
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

// ---------------------------------------------------------------------------
// Trusted-setup ceremony support
// ---------------------------------------------------------------------------
//
// [`ProvingKey`] stores only the *combined* `[βA_k+αB_k+C_k]/δ` (and `/γ`) term,
// which is enough to prove but **not** enough for a multi-party phase-2
// contribution: a contributor must fold a fresh `α'`, `β'`, `γ'`, `δ'` into the
// CRS *without* ever learning the aggregate `α,β,γ,δ`. That is only possible if
// the `βA_k`, `αB_k` and `C_k` pieces are kept separate, so each can be scaled
// independently (the standard Groth16 MPC structure).
//
// `setup_ceremony` is the genesis step: it derives the secrets from `seed` and
// emits the split CRS. `gpn-shield`'s ceremony module folds contributions into
// it. τ is fixed here (see `design-ceremony.md` for the powers-of-tau extension).

/// Split CRS components emitted by [`setup_ceremony`] — the phase-2 genesis.
#[derive(Clone, Debug)]
pub struct CeremonyKeys {
    pub alpha_g1: G1Affine,
    pub beta_g1: G1Affine,
    pub beta_g2: G2Affine,
    pub gamma_g2: G2Affine,
    pub delta_g1: G1Affine,
    pub delta_g2: G2Affine,
    /// `[A_k(τ)]₁`, `[B_k(τ)]₁`, `[B_k(τ)]₂` for every variable (τ fixed).
    pub a_g1: Vec<G1Affine>,
    pub b_g1: Vec<G1Affine>,
    pub b_g2: Vec<G2Affine>,
    /// `[(τ^i)/δ]₁` for i in `0..=deg(H·T)`.
    pub ht_delta_g1: Vec<G1Affine>,
    /// Private (`k >= num_public`) split combined terms (each `/δ`):
    /// `[βA_k/δ]₁`, `[αB_k/δ]₁`, `[C_k/δ]₁`.
    pub abc_beta_delta: Vec<G1Affine>,
    pub abc_alpha_delta: Vec<G1Affine>,
    pub abc_c_delta: Vec<G1Affine>,
    /// Public (`0..num_public`) split combined terms (each `/γ`):
    /// `[βA_i/γ]₁`, `[αB_i/γ]₁`, `[C_i/γ]₁`.
    pub gabc_beta_gamma: Vec<G1Affine>,
    pub gabc_alpha_gamma: Vec<G1Affine>,
    pub gabc_c_gamma: Vec<G1Affine>,
    pub num_public: usize,
    pub num_variables: usize,
    pub domain_size: usize,
}

/// Build the split (phase-2 genesis) CRS for `qap` from a `seed`.
///
/// Uses the **constant-time** keygen path ([`KeygenMode::ConstantTime`]) because
/// the derived scalars are the CRS toxic waste; the group elements are
/// byte-identical to the fast path.
///
/// Panics if any of the derived secrets is zero (astronomically unlikely).
pub fn setup_ceremony(qap: &Qap, seed: &str) -> CeremonyKeys {
    let mut rng = Drng::from_seed(seed);
    let alpha = rng.scalar();
    let beta = rng.scalar();
    let gamma = rng.scalar();
    let delta = rng.scalar();
    let tau = rng.scalar();

    let n = qap.num_variables;
    let num_public = qap.num_public;
    let (a_tau, b_tau, c_tau) = qap.eval_columns_at(&tau);

    let fb_g1 = ec_ops::fixed_base_g1_gen_ct;
    let fb_g2 = ec_ops::fixed_base_g2_gen_ct;
    let fb_g1_scaled = ec_ops::fixed_base_g1_gen_scaled_ct;

    let alpha_g1 = fb_g1(&[alpha])[0];
    let beta_g1 = fb_g1(&[beta])[0];
    let beta_g2 = fb_g2(&[beta])[0];
    let delta_g1 = fb_g1(&[delta])[0];
    let delta_g2 = fb_g2(&[delta])[0];
    let gamma_g2 = fb_g2(&[gamma])[0];

    let a_g1: Vec<G1Affine> = fb_g1(&a_tau);
    let b_g1: Vec<G1Affine> = fb_g1(&b_tau);
    let b_g2: Vec<G2Affine> = fb_g2(&b_tau);

    let gamma_inv = gamma.invert().expect("gamma non-zero");
    let delta_inv = delta.invert().expect("delta non-zero");

    // Split `β·a_k`, `α·b_k`, `c_k` per variable, then fold the `/δ` (private)
    // or `/γ` (public) factor into a single fixed-base multiply.
    let priv_r: Vec<usize> = (num_public..n).collect();
    let beta_a_priv: Vec<Scalar> = priv_r.iter().map(|&k| beta * a_tau[k]).collect();
    let alpha_b_priv: Vec<Scalar> = priv_r.iter().map(|&k| alpha * b_tau[k]).collect();
    let c_priv: Vec<Scalar> = priv_r.iter().map(|&k| c_tau[k]).collect();
    let abc_beta_delta = fb_g1_scaled(&beta_a_priv, &delta_inv);
    let abc_alpha_delta = fb_g1_scaled(&alpha_b_priv, &delta_inv);
    let abc_c_delta = fb_g1_scaled(&c_priv, &delta_inv);

    let pub_r: Vec<usize> = (0..num_public).collect();
    let beta_a_pub: Vec<Scalar> = pub_r.iter().map(|&k| beta * a_tau[k]).collect();
    let alpha_b_pub: Vec<Scalar> = pub_r.iter().map(|&k| alpha * b_tau[k]).collect();
    let c_pub: Vec<Scalar> = pub_r.iter().map(|&k| c_tau[k]).collect();
    let gabc_beta_gamma = fb_g1_scaled(&beta_a_pub, &gamma_inv);
    let gabc_alpha_gamma = fb_g1_scaled(&alpha_b_pub, &gamma_inv);
    let gabc_c_gamma = fb_g1_scaled(&c_pub, &gamma_inv);

    // `[(τ^i)/δ]₁` up to `2·domain_size` (deg(H·T) ≤ 2·domain_size − 2).
    let max_deg = 2 * qap.domain_size.max(1);
    let mut tau_pows = Vec::with_capacity(max_deg + 1);
    let mut tau_pow = Scalar::ONE;
    for _ in 0..=max_deg {
        tau_pows.push(tau_pow);
        tau_pow *= tau;
    }
    let ht_delta_g1 = fb_g1_scaled(&tau_pows, &delta_inv);

    CeremonyKeys {
        alpha_g1,
        beta_g1,
        beta_g2,
        gamma_g2,
        delta_g1,
        delta_g2,
        a_g1,
        b_g1,
        b_g2,
        ht_delta_g1,
        abc_beta_delta,
        abc_alpha_delta,
        abc_c_delta,
        gabc_beta_gamma,
        gabc_alpha_gamma,
        gabc_c_gamma,
        num_public,
        num_variables: n,
        domain_size: qap.domain_size,
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
            Scalar::from(2),  // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8),  // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (proof, pub_in) = prove(&pk, &qap, &z);
        assert_eq!(pub_in, vec![Scalar::from(2)]); // only `fee` is public
                                                   // debug: check verification with blinding stripped is impossible, but check acc len
        eprintln!(
            "DEBUG gamma_abc len={} pub_in={:?}",
            pk.vk.gamma_abc_g1.len(),
            pub_in
        );
        assert_eq!(verify(&pk.vk, &pub_in, &proof), Ok(()));
    }

    #[test]
    fn wrong_public_input_rejected() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk = setup(&qap, "testnet-crs-v0");
        let z = vec![
            Scalar::ONE,
            Scalar::from(2),  // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8),  // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (proof, _) = prove(&pk, &qap, &z);
        // Tamper with the public input: fee 2 -> 3.
        assert_eq!(
            verify(&pk.vk, &[Scalar::from(3)], &proof),
            Err(VerifyError::InvalidProof)
        );
    }

    #[test]
    fn tampered_proof_rejected() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk = setup(&qap, "testnet-crs-v0");
        let z = vec![
            Scalar::ONE,
            Scalar::from(2),  // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8),  // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (mut proof, pub_in) = prove(&pk, &qap, &z);
        proof.a = (G1Projective::generator() * Scalar::from(9u64)).into();
        assert_eq!(
            verify(&pk.vk, &pub_in, &proof),
            Err(VerifyError::InvalidProof)
        );
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
    fn constant_time_setup_matches_fast_setup_bytes() {
        // The constant-time keygen path must produce byte-identical keys to the
        // fast path (same group elements), so on-chain compat is preserved.
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        for seed in ["testnet-crs-v0", "crs-v1", "other-seed"] {
            let fast = setup(&qap, seed);
            let ct = setup_constant_time(&qap, seed);
            assert_eq!(fast.vk, ct.vk, "vk mismatch (seed={seed})");
            assert_eq!(fast.alpha_g1, ct.alpha_g1);
            assert_eq!(fast.beta_g1, ct.beta_g1);
            assert_eq!(fast.beta_g2, ct.beta_g2);
            assert_eq!(fast.delta_g1, ct.delta_g1);
            assert_eq!(fast.delta_g2, ct.delta_g2);
            assert_eq!(fast.a_g1, ct.a_g1);
            assert_eq!(fast.b_g1, ct.b_g1);
            assert_eq!(fast.b_g2, ct.b_g2);
            assert_eq!(fast.abc_delta_g1, ct.abc_delta_g1);
            assert_eq!(fast.ht_delta_g1, ct.ht_delta_g1);
            // A proof made with the CT key verifies exactly as with the fast key.
            let z = vec![
                Scalar::ONE,
                Scalar::from(2),
                Scalar::from(10),
                Scalar::from(8),
                Scalar::from(10),
                Scalar::from(80),
            ];
            let (proof, pub_in) = prove(&ct, &qap, &z);
            assert_eq!(verify(&fast.vk, &pub_in, &proof), Ok(()), "seed={seed}");
        }
    }

    #[test]
    // Full Groth16 roundtrip on the REAL note-spend circuit (~87k vars × ~121k
    // constraints) using the FFT-native RootsOfUnity domain. This is the whole
    // point of the NTT work: setup + prove + verify must all complete at scale
    // (no OOM, no O(n²) blowup). Slow in debug; run in release:
    //   cargo test --release -p gpn-shield -- --ignored note_spend_full_roundtrip
    #[ignore = "full note-spend circuit; run with --release"]
    fn note_spend_full_roundtrip() {
        let note = crate::note_spend::tests::test_note(4);
        let (r1cs, z) = crate::note_spend::note_spend_r1cs(&note);
        assert!(
            crate::gadget::constraints_hold(&r1cs, &z),
            "witness must satisfy"
        );
        let qap = r1cs.qap_sparse();
        let pk = setup(&qap, "testnet-crs-v0");
        let (proof, pub_in) = prove(&pk, &qap, &z);
        assert_eq!(verify(&pk.vk, &pub_in, &proof), Ok(()), "proof must verify");
        // Tampering the public input must fail.
        let mut bad = pub_in.clone();
        bad[0] += Scalar::ONE;
        assert!(
            verify(&pk.vk, &bad, &proof).is_err(),
            "tampered input must be rejected"
        );
    }

    #[test]
    fn key_not_transferable_across_circuits() {
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let pk = setup(&qap, "testnet-crs-v0");
        let z = vec![
            Scalar::ONE,
            Scalar::from(2),  // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8),  // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (proof, _) = prove(&pk, &qap, &z);
        // A different CRS (same circuit shape) must reject the proof.
        let pk2 = setup(&qap, "different-crs");
        assert_eq!(
            verify(&pk2.vk, &[Scalar::from(2)], &proof),
            Err(VerifyError::InvalidProof)
        );
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
            domain: crate::qap::Domain::Consecutive,
            constraints: vec![
                Constraint {
                    a: vec![(2, one)],
                    b: vec![(0, one)],
                    c: vec![(4, one)],
                },
                Constraint {
                    a: vec![(3, one)],
                    b: vec![(0, one)],
                    c: vec![(4, one), (1, -(one))],
                },
                Constraint {
                    a: vec![(2, one)],
                    b: vec![(3, one)],
                    c: vec![(5, one)],
                },
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
        let z = vec![
            Scalar::ONE,
            Scalar::from(2),
            Scalar::from(10),
            Scalar::from(8),
            Scalar::from(10),
            Scalar::from(80),
        ];
        let (proof, pub_in) = prove(&pk_sparse, &dense_qap, &z);
        assert_eq!(verify(&pk_sparse.vk, &pub_in, &proof), Ok(()));
    }
}
