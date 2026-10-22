//! AP-hub off-chain escrow protocol — v2 of
//! `docs/design/design-offchain-escrow-channels.md`.
//!
//! The on-chain primitives (`open_channel` / `close_channel` /
//! `force_close_channel`, pallet `compute-marketplace`) let two parties lock a
//! deposit once and then move value with **two signatures**. This module is the
//! missing half: the exact **message schemas, signing payloads, and the two
//! state machines** (access-point hub side, provider side) that turn "an AI job"
//! into an off-chain signed exchange with **zero per-job on-chain txs**.
//!
//! ```text
//!  AP (requestor)                                   provider
//!  ─────────────                                    ────────
//!  Assignment{sig_requestor, sig_settle_after}  ──►  accept():
//!      channel_id, job_id, model, prompt,               verifies both signatures,
//!      price, state_before, state_after,                checks nonce monotonicity,
//!      deadline_ms, request_nonce                       commits state_after
//!                                                   run inference (off-chain)
//!  JobResultMsg{output, result_hash, sig_provider,
//!               sig_settle}                     ◄──  sign_result()
//!  complete_job(): verify, advance state,
//!                  keep provider's settle sig
//! ```
//!
//! ## Trust properties
//! * **The AP cannot steal.** The requestor (AP) pre-signs the *post-job* channel
//!   state ([`Assignment::sig_settle`] over the pallet's
//!   `channel_settle_payload`). Once the provider has done the work it holds a
//!   signature that pays it even if the AP goes dark — it can `force_close_channel`
//!   after the channel deadline. The AP can never produce a state with a lower
//!   `balance_provider` and have it accepted ([`HubChannel::apply`] rejects a
//!   provider-balance decrease).
//! * **The provider cannot overcharge or forge.** The assignment pins the price
//!   and `state_after == state_before + price`, and the AP verifies the
//!   provider's signature over the result (which binds `job_id`, `result_hash`,
//!   token counts and `state_after`).
//! * **Replay is impossible.** Every assignment carries a strictly increasing
//!   per-channel `request_nonce` **and** a unique `job_id`; every state update
//!   advances the channel `nonce` (the pallet rejects `nonce <= on-chain nonce`).
//!
//! Everything here is pure (no chain, no HTTP) so both sides and the tests use
//! exactly one implementation of the wire format.

use serde::{Deserialize, Serialize};
use sp_core::sr25519::{Pair, Public, Signature};
use sp_core::Pair as _;

use crate::chain::channel::channel_settle_payload;
use crate::chain::signer::Sr25519Signer;

/// Domain tag for the signed job assignment (AP → provider).
pub const HUB_ASSIGN_DOMAIN: &[u8] = b"gpn/hub-assign-v1";
/// Domain tag for the signed job result (provider → AP).
pub const HUB_RESULT_DOMAIN: &[u8] = b"gpn/hub-result-v1";

/// A 32-byte public key, serialized as a `0x`-prefixed hex string.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct PubKey(pub [u8; 32]);

/// A 64-byte sr25519 signature, serialized as a `0x`-prefixed hex string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signature64(pub [u8; 64]);

impl PubKey {
    pub fn to_hex(self) -> String {
        format!("0x{}", hex::encode(self.0))
    }
    pub fn from_slice(b: &[u8]) -> Option<Self> {
        if b.len() != 32 {
            return None;
        }
        let mut o = [0u8; 32];
        o.copy_from_slice(b);
        Some(Self(o))
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        let b = hex::decode(s.trim().trim_start_matches("0x")).ok()?;
        Self::from_slice(&b)
    }
}

impl PubKey {
    /// Accept either a `0x`-hex 32-byte public key or an ss58 address (the
    /// scheduler's provider registry keys on ss58).
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(p) = Self::from_hex(s) {
            return Some(p);
        }
        use sp_core::crypto::Ss58Codec;
        Public::from_ss58check(s.trim()).ok().map(|p| Self(p.0))
    }
}

impl Signature64 {
    pub fn to_hex(self) -> String {
        format!("0x{}", hex::encode(self.0))
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        let b = hex::decode(s.trim().trim_start_matches("0x")).ok()?;
        if b.len() != 64 {
            return None;
        }
        let mut o = [0u8; 64];
        o.copy_from_slice(&b);
        Some(Self(o))
    }
}

impl Serialize for PubKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl<'de> Deserialize<'de> for PubKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_hex(&s).ok_or_else(|| serde::de::Error::custom("invalid 32-byte public key"))
    }
}
impl Serialize for Signature64 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl<'de> Deserialize<'de> for Signature64 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_hex(&s).ok_or_else(|| serde::de::Error::custom("invalid 64-byte signature"))
    }
}

/// A `0x`-prefixed 32-byte hash (result commitment), serialized as hex.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hash32(pub [u8; 32]);

impl Serialize for Hash32 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{}", hex::encode(self.0)))
    }
}
impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let b = hex::decode(s.trim().trim_start_matches("0x"))
            .map_err(|_| serde::de::Error::custom("invalid 32-byte hash hex"))?;
        if b.len() != 32 {
            return Err(serde::de::Error::custom("invalid 32-byte hash length"));
        }
        let mut o = [0u8; 32];
        o.copy_from_slice(&b);
        Ok(Self(o))
    }
}

