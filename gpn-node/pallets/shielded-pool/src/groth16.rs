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

    // Negate α, the accumulated public-input term, and C for the pairing
    // equation. The correct Groth16 verification equation is
    //   e(A,B) · e(-α,β) · e(-acc,γ) · e(-C,δ) = 1_GT
    // (the γabc term is NEGATED). The legacy trivial test key was constructed
    // to absorb this sign; the real value-balance CRS requires it.
    let neg_alpha = -G1Projective::from(vk.alpha_g1);
    let neg_acc = -acc;
    let neg_c = -G1Projective::from(proof.c);

    // e(A, B) · e(-α, β) · e(-acc, γ) · e(-C, δ) == 1_GT
    //
    // bls12_381 models GT as an additive group (group::Group), so the pairing
    // product is a sum of the four pairing results.
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

/// Deserialize a proof from its compressed wire bytes (G1 48 + G2 96 + G1 48).
/// Returns `None` if any point fails to decompress.
pub fn proof_from_bytes(a: &[u8; 48], b: &[u8; 96], c: &[u8; 48]) -> Option<Proof> {
    let a = G1Affine::from_compressed(a).into_option()?;
    let b = G2Affine::from_compressed(b).into_option()?;
    let c = G1Affine::from_compressed(c).into_option()?;
    Some(Proof { a, b, c })
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
    // Constant term C₀ = -[α·β / γ]₁. Under the corrected verification
    // equation e(A,B)·e(-α,β)·e(-acc,γ)·e(-C,δ), with acc=C₀ the honest proof
    // A=α,B=β,C=[αβ/δ] cancels: e(α,β)·e(-α,β)·e(αβ,γ)/... resolves to 1.
    // (The sign must match the verifier's negated γabc term.)
    let const_scalar = -((alpha * beta) * gamma.invert().unwrap_or(Scalar::ONE));
    VerifyingKey {
        alpha_g1: (G1Projective::generator() * alpha).into(),
        beta_g2: (bls12_381::G2Projective::generator() * beta).into(),
        gamma_g2: (bls12_381::G2Projective::generator() * gamma).into(),
        delta_g2: (bls12_381::G2Projective::generator() * delta).into(),
        gamma_abc_g1: vec![(G1Projective::generator() * const_scalar).into()],
    }
}

// ---------------------------------------------------------------------------
// Real value-balance circuit verification key (Step 5, sub-step a)
// ---------------------------------------------------------------------------
//
// This is the **real** CRS for the value-balance circuit defined in
// `gpn-shield/src/qap.rs::value_balance_r1cs` (6 variables, 2 public — constant
// + `fee` —, 3 constraints), deterministically derived from seed
// `"testnet-crs-v0"` by `gpn-shield qap-setup`. It is baked in here so the
// on-chain `verify_proof` binds the *real* public input (`fee`) instead of the
// trivial l=0 test key. Production replaces this with a Powers-of-Tau ceremony.
//
// The `gamma_abc` list has 2 entries: index 0 = constant (a₀ = 1), index 1 =
// the single variable public input `fee`.
pub fn value_balance_key() -> VerifyingKey {
    // Compressed points from `gpn_shield qap-setup testnet-crs-v0`.
    let alpha = G1Affine::from_compressed(&hex48(
        "8b1beb9eaa46f84617b8a487edc7d8430713e5860594857c511c5dd80e4a8c79\
         d89d382955b0ccfd04264efe005f6678",
    )).expect("valid alpha_g1");
    let beta = G2Affine::from_compressed(&hex96(
        "af71521ffa3621e20b79e99a5c1ac5afbe6cf6e4d7008c5c6f77b7065264579a\
         f863ac70998d9cde287bab614a8407e807a89a7638059e365914797401405eb4\
         cec1995e3c84dfa23a115c55b832347f7ef8d2d2336f6301f9c4e3903b65b275",
    )).expect("valid beta_g2");
    let gamma = G2Affine::from_compressed(&hex96(
        "810ae3861a991a8ee80a763542e7b7e27711403f088f501c08ae675644a6764c\
         54f1f1e03c4ae3d79a3ca2305a5937c21092ab265bf216c1d65f90164b44ed60\
         93d4a567af4ff52ce8a489729b576094fd91e73ba492b45c30912c482ab4974a",
    )).expect("valid gamma_g2");
    let delta = G2Affine::from_compressed(&hex96(
        "858fa473094c8957eda90c0f3a7e8d1424849268c463c8d6d8ecd1a70575768d\
         13861ebf010f604fd2f7bc154d4f7fb004cdeec875473c342f9320144e64a1ede\
         91f7bd82f67d92dbbbb860d78dfd94cf4e9ba6c2c69e4b57cd476693dfdf0db",
    )).expect("valid delta_g2");
    let gabc0 = G1Affine::from_compressed(&hex48(
        "af8dd62de36279f1a7d53eb0d0d97dde599e9b142be8ab849cda3f77f2fe2a08\
         c9e7cc795ae9b852eba83c40d176d56b",
    )).expect("valid gamma_abc[0]");
    let gabc1 = G1Affine::from_compressed(&hex48(
        "aeae8329b9b6d1d1231ff3c1aef5e86fc629057a7e62a0a69dee016638d49a27\
         a034209c612feac2de59b700a78e3762",
    )).expect("valid gamma_abc[1]");
    VerifyingKey {
        alpha_g1: alpha,
        beta_g2: beta,
        gamma_g2: gamma,
        delta_g2: delta,
        gamma_abc_g1: vec![gabc0, gabc1],
    }
}

/// Decode a 96-hex-char (48-byte) G1 compressed point string.
fn hex48(s: &str) -> [u8; 48] {
    let mut out = [0u8; 48];
    decode_hex_into(s, &mut out);
    out
}

/// Decode a 192-hex-char (96-byte) G2 compressed point string.
fn hex96(s: &str) -> [u8; 96] {
    let mut out = [0u8; 96];
    decode_hex_into(s, &mut out);
    out
}

fn decode_hex_into(s: &str, out: &mut [u8]) {
    let mut i = 0;
    let mut o = 0;
    let bytes = s.as_bytes();
    while i + 1 < bytes.len() {
        // Skip whitespace (used for source-line wrapping in the literals).
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let hi = hex_val(bytes[i]);
        let lo = hex_val(bytes[i + 1]);
        out[o] = (hi << 4) | lo;
        o += 1;
        i += 2;
    }
    debug_assert_eq!(o, out.len());
}

fn hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
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
