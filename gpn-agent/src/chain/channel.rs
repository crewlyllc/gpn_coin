//! Off-chain escrow channel manager — item #2, `design-offchain-escrow-channels.md`.
//!
//! The chain is the **open/close/settle root of trust**; the per-job money
//! movement happens **off-chain** as mutually-signed balance updates, so an AI
//! job no longer waits a block for escrow/settlement.
//!
//!   open (on-chain, rare)   per-job (OFF-CHAIN, instant)   close (on-chain, rare)
//!   requestor ──deposit──►[channel] ◄──signed updates──►  [channel] ──settle──► both
//!
//! ## What this module owns
//! - The **exact** settle-payload encoding the pallet verifies
//!   ([`channel_settle_payload`]) — byte-for-byte with
//!   `pallet_compute_marketplace::channel_settle_payload`, so a signature made
//!   here passes `ChannelAuth::verify` on-chain.
//! - The **off-chain state machine** ([`EscrowChannel`]): track balances + nonce,
//!   charge a job against the requestor side, and produce the two signatures a
//!   cooperative close needs (see [`EscrowChannel::sign_state`]).
//!
//! It deliberately does **not** talk to the chain — `ChainClient::open_channel` /
//! `close_channel` / `force_close_channel` do that. Keeping the crypto pure makes
//! both sides (requestor and provider) co-sign deterministically.

use crate::chain::signer::Sr25519Signer;

/// Domain tag for the payload a channel party signs to authorize a settlement.
/// **Must** match `pallet_compute_marketplace::CHANNEL_SETTLE_DOMAIN`.
pub const CHANNEL_SETTLE_DOMAIN: &[u8] = b"gpn/channel-settle-v1";

/// SCALE-encode the channel-settlement payload and hash it with blake2-256.
///
/// Mirrors `pallet_compute_marketplace::channel_settle_payload`: the domain tag
/// followed by SCALE-encoded `channel_id: u64`, `balance_requestor: u128`,
/// `balance_provider: u128`, `nonce: u64`. All four are fixed-width integers, so
/// SCALE == little-endian; the encoding is written explicitly (not via a codec)
/// to keep this dependency-light and auditable.
pub fn channel_settle_payload(
    channel_id: u64,
    balance_requestor: u128,
    balance_provider: u128,
    nonce: u64,
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(CHANNEL_SETTLE_DOMAIN.len() + 8 + 16 + 16 + 8);
    buf.extend_from_slice(CHANNEL_SETTLE_DOMAIN);
    buf.extend_from_slice(&channel_id.to_le_bytes());
    buf.extend_from_slice(&balance_requestor.to_le_bytes());
    buf.extend_from_slice(&balance_provider.to_le_bytes());
    buf.extend_from_slice(&nonce.to_le_bytes());
    sp_core::blake2_256(&buf)
}

/// One party's authorization over a channel-settlement state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSignature {
    pub payload: [u8; 32],
    pub signature: [u8; 64],
}

/// An off-chain view of a payment channel between a requestor and a provider.
///
/// `deposit` is the on-chain-locked total; `balance_requestor + balance_provider`
/// must always equal it (the pallet enforces the same invariant on settle). As
/// jobs are served, the requestor's balance is decremented and the provider's
/// incremented, and `nonce` advances — a newer state always supersedes an older
/// one (the pallet rejects `nonce <= channel.nonce`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscrowChannel {
    pub channel_id: u64,
    pub deposit: u128,
    pub balance_requestor: u128,
    pub balance_provider: u128,
    pub nonce: u64,
}

/// Errors from the off-chain channel state machine.
#[derive(Debug, PartialEq, Eq)]
pub enum ChannelError {
    /// A job's price exceeds the requestor's remaining off-chain balance.
    InsufficientChannelBalance,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::InsufficientChannelBalance => {
                write!(f, "job price exceeds remaining requestor channel balance")
            }
        }
    }
}

impl std::error::Error for ChannelError {}

impl EscrowChannel {
    /// A freshly-opened channel: the whole deposit sits on the requestor side.
    pub fn new(channel_id: u64, deposit: u128) -> Self {
        Self {
            channel_id,
            deposit,
            balance_requestor: deposit,
            balance_provider: 0,
            nonce: 0,
        }
    }

    /// The 32-byte payload both parties sign for the *current* state.
    pub fn current_payload(&self) -> [u8; 32] {
        channel_settle_payload(
            self.channel_id,
            self.balance_requestor,
            self.balance_provider,
            self.nonce,
        )
    }

    /// Charge `price` for one served job: move it from the requestor's balance to
    /// the provider's and advance the nonce. Refuses to overdraw the channel.
    pub fn charge(&mut self, price: u128) -> Result<(), ChannelError> {
        if price > self.balance_requestor {
            return Err(ChannelError::InsufficientChannelBalance);
        }
        self.balance_requestor -= price;
        self.balance_provider += price;
        self.nonce += 1;
        debug_assert_eq!(self.balance_requestor + self.balance_provider, self.deposit);
        Ok(())
    }

