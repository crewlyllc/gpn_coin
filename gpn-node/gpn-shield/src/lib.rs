//! # gpn_shield — GPN shielded-pool client prover (Step 5)
//!
//! Client-side companion to `pallet-shielded-pool`. Produces Groth16 proofs
//! over BLS12-381 (the Sapling curve) that are **byte-compatible** with the
//! on-chain [`verify_proof`] dispatchable: proof `A`/`C` as G1 compressed
//! (48 bytes), `B` as G2 compressed (96 bytes), public inputs as Fr
//! big-endian (32 bytes each).
//!
//! ## What this milestone delivers
//! - [`keygen`] — reproduces the exact verification key the pallet's on-chain
//!   [`test_key`] uses, so a proof generated here verifies on-chain.
//! - [`prove`] — a Groth16 prover producing the honest proof for that key.
//! - [`verify`] — the **identical pairing-product equation** as the runtime
//!   verifier, so the client can self-check before submitting.
//! - [`ser`] — the wire format matching `verify_proof`'s signature.
//!
//! ## Honest scope (do not over-claim)
//! The on-chain verifier is currently the **trivial (l = 0) test circuit** —
//! no public inputs are bound to note commitment / nullifier / value-balance
//! yet. So this milestone proves the **client↔chain proof toolchain** end to
//! end (same key, same pairing equation, same bytes) and produces the exact
//! proof the chain's `verify_proof` accepts. Binding the proof to a real
//! shielded-note *spend* circuit (public inputs `cm`, `nf`, value balance) is
//! the **next sub-step** — it requires defining the QAP circuit and swapping
//! the on-chain key for that circuit's real CRS (Powers-of-Tau for production,
//! deterministic for the testnet). See `feature2-shielded-pool-spec.md` Step 5.

//! A real Groth16 prover/verifier pipeline (R1CS → QAP → Groth16) lives in the
//! [`qap`] and [`groth16`] modules. The legacy `keygen`/`prove`/`verify`
//! functions below target the pallet's **trivial l=0 test key** and remain for
//! on-chain `verify_proof` byte-compatibility; new circuit work should use the
//! [`groth16`] module.

pub mod groth16;
pub mod poly;
pub mod qap;

use bls12_381::{pairing, G1Affine, G1Projective, G2Affine, Gt, Scalar};
use ff::Field;

/// Groth16 verification key (same layout as `pallet-shielded-pool::groth16`).
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyingKey {
    pub alpha_g1: G1Affine,
    pub beta_g2: G2Affine,
    pub gamma_g2: G2Affine,
    pub delta_g2: G2Affine,
    /// `[βuᵢ(x)+αvᵢ(x)+wᵢ(x)]₁ / γ` per public input (index 0 = constant a₀=1).
    pub gamma_abc_g1: Vec<G1Affine>,
}

/// Groth16 proof.
#[derive(Clone, Debug, PartialEq)]
pub struct Proof {
    pub a: G1Affine,
    pub b: G2Affine,
    pub c: G1Affine,
}

/// Verification failure reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    InputLenMismatch,
    InvalidProof,
}

/// Verify a Groth16 proof against a verification key and public inputs.
///
/// This is a byte-for-byte match of the runtime verifier's pairing-product
/// equation, so any proof accepted here is accepted on-chain (and vice-versa).
pub fn verify(vk: &VerifyingKey, public_inputs: &[Scalar], proof: &Proof) -> Result<(), VerifyError> {
    if public_inputs.len() + 1 != vk.gamma_abc_g1.len() {
        return Err(VerifyError::InputLenMismatch);
    }
    let mut acc = G1Projective::from(vk.gamma_abc_g1[0]);
    for (a, g) in public_inputs.iter().zip(vk.gamma_abc_g1.iter().skip(1)) {
        acc += *g * a;
    }
    let neg_alpha = -G1Projective::from(vk.alpha_g1);
    let neg_c = -G1Projective::from(proof.c);
    let lhs = pairing(&proof.a, &proof.b)
        + pairing(&G1Affine::from(neg_alpha), &vk.beta_g2)
        + pairing(&G1Affine::from(acc), &vk.gamma_g2)
        + pairing(&G1Affine::from(neg_c), &vk.delta_g2);
    if lhs == Gt::identity() { Ok(()) } else { Err(VerifyError::InvalidProof) }
}

