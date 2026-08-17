//! # Groth16 Verifier over BLS12-381 (Step 4 of the shielded-pool roadmap)
//!
//! A self-contained, no_std-clean **Groth16 verification** routine over the
//! BLS12-381 pairing curve (the Sapling curve), implemented directly on the
//! `bls12_381` crate. Proving stays off-chain (the client `gpn_shield` tool,
//! Step 5); the chain only ever *verifies*, so this module is deliberately
//! light and dependency-free beyond `bls12_381`.
//!
//! ## What this implements
//! The standard Groth16 pairing-product verification equation. Given a
//! verification key `vk` and a proof `π`, the verifier accepts iff
//!
//! ```text
//! e(A, B) · e(-α, β) · e(Σᵢ aᵢ·[βuᵢ+αvᵢ+wᵢ]₁/γ, γ) · e(-C, δ) = 1_GT
//! ```
//!
//! where `aᵢ` are the public inputs (with `a₀ = 1` implicitly folded into the
//! constant term of `vk.gamma_abc`).
//!
//! ## Security posture
//! - This is the **verification** half only. No trusted setup, no prover, no
//!   witness — nothing secret ever touches the chain.
//! - The pallet stores the verification key (α, β, γ, δ, and the
//!   `gamma_abc` accumulator for public inputs) as **fixed genesis constants**.
//!   In production these come from a Powers-of-Tau / Sapling-style ceremony;
//!   for the testnet prototype we use a deterministic test key (see
//!   [`test_key`]).
//! - The verifier is side-channel-agnostic: it only reads public data and
//!   returns a boolean. All curve operations are constant-time within the
//!   `bls12_381` crate.

use bls12_381::{pairing, G1Affine, G1Projective, G2Affine, Gt, Scalar};
use ff::Field;
use sp_std::vec;
use sp_std::vec::Vec;

/// A Groth16 verification key. `gamma_abc` holds one G1 point per public input
/// **including** the constant term at index 0 (i.e. `gamma_abc[0]` corresponds
/// to `a₀ = 1`).
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyingKey {
    pub alpha_g1: G1Affine,
    pub beta_g2: G2Affine,
    pub gamma_g2: G2Affine,
    pub delta_g2: G2Affine,
    /// `[βuᵢ(x)+αvᵢ(x)+wᵢ(x)]₁ / γ` for each public input (index 0 = constant).
    pub gamma_abc_g1: Vec<G1Affine>,
}

/// A Groth16 proof.
#[derive(Clone, Debug, PartialEq)]
pub struct Proof {
    pub a: G1Affine,
    pub b: G2Affine,
    pub c: G1Affine,
}

/// Verification failure reasons (kept terse; the caller maps to a pallet error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// The number of public inputs does not match the verification key.
    InputLenMismatch,
    /// The pairing-product equation did not hold.
    InvalidProof,
}

/// Verify a Groth16 proof against a verification key and public inputs.
///
/// `public_inputs` are the *variable* public inputs (a₁..aₗ). The constant term
/// (a₀ = 1) is handled internally via `vk.gamma_abc_g1[0]`.
pub fn verify(vk: &VerifyingKey, public_inputs: &[Scalar], proof: &Proof) -> Result<(), VerifyError> {
    // The vk stores gamma_abc for (l+1) entries: constant + l variable inputs.
    if public_inputs.len() + 1 != vk.gamma_abc_g1.len() {
        return Err(VerifyError::InputLenMismatch);
    }

    // Accumulate the public-input term: acc = gamma_abc[0] + Σᵢ aᵢ·gamma_abc[i+1].
    let mut acc = G1Projective::from(vk.gamma_abc_g1[0]);
    for (a, g) in public_inputs.iter().zip(vk.gamma_abc_g1.iter().skip(1)) {
        // Multiplication by a zero scalar is a no-op addition of the identity,
        // so this is safe even for a = 0.
        acc += *g * a;
    }

    // Negate α and C for the pairing equation.
    let neg_alpha = -G1Projective::from(vk.alpha_g1);
    let neg_c = -G1Projective::from(proof.c);

    // e(A, B) · e(-α, β) · e(acc, γ) · e(-C, δ) == 1_GT
    //
    // bls12_381 models GT as an additive group (group::Group), so the pairing
    // product is a sum of the four pairing results.
    let lhs = pairing(&proof.a, &proof.b)
        + pairing(&G1Affine::from(neg_alpha), &vk.beta_g2)
        + pairing(&G1Affine::from(acc), &vk.gamma_g2)
        + pairing(&G1Affine::from(neg_c), &vk.delta_g2);

    if lhs == Gt::identity() {
        Ok(())
    } else {
        Err(VerifyError::InvalidProof)
    }
}