    /// Sign the current settle state with `signer` (one party's authorization).
    /// Both parties call this to produce the `sig_requestor` / `sig_provider`
    /// pair a cooperative `close_channel` needs.
    pub fn sign_state(&self, signer: &Sr25519Signer) -> ChannelSignature {
        let payload = self.current_payload();
        ChannelSignature {
            payload,
            signature: signer.sign_raw(&payload),
        }
    }

    /// Balance conservation holds (mirrors the pallet's `ChannelBalanceMismatch`).
    pub fn is_conserved(&self) -> bool {
        self.balance_requestor + self.balance_provider == self.deposit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::{crypto::Pair as _, sr25519};

    /// The payload is domain-separated and 32 bytes; its length equals the
    /// domain tag + 8 + 16 + 16 + 8 bytes of SCALE-encoded fields.
    #[test]
    fn payload_is_domain_separated_and_deterministic() {
        let a = channel_settle_payload(7, 100, 0, 1);
        let b = channel_settle_payload(7, 100, 0, 1);
        assert_eq!(a, b, "same inputs must hash identically");
        assert_eq!(a.len(), 32);
        // Changing any field changes the digest.
        assert_ne!(a, channel_settle_payload(8, 100, 0, 1));
        assert_ne!(a, channel_settle_payload(7, 101, 0, 1));
        assert_ne!(a, channel_settle_payload(7, 100, 1, 1));
        assert_ne!(a, channel_settle_payload(7, 100, 0, 2));
    }

    /// The payload matches an independent hand-computation of
    /// `blake2_256(domain || LE(channel_id) || LE(br) || LE(bp) || LE(nonce))`.
    /// This pins the exact byte layout the pallet verifies on-chain, so a
    /// signature produced by the agent is accepted by `ChannelAuth`.
    #[test]
    fn payload_matches_manual_scale_encoding() {
        let (cid, br, bp, nonce) = (42u64, 1_000_000u128, 250u128, 9u64);
        let mut manual = Vec::new();
        manual.extend_from_slice(b"gpn/channel-settle-v1");
        manual.extend_from_slice(&42u64.to_le_bytes());
        manual.extend_from_slice(&1_000_000u128.to_le_bytes());
        manual.extend_from_slice(&250u128.to_le_bytes());
        manual.extend_from_slice(&9u64.to_le_bytes());
        let expected = sp_core::blake2_256(&manual);
        assert_eq!(channel_settle_payload(cid, br, bp, nonce), expected);
    }

    /// Frozen cross-crate interface vector: `blake2_256("gpn/channel-settle-v1"
    /// || u64(42) || u128(1_000_000) || u128(250) || u64(9))`, computed
    /// independently (Python `hashlib.blake2b(digest_size=32)`). The runtime's
    /// `pallet_compute_marketplace::channel_settle_payload` hashes the identical
    /// bytes; if this ever drifts, agent-signed closes would be rejected
    /// on-chain with `InvalidChannelSignature`.
    #[test]
    fn frozen_interface_vector_matches_runtime() {
        let got = hex::encode(channel_settle_payload(42, 1_000_000, 250, 9));
        assert_eq!(
            got,
            "09cf59874e49060554e5d517d514f545b42f529b4da02153501d090cd42e4dc6"
        );
    }

    #[test]
    fn charge_moves_value_and_advances_nonce() {
        let mut ch = EscrowChannel::new(1, 1_000);
        assert_eq!(ch.balance_requestor, 1_000);
        ch.charge(300).unwrap();
        assert_eq!(
            (ch.balance_requestor, ch.balance_provider, ch.nonce),
            (700, 300, 1)
        );
        ch.charge(700).unwrap();
        assert_eq!(
            (ch.balance_requestor, ch.balance_provider, ch.nonce),
            (0, 1_000, 2)
        );
        assert!(ch.is_conserved());
        // Overdraw is rejected, leaving the state unchanged.
        assert_eq!(ch.charge(1), Err(ChannelError::InsufficientChannelBalance));
        assert_eq!(
            (ch.balance_requestor, ch.balance_provider, ch.nonce),
            (0, 1_000, 2)
        );
    }

    /// Both parties sign the SAME payload for a given state, and the signature
    /// verifies with the signing key (the same sr25519 verify the runtime's
    /// `Sr25519ChannelAuth` performs).
    #[test]
    fn both_parties_sign_the_same_state_and_verify() {
        let req = sr25519::Pair::from_string("//Alice", None).unwrap();
        let prov = sr25519::Pair::from_string("//Bob", None).unwrap();
        let mut ch = EscrowChannel::new(5, 500);
        ch.charge(120).unwrap();

        let sig_req = ch
            .sign_state(&Sr25519Signer::from_pair(req.clone()))
            .signature;
        let sig_prov = ch
            .sign_state(&Sr25519Signer::from_pair(prov.clone()))
            .signature;
        let payload = ch.current_payload();

        assert!(sr25519::Pair::verify(
            &sr25519::Signature::from_raw(sig_req),
            &payload,
            &req.public()
        ));
        assert!(sr25519::Pair::verify(
            &sr25519::Signature::from_raw(sig_prov),
            &payload,
            &prov.public()
        ));
    }
}