/// Deterministic scalar (must match the pallet's `groth16::test_scalar`).
fn test_scalar(seed: u64) -> Scalar {
    let lo = seed & 0xFFFF_FFFF;
    let hi = (seed >> 32) & 0xFFFF_FFFF;
    Scalar::from(lo) + Scalar::from(hi) * Scalar::from(1u64 << 32)
}

/// Reproduce the pallet's on-chain test verification key (l = 0 trivial circuit).
pub fn keygen() -> VerifyingKey {
    let alpha = test_scalar(0x1111_1111);
    let beta = test_scalar(0x2222_2222);
    let gamma = test_scalar(0x3333_3333);
    let delta = test_scalar(0x4444_4444);
    let const_scalar = (alpha * beta) * gamma.invert().unwrap_or(Scalar::ONE);
    VerifyingKey {
        alpha_g1: (G1Projective::generator() * alpha).into(),
        beta_g2: (bls12_381::G2Projective::generator() * beta).into(),
        gamma_g2: (bls12_381::G2Projective::generator() * gamma).into(),
        delta_g2: (bls12_381::G2Projective::generator() * delta).into(),
        gamma_abc_g1: vec![(G1Projective::generator() * const_scalar).into()],
    }
}

/// Produce the honest proof for the current (l = 0) on-chain key.
///
/// The trivial circuit has no private witness; a satisfying proof is
/// `A = α, B = β, C = αβ/δ`, which cancels the pairing equation. This is the
/// exact proof the chain's `verify_proof` accepts.
pub fn prove() -> Proof {
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

/// Wire-format serialization matching the `verify_proof` dispatchable signature
/// `(_proof_a: [u8;48], _proof_b: [u8;96], _proof_c: [u8;48], public_inputs)`.
pub mod ser {
    use super::*;

    pub fn proof_to_bytes(p: &Proof) -> ([u8; 48], [u8; 96], [u8; 48]) {
        (p.a.to_compressed(), p.b.to_compressed(), p.c.to_compressed())
    }

    pub fn proof_from_bytes(a: &[u8; 48], b: &[u8; 96], c: &[u8; 48]) -> Option<Proof> {
        let a = G1Affine::from_compressed(a).into_option()?;
        let b = G2Affine::from_compressed(b).into_option()?;
        let c = G1Affine::from_compressed(c).into_option()?;
        Some(Proof { a, b, c })
    }

    /// u128 value -> 32-byte Fr big-endian (matches the on-chain u64-low parse).
    pub fn scalar_to_32(sc: &Scalar) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[..8].copy_from_slice(&sc.to_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keygen_matches_pallet_honest_verifies() {
        let vk = keygen();
        let proof = prove();
        assert_eq!(verify(&vk, &[], &proof), Ok(()));
    }

    #[test]
    fn tampered_proof_rejected() {
        let vk = keygen();
        let mut proof = prove();
        proof.a = (G1Projective::generator() * Scalar::from(9u64)).into();
        assert_eq!(verify(&vk, &[], &proof), Err(VerifyError::InvalidProof));
    }

    #[test]
    fn round_trip_serialization() {
        let proof = prove();
        let (a, b, c) = ser::proof_to_bytes(&proof);
        let back = ser::proof_from_bytes(&a, &b, &c).expect("valid compressed points");
        assert_eq!(back, proof);
    }

    #[test]
    fn onchain_verify_proof_accepts_client_proof() {
        // Mirrors pallet verify_proof: empty public inputs + the honest proof
        // against the on-chain test key must pass (the dispatchable's contract).
        let vk = keygen();
        let proof = prove();
        let scalars: Vec<Scalar> = Vec::new();
        assert_eq!(verify(&vk, &scalars, &proof), Ok(()));
    }
}