impl Hash32 {
    pub fn of(bytes: &[u8]) -> Self {
        Self(sp_core::blake2_256(bytes))
    }
    pub fn to_hex(self) -> String {
        format!("0x{}", hex::encode(self.0))
    }
}

/// The channel balance vector — the thing both parties sign.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ChannelState {
    pub balance_requestor: u128,
    pub balance_provider: u128,
    pub nonce: u64,
}

impl ChannelState {
    pub fn new(deposit: u128) -> Self {
        Self {
            balance_requestor: deposit,
            balance_provider: 0,
            nonce: 0,
        }
    }
    /// The exact 32-byte payload the pallet's `ChannelAuth` verifies.
    pub fn settle_payload(&self, channel_id: u64) -> [u8; 32] {
        channel_settle_payload(
            channel_id,
            self.balance_requestor,
            self.balance_provider,
            self.nonce,
        )
    }
    pub fn is_conserved(&self, deposit: u128) -> bool {
        self.balance_requestor.saturating_add(self.balance_provider) == deposit
    }
}

// ── canonical signing payloads ──────────────────────────────────────────────
// Explicit little-endian / length-prefixed encoding (no JSON in the hashed
// bytes) so a signature can never depend on map ordering or float formatting.

fn push_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn push_state(buf: &mut Vec<u8>, s: &ChannelState) {
    buf.extend_from_slice(&s.balance_requestor.to_le_bytes());
    buf.extend_from_slice(&s.balance_provider.to_le_bytes());
    buf.extend_from_slice(&s.nonce.to_le_bytes());
}

/// Canonical hash the AP signs to authorise one job.
#[allow(clippy::too_many_arguments)]
pub fn assignment_payload(a: &Assignment) -> [u8; 32] {
    let mut b = Vec::with_capacity(256);
    b.extend_from_slice(HUB_ASSIGN_DOMAIN);
    b.extend_from_slice(&a.channel_id.to_le_bytes());
    b.extend_from_slice(&a.job_id.to_le_bytes());
    b.extend_from_slice(&a.requestor.0);
    b.extend_from_slice(&a.provider.0);
    b.extend_from_slice(&a.deposit.to_le_bytes());
    push_state(&mut b, &a.state_before);
    push_state(&mut b, &a.state_after);
    push_str(&mut b, &a.model);
    push_str(&mut b, &a.prompt);
    b.extend_from_slice(&a.max_tokens.to_le_bytes());
    b.extend_from_slice(&a.temperature_milli.to_le_bytes());
    b.extend_from_slice(&a.price.to_le_bytes());
    b.extend_from_slice(&a.deadline_ms.to_le_bytes());
    b.extend_from_slice(&a.request_nonce.to_le_bytes());
    sp_core::blake2_256(&b)
}

/// Canonical hash the provider signs to deliver a result (or a refund).
pub fn result_payload(r: &JobResultMsg) -> [u8; 32] {
    let mut b = Vec::with_capacity(256);
    b.extend_from_slice(HUB_RESULT_DOMAIN);
    b.extend_from_slice(&r.channel_id.to_le_bytes());
    b.extend_from_slice(&r.job_id.to_le_bytes());
    b.extend_from_slice(&r.provider.0);
    b.extend_from_slice(&r.result_hash.0);
    push_str(&mut b, &r.output);
    b.extend_from_slice(&r.prompt_tokens.to_le_bytes());
    b.extend_from_slice(&r.completion_tokens.to_le_bytes());
    b.push(if r.ok { 1 } else { 0 });
    push_state(&mut b, &r.state_after);
    sp_core::blake2_256(&b)
}

pub fn verify_sig(pubkey: &PubKey, payload: &[u8; 32], sig: &Signature64) -> bool {
    Pair::verify(
        &Signature::from_raw(sig.0),
        payload,
        &Public::from_raw(pubkey.0),
    )
}

// ── wire messages ───────────────────────────────────────────────────────────

/// AP → provider: one job, fully pre-authorised.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assignment {
    pub channel_id: u64,
    /// AP-assigned off-chain job id (unique per channel; never reused).
    pub job_id: u64,
    pub requestor: PubKey,
    pub provider: PubKey,
    /// Total locked in the channel (must equal br + bp).
    pub deposit: u128,
    pub state_before: ChannelState,
    /// The state the AP pre-authorises as payment: before + price, nonce + 1.
    pub state_after: ChannelState,
    pub model: String,
    pub prompt: String,
    pub max_tokens: u32,
    /// Temperature × 1000 (integer, so signing is deterministic).
    pub temperature_milli: u32,
    pub price: u128,
    /// Wall-clock deadline (unix ms) after which the provider must not start.
    pub deadline_ms: u64,
    /// Strictly increasing per channel: replay protection for assignments.
    pub request_nonce: u64,
    /// AP's sr25519 signature over [`assignment_payload`].
    pub sig_requestor: Signature64,
    /// AP's half of the 2-of-2 close for `state_after` (pallet settle payload).
    /// This is what stops the AP from walking away unpaid: the provider holds it
    /// and can `force_close_channel` after the channel deadline.
    pub sig_settle: Signature64,
}

