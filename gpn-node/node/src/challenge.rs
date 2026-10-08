//! Node-native GPU capacity challenge verifier.
//!
//! Part 2 of `design-verifiable-gpu-proof-2026-10-06.md`. The runtime cannot
//! run Ethash (it needs the ~16 MB light cache, and a no_std wasm runtime has
//! neither the cache nor the `ethash` dependency), so the marketplace-capacity
//! challenge is verified **natively by the node** — exactly like a block seal —
//! and the *result* is what gets committed on-chain.
//!
//! Flow (see the design doc):
//!   1. The chain publishes an unpredictable `AttestationChallenge` (derived
//!      from a recent block; `pallet-gpu-attestation::on_initialize`).
//!   2. A provider mines an **Ethash solution over that 32-byte challenge**
//!      (`EthashGpuMiner::mine` — same kernel it uses for block seals) at a
//!      target set by the runtime for the claimed throughput tier.
//!   3. The **node** verifies the solution here (it has the epoch cache) and a
//!      trusted verifier commits the boolean result on-chain; `submit_attestation`
//!      then treats the attestation as verified.
//!
//! The seal layout matches the block-seal convention used throughout the node:
//! `nonce(8 bytes, little-endian) ++ mix_hash(32 bytes)`.
//!
//! NOTE: this primitive is landed ahead of its wiring. The next step adds the
//! verifier-only on-chain `confirm_challenge` extrinsic + a node service task
//! that feeds this verifier; the `allow(dead_code)` is removed at that point.
#![allow(dead_code)]

use sp_core::U256;

use crate::service::{ethash_cache, ethash_full_size};

/// A capacity-challenge seal: `nonce(8 LE) ++ mix_hash(32)`.
pub const CHALLENGE_SEAL_LEN: usize = 8 + 32;
const NONCE_LEN: usize = 8;
const MIX_LEN: usize = 32;

/// Verify a provider's Ethash solution to a 32-byte capacity `challenge`.
///
/// Returns `true` iff `seal` is well-formed **and** recomputing Ethash over the
/// challenge reproduces the claimed `mix_hash` **and** the final hash is below
/// `target` (the same "higher target = easier" convention as block seals, where
/// `hash < target` is valid).
///
/// This is the node-side half of the challenge; the runtime only ever sees the
/// boolean result (committed by a trusted verifier), so the expensive/cacheful
/// work stays out of consensus.
pub fn verify_challenge_solution(challenge: &[u8; 32], seal: &[u8], target: U256) -> bool {
    if seal.len() != CHALLENGE_SEAL_LEN {
        return false;
    }
    let nonce = u64::from_le_bytes(seal[..NONCE_LEN].try_into().unwrap());
    let mut mix = [0u8; MIX_LEN];
    mix.copy_from_slice(&seal[NONCE_LEN..CHALLENGE_SEAL_LEN]);

    let header_hash = ethereum_types::H256::from_slice(challenge);
    let (computed_mix, result) = ethash::hashimoto_light(
        header_hash,
        ethereum_types::H64::from_low_u64_be(nonce),
        ethash_full_size(),
        ethash_cache(),
    );

    // The mix must match exactly (proves the work was done over THIS challenge),
    // and the final hash must meet the difficulty target.
    if computed_mix.as_bytes() != mix.as_slice() {
        return false;
    }
    U256::from_little_endian(result.as_bytes()) < target
}

/// Parse a hex-encoded seal (`nonce ++ mix`, 40 bytes) into raw bytes.
/// Accepts an optional `0x` prefix. Returns `None` on bad hex / wrong length.
pub fn parse_seal_hex(hex_str: &str) -> Option<Vec<u8>> {
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if s.len() != CHALLENGE_SEAL_LEN * 2 {
        return None;
    }
    let mut out = vec![0u8; CHALLENGE_SEAL_LEN];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{ethash_cache, ethash_full_size};
    use sp_core::U256;

    /// Mine a real (light-cache) Ethash solution over `challenge` at `target`.
    fn mine(challenge: &[u8; 32], target: U256) -> Vec<u8> {
        let header = ethereum_types::H256::from_slice(challenge);
        let mut nonce: u64 = 0;
        loop {
            let (mix, result) = ethash::hashimoto_light(
                header,
                ethereum_types::H64::from_low_u64_be(nonce),
                ethash_full_size(),
                ethash_cache(),
            );
            if U256::from_little_endian(result.as_bytes()) < target {
                let mut seal = Vec::with_capacity(CHALLENGE_SEAL_LEN);
                seal.extend_from_slice(&nonce.to_le_bytes());
                seal.extend_from_slice(mix.as_bytes());
                return seal;
            }
            nonce += 1;
        }
    }

    fn easy_target() -> U256 {
        // Very easy target so the test mines in a handful of nonces.
        let mut buf = [0u8; 32];
        buf[0] = 0xff;
        buf[1] = 0x00;
        U256::from_big_endian(&buf)
    }

    #[test]
    fn accepts_valid_solution() {
        let challenge = [0x42u8; 32];
        let target = easy_target();
        let seal = mine(&challenge, target);
        assert!(verify_challenge_solution(&challenge, &seal, target));
    }

    #[test]
    fn rejects_wrong_challenge() {
        let target = easy_target();
        let seal = mine(&[0x42u8; 32], target);
        // Same seal, different challenge -> mix won't reproduce.
        assert!(!verify_challenge_solution(&[0x43u8; 32], &seal, target));
    }

    #[test]
    fn rejects_above_target() {
        let challenge = [0x42u8; 32];
        // Mine at an easy target, then demand an impossibly hard one.
        let seal = mine(&challenge, easy_target());
        let hard = U256::from(1u64); // result < 1 ~ impossible
        assert!(!verify_challenge_solution(&challenge, &seal, hard));
    }

    #[test]
    fn rejects_bad_length() {
        let target = easy_target();
        assert!(!verify_challenge_solution(&[0u8; 32], &[0u8; 39], target));
        assert!(!verify_challenge_solution(&[0u8; 32], &[0u8; 41], target));
    }

    #[test]
    fn parse_seal_hex_roundtrip() {
        let raw = vec![0xabu8; CHALLENGE_SEAL_LEN];
        let hex_s: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_seal_hex(&hex_s).unwrap(), raw);
        assert_eq!(parse_seal_hex(&format!("0x{hex_s}")).unwrap(), raw);
        assert!(parse_seal_hex("deadbeef").is_none());
        assert!(parse_seal_hex(&format!("zz{hex_s}")).is_none());
    }
}