// ---------------------------------------------------------------------------
// Deterministic test key + witness (prototype only)
// ---------------------------------------------------------------------------
//
// These helpers build a *fixed* (non-random) key so the testnet prototype has
// a working verifier end-to-end without a Powers-of-Tau ceremony. **Not** for
// mainnet: a real deployment needs a proper trusted setup so no single party
// can forge proofs.

/// A deterministic, publicly-known scalar used to derive the test key.
fn test_scalar(seed: u64) -> Scalar {
    // Scalar::from takes u64; fold a larger seed into two limbs for variety.
    let lo = seed & 0xFFFF_FFFF;
    let hi = (seed >> 32) & 0xFFFF_FFFF;
    Scalar::from(lo) + Scalar::from(hi) * Scalar::from(1u64 << 32) // == from(seed) for u64
}

/// Build a deterministic verification key for the prototype.
///
/// We pick arbitrary `α, β, γ, δ` and a single constant term, producing a key
/// that the matching [`test_proof`] satisfies. The exact values don't matter
/// for the prototype — only that verify() accepts the honest proof and rejects
/// tampering.
pub fn test_key() -> VerifyingKey {
    let alpha = test_scalar(0x1111_1111);
    let beta = test_scalar(0x2222_2222);
    let gamma = test_scalar(0x3333_3333);
    let delta = test_scalar(0x4444_4444);
    // Constant term C₀ = [α·β / γ]₁ (so e(C₀,γ)·e(-α,β) cancels for a₀=1).
    let const_scalar = (alpha * beta) * gamma.invert().unwrap_or(Scalar::ONE);
    VerifyingKey {
        alpha_g1: (G1Projective::generator() * alpha).into(),
        beta_g2: (bls12_381::G2Projective::generator() * beta).into(),
        gamma_g2: (bls12_381::G2Projective::generator() * gamma).into(),
        delta_g2: (bls12_381::G2Projective::generator() * delta).into(),
        gamma_abc_g1: vec![(G1Projective::generator() * const_scalar).into()],
    }
}

/// Build a valid proof for [`test_key`] with no variable public inputs.
///
/// For the trivial circuit (l = 0), a satisfying proof is
/// `A = α, B = β, C = [α·β/δ]₁`, which satisfies the pairing equation.
pub fn test_proof() -> Proof {
    let alpha = test_scalar(0x1111_1111);
    let beta = test_scalar(0x2222_2222);
    let delta = test_scalar(0x4444_4444);
    let c_scalar = (alpha * beta) * delta.invert().unwrap_or(Scalar::ONE);
    Proof {
        a: (G1Projective::generator() * alpha).into(),
        b: (bls12_381::G2Projective::generator() * beta).into(),
        c: (G1Projective::generator() * c_scalar).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_honest_proof_verifies() {
        let vk = test_key();
        let proof = test_proof();
        // No variable public inputs for the trivial circuit.
        assert_eq!(verify(&vk, &[], &proof), Ok(()));
    }

    #[test]
    fn test_tampered_proof_rejected() {
        let vk = test_key();
        let mut proof = test_proof();
        proof.a = (G1Projective::generator() * Scalar::from(7u64)).into();
        assert_eq!(verify(&vk, &[], &proof), Err(VerifyError::InvalidProof));
    }

    #[test]
    fn test_wrong_public_input_count() {
        let vk = test_key();
        let proof = test_proof();
        // vk has 1 gamma_abc entry (constant only); any variable input mismatches.
        let one_input = vec![Scalar::ONE];
        assert_eq!(verify(&vk, &one_input, &proof), Err(VerifyError::InputLenMismatch));
    }

    #[test]
    fn test_scalar_folding() {
        // test_scalar(seed) == Scalar::from(seed) for seed < 2^32.
        assert_eq!(test_scalar(42), Scalar::from(42u64));
    }
}