/// Provider → AP: the work (or a refund), and the provider's half of the settle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobResultMsg {
    pub channel_id: u64,
    pub job_id: u64,
    pub provider: PubKey,
    /// `false` = the job failed and this is a full refund to the requestor.
    pub ok: bool,
    pub output: String,
    pub result_hash: Hash32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// The state this result settles to (equals the assignment's `state_after`
    /// on success; equals a reverted state with the next nonce on failure).
    pub state_after: ChannelState,
    /// Provider's sr25519 signature over [`result_payload`].
    pub sig_provider: Signature64,
    /// Provider's half of the 2-of-2 close for `state_after`.
    pub sig_settle: Signature64,
}

/// Everything a cooperative `close_channel` needs.
#[derive(Clone, Debug)]
pub struct CloseMaterial {
    pub channel_id: u64,
    pub state: ChannelState,
    pub sig_requestor: Signature64,
    pub sig_provider: Signature64,
}

// ── errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub enum HubError {
    InsufficientChannelBalance,
    InvalidRequestorSignature,
    InvalidProviderSignature,
    ChannelMismatch,
    DepositMismatch,
    StateNotConserved,
    /// The assignment's `state_after` is not `state_before + price`.
    PaymentNotExact,
    /// `state_before` is not the channel's current state.
    StaleStateBefore,
    /// A state update that would lower the provider's balance (AP clawback).
    ProviderBalanceDecreased,
    NonceNotAdvancing,
    ReplayedRequestNonce,
    ReplayedJobId,
    UnknownJob,
    JobAlreadySettled,
    Expired,
    ResultHashMismatch,
    UnexpectedRefund,
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            HubError::InsufficientChannelBalance => "job price exceeds the channel balance",
            HubError::InvalidRequestorSignature => "invalid requestor signature",
            HubError::InvalidProviderSignature => "invalid provider signature",
            HubError::ChannelMismatch => "channel id / party mismatch",
            HubError::DepositMismatch => "channel deposit mismatch",
            HubError::StateNotConserved => "state does not conserve the deposit",
            HubError::PaymentNotExact => "state_after is not state_before + price",
            HubError::StaleStateBefore => "state_before is not the current channel state",
            HubError::ProviderBalanceDecreased => "state would decrease the provider balance",
            HubError::NonceNotAdvancing => "channel nonce did not advance",
            HubError::ReplayedRequestNonce => "replayed request nonce",
            HubError::ReplayedJobId => "replayed job id",
            HubError::UnknownJob => "unknown job id",
            HubError::JobAlreadySettled => "job already settled",
            HubError::Expired => "deadline passed",
            HubError::ResultHashMismatch => "result hash does not match the output",
            HubError::UnexpectedRefund => "refund for a job that did not fail",
        };
        write!(f, "{s}")
    }
}
impl std::error::Error for HubError {}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── AP (hub) side ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    price: u128,
    deadline_ms: u64,
    before: ChannelState,
    after: ChannelState,
}

/// The AP's view of one hub channel (AP = requestor).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HubChannel {
    pub channel_id: u64,
    pub deposit: u128,
    pub state: ChannelState,
    pub request_nonce: u64,
    /// Latest provider-signed state (the cheapest cooperative close).
    pub latest_provider_sig: Option<Signature64>,
    pending: std::collections::BTreeMap<u64, Pending>,
    settled_jobs: std::collections::BTreeSet<u64>,
}

impl HubChannel {
    pub fn new(channel_id: u64, deposit: u128) -> Self {
        Self {
            channel_id,
            deposit,
            state: ChannelState::new(deposit),
            request_nonce: 0,
            latest_provider_sig: None,
            pending: Default::default(),
            settled_jobs: Default::default(),
        }
    }

    /// Resume from an on-chain/resumed state (`state.nonce` is the last settled
    /// nonce; off-chain state may be ahead).
    pub fn with_state(channel_id: u64, deposit: u128, state: ChannelState) -> Self {
        Self {
            channel_id,
            deposit,
            state,
            ..Self::new(channel_id, deposit)
        }
    }

    /// Price the next job and produce the AP-signed assignment.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_job(
        &mut self,
        signer: &Sr25519Signer,
        job_id: u64,
        model: &str,
        prompt: &str,
        max_tokens: u32,
        temperature_milli: u32,
        price: u128,
        deadline_ms: u64,
    ) -> Result<Assignment, HubError> {
        if price == 0 || price > self.state.balance_requestor {
            return Err(HubError::InsufficientChannelBalance);
        }
        if self.settled_jobs.contains(&job_id) || self.pending.contains_key(&job_id) {
            return Err(HubError::ReplayedJobId);
        }
        let deadline_ms = if deadline_ms == 0 {
            now_ms() + 60_000
        } else {
            deadline_ms
        };
        if deadline_ms <= now_ms() {
            return Err(HubError::Expired);
        }

        let before = self.state;
        let after = ChannelState {
            balance_requestor: before.balance_requestor - price,
            balance_provider: before.balance_provider + price,
            nonce: before.nonce + 1,
        };
        let request_nonce = self.request_nonce + 1;

        let mut a = Assignment {
            channel_id: self.channel_id,
            job_id,
            requestor: PubKey(signer.public_key()),
            provider: PubKey([0u8; 32]),
            deposit: self.deposit,
            state_before: before,
            state_after: after,
            model: model.to_string(),
            prompt: prompt.to_string(),
            max_tokens,
            temperature_milli,
            price,
            deadline_ms,
            request_nonce,
            sig_requestor: Signature64([0u8; 64]),
            sig_settle: Signature64([0u8; 64]),
        };
        // The provider field is only known by the caller (registry key); it is
        // filled in by `begin_job_for` / `sign_assignment`.
        a.sig_requestor = Signature64(signer.sign_raw(&assignment_payload(&a)));
        a.sig_settle = Signature64(signer.sign_raw(&after.settle_payload(self.channel_id)));
        self.request_nonce = request_nonce;
        self.pending.insert(
            job_id,
            Pending {
                price,
                deadline_ms,
                before,
                after,
            },
        );
        Ok(a)
    }

    /// [`Self::begin_job`] with the provider public key bound into the signature.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_job_for(
        &mut self,
        signer: &Sr25519Signer,
        provider: PubKey,
        job_id: u64,
        model: &str,
        prompt: &str,
        max_tokens: u32,
        temperature_milli: u32,
        price: u128,
        deadline_ms: u64,
    ) -> Result<Assignment, HubError> {
        let mut a = self.begin_job(
            signer,
            job_id,
            model,
            prompt,
            max_tokens,
            temperature_milli,
            price,
            deadline_ms,
        )?;
        // Re-sign with the provider field populated (it is inside the payload).
        a.provider = provider;
        a.sig_requestor = Signature64(signer.sign_raw(&assignment_payload(&a)));
        Ok(a)
    }

    /// Apply a completed result. Verifies the provider's signature, the result
    /// hash, and the exact expected post-state; advances the channel.
    pub fn complete_job(&mut self, r: &JobResultMsg) -> Result<(), HubError> {
        if r.channel_id != self.channel_id {
            return Err(HubError::ChannelMismatch);
        }
        let p = self
            .pending
            .get(&r.job_id)
            .ok_or(HubError::UnknownJob)?
            .clone();
        // Authenticate before interpreting the state: a forged result must never
        // be classified by its (attacker-chosen) balances.
        if !verify_sig(&r.provider, &result_payload(r), &r.sig_provider) {
            return Err(HubError::InvalidProviderSignature);
        }
        if !verify_sig(
            &r.provider,
            &r.state_after.settle_payload(self.channel_id),
            &r.sig_settle,
        ) {
            return Err(HubError::InvalidProviderSignature);
        }
        if r.state_after == p.after {
            if !r.ok {
                return Err(HubError::UnexpectedRefund);
            }
        } else if !r.ok {
            // A refund must revert to the pre-job balances with a fresh nonce.
            let expected = ChannelState {
                balance_requestor: p.before.balance_requestor,
                balance_provider: p.before.balance_provider,
                nonce: p.after.nonce + 1,
            };
            if r.state_after != expected {
                return Err(HubError::PaymentNotExact);
            }
        } else {
            return Err(HubError::PaymentNotExact);
        }
        if !r.state_after.is_conserved(self.deposit) {
            return Err(HubError::StateNotConserved);
        }
        if r.state_after.nonce <= self.state.nonce {
            return Err(HubError::NonceNotAdvancing);
        }
        if r.state_after.balance_provider < self.state.balance_provider {
            return Err(HubError::ProviderBalanceDecreased);
        }
        if r.ok && Hash32::of(r.output.as_bytes()) != r.result_hash {
            return Err(HubError::ResultHashMismatch);
        }

        self.state = r.state_after;
        self.latest_provider_sig = Some(r.sig_settle);
        self.pending.remove(&r.job_id);
        self.settled_jobs.insert(r.job_id);
        Ok(())
    }

    /// Drop a pending job whose deadline passed with no result. Returns true if
    /// it was pending. The channel state is untouched (no value moved).
    pub fn expire_pending(&mut self, job_id: u64) -> bool {
        match self.pending.get(&job_id) {
            Some(p) if now_ms() > p.deadline_ms => {
                self.pending.remove(&job_id);
                true
            }
            _ => false,
        }
    }

    /// Drop a pending job that **provably never ran**: the assignment was never
    /// delivered, or the provider rejected it at accept-time. Returns true if it
    /// was pending.
    ///
    /// This is deliberately *not* [`Self::expire_pending`] (which waits for the
    /// deadline): a caller that is about to retry on-chain needs the slot gone
    /// immediately, or the next sweep would still see a phantom in-flight job.
    /// Only call it when no value moved — an assignment that may have been
    /// accepted must keep its pending entry, because the provider holds the
    /// AP's pre-signed `sig_settle` and could force-close with it.
    pub fn abort_pending(&mut self, job_id: u64) -> bool {
        self.pending.remove(&job_id).is_some()
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Should the AP settle on-chain? (provider balance or nonce threshold)
    pub fn should_close(&self, max_unsettled: u128, max_nonce_gap: u64) -> bool {
        (max_unsettled > 0 && self.state.balance_provider >= max_unsettled)
            || (max_nonce_gap > 0 && self.state.nonce >= max_nonce_gap)
    }

    /// Build the cooperative-close material for the *latest signed* state.
    pub fn close_material(&self, signer: &Sr25519Signer) -> Result<CloseMaterial, HubError> {
        let sig_provider = self.latest_provider_sig.ok_or(HubError::UnknownJob)?;
        Ok(CloseMaterial {
            channel_id: self.channel_id,
            state: self.state,
            sig_requestor: Signature64(
                signer.sign_raw(&self.state.settle_payload(self.channel_id)),
            ),
            sig_provider,
        })
    }
}

// ── provider side ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedJob {
    pub before: ChannelState,
    pub after: ChannelState,
    pub price: u128,
    pub deadline_ms: u64,
}

/// The provider's view of one hub channel (provider = provider).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderChannel {
    pub channel_id: u64,
    pub deposit: u128,
    pub requestor: PubKey,
    pub state: ChannelState,
    pub last_request_nonce: u64,
    accepted: std::collections::BTreeMap<u64, AcceptedJob>,
}

impl ProviderChannel {
    pub fn new(channel_id: u64, deposit: u128, requestor: PubKey) -> Self {
        Self {
            channel_id,
            deposit,
            requestor,
            state: ChannelState::new(deposit),
            last_request_nonce: 0,
            accepted: Default::default(),
        }
    }

    /// Verify an assignment and commit its pre-authorised payment.
    pub fn accept(&mut self, a: &Assignment) -> Result<AcceptedJob, HubError> {
        if a.channel_id != self.channel_id || a.requestor != self.requestor {
            return Err(HubError::ChannelMismatch);
        }
        if a.deposit != self.deposit {
            return Err(HubError::DepositMismatch);
        }
        if a.request_nonce <= self.last_request_nonce {
            return Err(HubError::ReplayedRequestNonce);
        }
        if self.accepted.contains_key(&a.job_id) {
            return Err(HubError::ReplayedJobId);
        }
        if a.state_before != self.state {
            return Err(HubError::StaleStateBefore);
        }
        if a.deadline_ms <= now_ms() {
            return Err(HubError::Expired);
        }
        if a.price == 0
            || a.state_after.balance_requestor
                != a.state_before.balance_requestor.saturating_sub(a.price)
            || a.state_after.balance_provider
                != a.state_before.balance_provider.saturating_add(a.price)
            || a.state_after.nonce != a.state_before.nonce + 1
        {
            return Err(HubError::PaymentNotExact);
        }
        if !a.state_after.is_conserved(self.deposit) {
            return Err(HubError::StateNotConserved);
        }
        if !verify_sig(&a.requestor, &assignment_payload(a), &a.sig_requestor) {
            return Err(HubError::InvalidRequestorSignature);
        }
        // The AP's half of the close over the *post* state — our liveness proof.
        if !verify_sig(
            &a.requestor,
            &a.state_after.settle_payload(self.channel_id),
            &a.sig_settle,
        ) {
            return Err(HubError::InvalidRequestorSignature);
        }

        let job = AcceptedJob {
            before: a.state_before,
            after: a.state_after,
            price: a.price,
            deadline_ms: a.deadline_ms,
        };
        self.state = a.state_after;
        self.last_request_nonce = a.request_nonce;
        self.accepted.insert(a.job_id, job.clone());
        Ok(job)
    }

    fn settle_sig(&self, signer: &Sr25519Signer, state: &ChannelState) -> Signature64 {
        Signature64(signer.sign_raw(&state.settle_payload(self.channel_id)))
    }

    /// Sign the delivered result. Charges the pre-authorised price.
    pub fn sign_result(
        &mut self,
        signer: &Sr25519Signer,
        job_id: u64,
        output: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) -> Result<JobResultMsg, HubError> {
        let job = self
            .accepted
            .get(&job_id)
            .ok_or(HubError::UnknownJob)?
            .clone();
        let mut r = JobResultMsg {
            channel_id: self.channel_id,
            job_id,
            provider: PubKey(signer.public_key()),
            ok: true,
            output: output.to_string(),
            result_hash: Hash32::of(output.as_bytes()),
            prompt_tokens,
            completion_tokens,
            state_after: job.after,
            sig_provider: Signature64([0u8; 64]),
            sig_settle: self.settle_sig(signer, &job.after),
        };
        r.sig_provider = Signature64(signer.sign_raw(&result_payload(&r)));
        self.accepted.remove(&job_id);
        Ok(r)
    }

    /// Sign a refund (the work failed): revert to the pre-job balances with a
    /// fresh nonce, so the requestor's channel is made whole.
    pub fn sign_refund(
        &mut self,
        signer: &Sr25519Signer,
        job_id: u64,
        error: &str,
    ) -> Result<JobResultMsg, HubError> {
        let job = self
            .accepted
            .get(&job_id)
            .ok_or(HubError::UnknownJob)?
            .clone();
        let after = ChannelState {
            balance_requestor: job.before.balance_requestor,
            balance_provider: job.before.balance_provider,
            nonce: job.after.nonce + 1,
        };
        let mut r = JobResultMsg {
            channel_id: self.channel_id,
            job_id,
            provider: PubKey(signer.public_key()),
            ok: false,
            output: error.to_string(),
            result_hash: Hash32::of(error.as_bytes()),
            prompt_tokens: 0,
            completion_tokens: 0,
            state_after: after,
            sig_provider: Signature64([0u8; 64]),
            sig_settle: self.settle_sig(signer, &after),
        };
        r.sig_provider = Signature64(signer.sign_raw(&result_payload(&r)));
        self.state = after;
        self.accepted.remove(&job_id);
        Ok(r)
    }

    pub fn pending_len(&self) -> usize {
        self.accepted.len()
    }
}

/// Persisted AP-side registry (provider hex → channel). Survives restarts so a
/// hub channel is opened **once**, not per process start.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HubRegistry {
    pub version: u32,
    /// provider account hex (`0x…`) → channel
    pub channels: std::collections::BTreeMap<String, HubChannel>,
}

impl HubRegistry {
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &std::path::Path) -> bool {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        let Ok(s) = serde_json::to_string_pretty(self) else {
            return false;
        };
        if std::fs::write(&tmp, s).is_err() {
            return false;
        }
        std::fs::rename(&tmp, path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::crypto::Pair as _;

    fn signer(uri: &str) -> Sr25519Signer {
        Sr25519Signer::from_uri(uri).unwrap()
    }

    fn setup(deposit: u128) -> (Sr25519Signer, Sr25519Signer, HubChannel, ProviderChannel) {
        let ap = signer("//Alice");
        let prov = signer("//Bob");
        let ch = HubChannel::new(7, deposit);
        let pch = ProviderChannel::new(7, deposit, PubKey(ap.public_key()));
        (ap, prov, ch, pch)
    }

    #[test]
    fn happy_path_moves_exact_value_and_advances_nonce() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(
                &ap,
                PubKey(prov.public_key()),
                1,
                "m",
                "hello",
                32,
                700,
                100,
                0,
            )
            .unwrap();
        pch.accept(&a).unwrap();
        assert_eq!(pch.state.balance_provider, 100);
        let r = pch.sign_result(&prov, 1, "world", 3, 4).unwrap();
        ch.complete_job(&r).unwrap();
        assert_eq!(
            (
                ch.state.balance_requestor,
                ch.state.balance_provider,
                ch.state.nonce
            ),
            (900, 100, 1)
        );
        assert!(ch.state.is_conserved(1_000));
        assert_eq!(ch.pending_len(), 0);
        assert_eq!(pch.pending_len(), 0);
    }

    #[test]
    fn assignment_signature_binds_prompt_and_price() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(
                &ap,
                PubKey(prov.public_key()),
                1,
                "m",
                "hello",
                32,
                700,
                100,
                0,
            )
            .unwrap();
        // Tamper: same state, a bigger prompt / cheaper price.
        let mut t = a.clone();
        t.prompt = "a much bigger prompt".into();
        assert_eq!(pch.accept(&t), Err(HubError::InvalidRequestorSignature));
        // A cheaper price also breaks the state arithmetic, so it is refused
        // before any signature is even considered.
        let mut t2 = a.clone();
        t2.price = 1;
        assert_eq!(pch.accept(&t2), Err(HubError::PaymentNotExact));
        // Untampered still accepted afterwards (no state consumed on error).
        assert!(pch.accept(&a).is_ok());
    }

    #[test]
    fn replayed_assignment_is_rejected() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 10, 0)
            .unwrap();
        pch.accept(&a).unwrap();
        // Exact replay (same nonce + job id).
        assert_eq!(pch.accept(&a), Err(HubError::ReplayedRequestNonce));
        // Same job id with a bumped request nonce also fails.
        let mut b = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 2, "m", "p", 8, 0, 10, 0)
            .unwrap();
        b.job_id = 1;
        // re-sign so the signature is valid for the mutated id
        b.sig_requestor = Signature64(ap.sign_raw(&assignment_payload(&b)));
        assert_eq!(pch.accept(&b), Err(HubError::ReplayedJobId));
    }

    #[test]
    fn provider_cannot_overcharge() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let mut a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 10, 0)
            .unwrap();
        // Provider tries to give itself more than the price.
        a.state_after.balance_provider = 900;
        a.state_after.balance_requestor = 100;
        a.sig_requestor = Signature64(ap.sign_raw(&assignment_payload(&a)));
        a.sig_settle = Signature64(ap.sign_raw(&a.state_after.settle_payload(a.channel_id)));
        assert_eq!(pch.accept(&a), Err(HubError::PaymentNotExact));
    }

    #[test]
    fn ap_cannot_claw_back_provider_balance() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 100, 0)
            .unwrap();
        pch.accept(&a).unwrap();
        let mut r = pch.sign_result(&prov, 1, "out", 1, 1).unwrap();
        // AP tries to rewrite the provider's signed state downward — signature breaks.
        r.state_after.balance_provider = 0;
        r.state_after.balance_requestor = 1_000;
        assert_eq!(ch.complete_job(&r), Err(HubError::InvalidProviderSignature));
        // Even with a fresh, valid provider signature, a state that is not the
        // exact pre-authorised post-state is refused — this is what makes the
        // provider balance monotone with respect to the AP's own signatures.
        // (`ProviderBalanceDecreased` is the belt-and-braces invariant for
        // paths that feed a signed update straight into the state machine.)
        let evil = ChannelState {
            balance_requestor: 1_000,
            balance_provider: 0,
            nonce: 5,
        };
        let mut r2 = r.clone();
        r2.state_after = evil;
        r2.sig_provider = Signature64(prov.sign_raw(&result_payload(&r2)));
        r2.sig_settle = Signature64(prov.sign_raw(&evil.settle_payload(r2.channel_id)));
        assert_eq!(ch.complete_job(&r2), Err(HubError::PaymentNotExact));
        assert_eq!(ch.state.balance_provider, 0, "no state was applied");
        assert_eq!(ch.state.nonce, 0);
    }

    /// The provider holds the AP's pre-signature over the post-job state; that
    /// exact signature verifies against the AP key and is what a force-close
    /// would use. This is the "AP cannot walk away unpaid" property.
    #[test]
    fn provider_holds_a_settleable_ap_signature() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 250, 0)
            .unwrap();
        pch.accept(&a).unwrap();
        assert!(verify_sig(
            &PubKey(ap.public_key()),
            &a.state_after.settle_payload(7),
            &a.sig_settle
        ));
        // Close material after a result has both halves over the same state.
        let r = pch.sign_result(&prov, 1, "out", 1, 1).unwrap();
        ch.complete_job(&r).unwrap();
        let cm = ch.close_material(&ap).unwrap();
        assert_eq!(cm.state, r.state_after);
        assert!(verify_sig(
            &PubKey(ap.public_key()),
            &cm.state.settle_payload(7),
            &cm.sig_requestor
        ));
        assert!(verify_sig(
            &PubKey(prov.public_key()),
            &cm.state.settle_payload(7),
            &cm.sig_provider
        ));
    }

    #[test]
    fn expired_assignment_is_rejected() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let mut a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 10, 0)
            .unwrap();
        a.deadline_ms = 1; // 1970
        a.sig_requestor = Signature64(ap.sign_raw(&assignment_payload(&a)));
        assert_eq!(pch.accept(&a), Err(HubError::Expired));
    }

    #[test]
    fn nonce_rollback_is_rejected() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        for id in 1..=2 {
            let a = ch
                .begin_job_for(&ap, PubKey(prov.public_key()), id, "m", "p", 8, 0, 10, 0)
                .unwrap();
            pch.accept(&a).unwrap();
            let r = pch.sign_result(&prov, id, "out", 1, 1).unwrap();
            ch.complete_job(&r).unwrap();
        }
        // Replaying job 1's result now fails (settled).
        let mut old = JobResultMsg {
            channel_id: 7,
            job_id: 1,
            provider: PubKey(prov.public_key()),
            ok: true,
            output: "out".into(),
            result_hash: Hash32::of(b"out"),
            prompt_tokens: 1,
            completion_tokens: 1,
            state_after: ChannelState {
                balance_requestor: 990,
                balance_provider: 10,
                nonce: 1,
            },
            sig_provider: Signature64([0u8; 64]),
            sig_settle: Signature64([0u8; 64]),
        };
        old.sig_provider = Signature64(prov.sign_raw(&result_payload(&old)));
        old.sig_settle =
            Signature64(prov.sign_raw(&old.state_after.settle_payload(old.channel_id)));
        assert_eq!(ch.complete_job(&old), Err(HubError::UnknownJob));
    }

    #[test]
    fn forged_result_signature_is_rejected() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 10, 0)
            .unwrap();
        pch.accept(&a).unwrap();
        let mut r = pch.sign_result(&prov, 1, "out", 1, 1).unwrap();
        r.output = "forged output".into();
        assert_eq!(ch.complete_job(&r), Err(HubError::InvalidProviderSignature));
    }

    #[test]
    fn result_hash_must_match_the_delivered_output() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 10, 0)
            .unwrap();
        pch.accept(&a).unwrap();
        let mut r = pch.sign_result(&prov, 1, "out", 1, 1).unwrap();
        r.result_hash = Hash32::of(b"something else");
        r.sig_provider = Signature64(prov.sign_raw(&result_payload(&r)));
        assert_eq!(ch.complete_job(&r), Err(HubError::ResultHashMismatch));
    }

    #[test]
    fn failed_job_refunds_the_requestor() {
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 100, 0)
            .unwrap();
        pch.accept(&a).unwrap();
        let r = pch.sign_refund(&prov, 1, "ollama exploded").unwrap();
        ch.complete_job(&r).unwrap();
        assert_eq!(ch.state.balance_requestor, 1_000);
        assert_eq!(ch.state.balance_provider, 0);
        assert_eq!(ch.state.nonce, 2, "refund advances the nonce");
    }

    #[test]
    fn insufficient_balance_is_refused_before_any_signature() {
        let (ap, prov, mut ch, _pch) = setup(100);
        let e = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 101, 0)
            .unwrap_err();
        assert_eq!(e, HubError::InsufficientChannelBalance);
        assert_eq!(ch.state.nonce, 0);
    }

    #[test]
    fn e2e_job_through_a_channel_has_zero_chain_calls() {
        // The whole flow is pure: no chain client exists in this module, so a
        // job that completes here provably issued no extrinsic. Drive 3 jobs
        // and assert the cumulative state + nonce.
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let mut outputs = Vec::new();
        for id in 1..=3u64 {
            let a = ch
                .begin_job_for(
                    &ap,
                    PubKey(prov.public_key()),
                    id,
                    "llama3.2:1b",
                    "say hi",
                    16,
                    700,
                    30,
                    0,
                )
                .unwrap();
            pch.accept(&a).unwrap();
            let out = format!("hi #{id}");
            let r = pch.sign_result(&prov, id, &out, 2, 2).unwrap();
            ch.complete_job(&r).unwrap();
            outputs.push(out);
        }
        assert_eq!(outputs.len(), 3);
        assert_eq!(ch.state.nonce, 3);
        assert_eq!(ch.state.balance_provider, 90);
        assert_eq!(ch.state.balance_requestor, 910);
        assert!(ch.state.is_conserved(1_000));
        // The latest state closes cooperatively with both signatures.
        let cm = ch.close_material(&ap).unwrap();
        assert!(verify_sig(
            &PubKey(ap.public_key()),
            &cm.state.settle_payload(7),
            &cm.sig_requestor
        ));
        assert!(verify_sig(
            &PubKey(prov.public_key()),
            &cm.state.settle_payload(7),
            &cm.sig_provider
        ));
    }

    #[test]
    fn double_settle_of_the_same_job_is_rejected() {
        // The channel path and the on-chain path must never both pay for one
        // job. The guard is that `complete_job` consumes the pending entry:
        // replaying the *exact* provider-signed result is a no-op refusal and
        // the channel state does not move a second time.
        let (ap, prov, mut ch, mut pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 100, 0)
            .unwrap();
        pch.accept(&a).unwrap();
        let r = pch.sign_result(&prov, 1, "out", 1, 1).unwrap();
        ch.complete_job(&r).unwrap();
        assert_eq!(ch.state.balance_provider, 100);
        assert_eq!(ch.state.nonce, 1);
        // Plain replay and a re-signed copy both fail: the job id is settled.
        assert_eq!(ch.complete_job(&r), Err(HubError::UnknownJob));
        let mut again = r.clone();
        again.sig_provider = Signature64(prov.sign_raw(&result_payload(&again)));
        assert_eq!(ch.complete_job(&again), Err(HubError::UnknownJob));
        assert_eq!(ch.state.balance_provider, 100, "no second payment");
        assert_eq!(ch.state.nonce, 1);
        // ...and the provider cannot re-sign it either (it dropped the entry).
        assert_eq!(
            pch.sign_result(&prov, 1, "out", 1, 1).err(),
            Some(HubError::UnknownJob)
        );
    }

    #[test]
    fn pending_entry_can_be_aborted_only_by_the_ap() {
        // `abort_pending` exists for the "assignment provably never ran" case
        // (connect error / 4xx accept-time rejection). It must remove the slot
        // without moving value, so the retry does not collide with a phantom.
        let (ap, prov, mut ch, _pch) = setup(1_000);
        let a = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 100, 0)
            .unwrap();
        assert_eq!(ch.pending_len(), 1);
        assert!(ch.abort_pending(a.job_id));
        assert_eq!(ch.pending_len(), 0);
        assert_eq!(ch.state.nonce, 0, "abort moves no value");
        assert!(!ch.abort_pending(a.job_id), "idempotent");
        // A fresh job reuses the (unchanged) state; the provider's copy of the
        // aborted assignment is worth the same amount, so no double charge is
        // possible even if it force-closes with it.
        let b = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 2, "m", "p", 8, 0, 100, 0)
            .unwrap();
        assert_eq!(b.state_after, a.state_after);
    }

    #[test]
    fn pubkey_parses_hex_and_ss58() {
        // The scheduler's provider registry keys on ss58; signatures need the
        // 32 raw bytes. Both forms must resolve to the same key.
        let ss58 = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj";
        let want = PubKey::parse(ss58).expect("ss58");
        assert_eq!(
            want.to_hex(),
            "0x664c6e12d0eff26c646b764f733f0468d44deea02f856ab9f1b629af06993d01"
        );
        assert_eq!(PubKey::parse(&want.to_hex()), Some(want));
        assert_eq!(PubKey::parse("garbage"), None);
    }

    #[test]
    fn registry_roundtrips_and_keeps_channel_state() {
        let (ap, prov, mut ch, _pch) = setup(500);
        let _ = ch
            .begin_job_for(&ap, PubKey(prov.public_key()), 1, "m", "p", 8, 0, 5, 0)
            .unwrap();
        let mut reg = HubRegistry::default();
        reg.channels.insert(PubKey(prov.public_key()).to_hex(), ch);
        let dir = std::env::temp_dir().join(format!("gpn-hub-{}", std::process::id()));
        let p = dir.join("hub.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(reg.save(&p));
        let back = HubRegistry::load(&p);
        let k = PubKey(prov.public_key()).to_hex();
        let c = back.channels.get(&k).unwrap();
        assert_eq!(c.channel_id, 7);
        assert_eq!(c.request_nonce, 1);
        assert_eq!(c.pending_len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
