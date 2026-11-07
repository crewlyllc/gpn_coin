//! Provider **push** heartbeat — the outbound complement to the access point's
//! inbound `/status` poll.
//!
//! # Why this exists
//!
//! The AP used to decide whether a provider is "serving" by polling
//! `http://<provider>:8093/status` every 5 s and treating
//! `last_seen.is_some()` as health. That makes inbound reachability on port
//! 8093 a requirement for *being counted*, even though p2p (outbound) works
//! fine for every NAT'd home node. Measured 2026-11-02: `/nodes` showed 4
//! peers, 1 serving, because 3 boxes sit behind NAT.
//!
//! The fix is a second mechanism, not a replacement: a provider signs a small
//! heartbeat and POSTs it to `<AP_URL>/provider/heartbeat` every ~10 s. The AP
//! keeps polling public nodes as before (a cross-check), and merges the pushed
//! record with the polled one so a provider is never counted twice.
//!
//! # Trust model
//!
//! Nothing a caller sends is trusted by itself:
//!
//! * the payload is signed with the provider's **own sr25519 account key**;
//! * the AP verifies it against the key that belongs to the address, and only
//!   after the address is confirmed to be a **registered provider on chain**
//!   (`ComputeMarketplace::RegisteredProviders`). On-chain identity is the
//!   authority; a caller-supplied public key is never used.
//! * The signed message is canonical and unambiguous (fixed field order,
//!   `|`-separated, with fixed-length SHA-256 digests for the variable-length
//!   lists), so no two different payloads can share a signature.
//!
//! The ingest rules live here rather than in the access-point binary so they
//! can be tested with an injectable clock: a push expires after [`TTL_SECS`]
//! without sleeping for it.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long a pushed heartbeat counts as evidence that a provider is serving.
pub const TTL_SECS: u64 = 30;

/// Hard cap on stored push records. A public endpoint must never be an
/// unbounded memory sink; the oldest record is evicted first.
pub const MAX_ENTRIES: usize = 500;

/// Largest accepted request body. A heartbeat is well under 2 KB; 64 KB is a
/// generous ceiling that still bounds parse cost.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// More models than this is not a heartbeat, it is an abuse attempt.
pub const MAX_MODELS: usize = 64;

/// Same reasoning as [`MAX_MODELS`], for the dial-target list.
pub const MAX_MULTIADDRS: usize = 64;

/// Maximum clock skew between the provider and the AP. Outside this window the
/// push is rejected as stale (`ts` in the past) or premature (`ts` in the
/// future), which also makes captured pushes useless for replay.
pub const MAX_SKEW_SECS: u64 = 90;

/// Minimum spacing between accepted pushes from one address.
pub const RATE_LIMIT_SECS: u64 = 3;

/// How long a positive on-chain account lookup is cached.
///
/// #144: this was 300 s while providers push their heartbeat every ~5 min, so
/// every push arrived just past expiry, was answered `503 account_lookup_pending`,
/// and the answer the refresher computed was never seen by a request — a lane
/// that failed 100 % while every aggregate health check stayed green. A provider
/// registration is a rare, deliberate event; 15 min is still refreshed well
/// inside the reject-notice window and keeps a 5-minute pusher on `Hit`.
pub const ACCOUNT_CACHE_SECS: u64 = 900;

/// Negative lookups expire sooner: a provider that registers immediately after
/// a rejected push should not have to wait five minutes to be believed.
pub const ACCOUNT_NEGATIVE_TTL_SECS: u64 = 60;

// Tests cover the TTL boundary arithmetic below; both values are part of the
// push lane's contract (#144), not an implementation detail.

/// Default push interval (`GPN_HEARTBEAT_PUSH_SECS`).
pub const DEFAULT_INTERVAL_SECS: u64 = 10;

/// `±` jitter applied to every interval, so a fleet restart does not produce a
/// synchronised thundering herd on the AP.
pub const JITTER_SECS: u64 = 2;

/// The wire payload. Field names are the public contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// libp2p peer id (`12D3Koo…` / `Qm…`).
    pub peer_id: String,
    /// The provider account, SS58.
    pub address: String,
    #[serde(default)]
    pub multiaddrs: Vec<String>,
    /// SPEC 47: `"backbone"` when the node proved inbound reachability,
    /// `"leaf"` otherwise. Absent on pre-SPEC-47 clients; the AP infers it
    /// from `multiaddrs` in that case. Not covered by the v1 signature, so a
    /// v2 canonical message is used whenever it is present (old clients keep
    /// verifying under v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachability: Option<String>,
    /// `true` only for the one dialable machine per LAN/egress
    /// (`GPN_SITE_GATEWAY=1`). Omitted when false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<bool>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub vram_mb: u64,
    #[serde(default)]
    pub capacity: u64,
    #[serde(default)]
    pub active_jobs: u64,
    #[serde(default)]
    pub started_at: u64,
    /// Unix seconds, when the signature was made.
    pub ts: u64,
    /// Hex sr25519 signature over [`canonical_message`], `0x` prefix optional.
    pub sig: String,
}

/// One accepted push, as stored by the AP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedProvider {
    pub peer_id: String,
    pub address: String,
    pub multiaddrs: Vec<String>,
    pub models: Vec<String>,
    pub vram_mb: u64,
    pub capacity: u64,
    pub active_jobs: u64,
    pub started_at: u64,
    pub ts: u64,
    /// `"backbone"` or `"leaf"` — see [`Heartbeat::reachability`].
    pub reachability: String,
    /// Site-gateway role (SPEC 47).
    pub gateway: bool,
    /// Local ingest time (not the provider's clock) — drives the TTL.
    pub pushed_at: u64,
}

impl PushedProvider {
    /// Still inside its [`TTL_SECS`] window?
    pub fn is_live(&self, now: u64) -> bool {
        now.saturating_sub(self.pushed_at) <= TTL_SECS
    }
}

/// Why a push was refused. The access point maps these to HTTP status codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    /// Body larger than [`MAX_BODY_BYTES`].
    TooLarge,
    /// Malformed JSON, missing field, invalid peer id, bad multiaddr, etc.
    BadRequest(&'static str),
    /// The address is not a registered provider, or the signature does not
    /// verify against its on-chain key.
    BadSignature,
    /// `ts` older than [`MAX_SKEW_SECS`].
    Stale,
    /// `ts` further in the future than [`MAX_SKEW_SECS`].
    Future,
    /// More than one push per [`RATE_LIMIT_SECS`] from this address.
    RateLimited,
}

impl IngestError {
    pub fn status_code(&self) -> u16 {
        match self {
            IngestError::TooLarge => 413,
            IngestError::BadRequest(_) => 400,
            IngestError::BadSignature => 403,
            IngestError::Stale | IngestError::Future => 400,
            IngestError::RateLimited => 429,
        }
    }

    /// Operator-facing reason. Deliberately does not echo the payload.
    pub fn reason(&self) -> &'static str {
        match self {
            IngestError::TooLarge => "body too large",
            IngestError::BadRequest(why) => why,
            IngestError::BadSignature => "bad signature or unregistered account",
            IngestError::Stale => "heartbeat timestamp too old",
            IngestError::Future => "heartbeat timestamp in the future",
            IngestError::RateLimited => "rate limited",
        }
    }
}

/// Canonical bytes signed by the provider. Fixed field order, `|` separated,
/// and variable-length lists reduced to fixed-length digests so an attacker
/// cannot shift the boundary between fields.
pub fn canonical_message(hb: &Heartbeat) -> String {
    let mut models = hb.models.clone();
    models.sort();
    models.dedup();
    let mut multiaddrs = hb.multiaddrs.clone();
    multiaddrs.sort();
    multiaddrs.dedup();
    format!(
        "gpn-hb-v1|{}|{}|{}|{}|{}|{}|{}|{}",
        hb.peer_id,
        hb.address,
        hb.ts,
        hb.vram_mb,
        hb.capacity,
        hb.active_jobs,
        sha256_hex(&models.join(",")),
        sha256_hex(&multiaddrs.join(",")),
    )
}

/// v2 canonical bytes: the v1 message plus the tier fields (SPEC 47).
///
/// Only used when the client actually sent a tier, so every pre-SPEC-47
/// heartbeat still verifies against [`canonical_message`] — no fleet-wide
/// signature break on upgrade.
pub fn canonical_message_v2(hb: &Heartbeat) -> String {
    format!(
        "{}|{}|{}",
        canonical_message(hb),
        hb.reachability.as_deref().unwrap_or(""),
        match hb.gateway {
            Some(true) => "1",
            Some(false) => "0",
            None => "",
        }
    )
}

/// Message the sender must sign: v2 when tier metadata is present.
pub fn signed_message(hb: &Heartbeat) -> String {
    if hb.reachability.is_some() || hb.gateway.is_some() {
        canonical_message_v2(hb)
    } else {
        canonical_message(hb)
    }
}

pub fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Sign `hb` in place with the node's account key.
pub fn sign_in_place(signer: &crate::chain::signer::Sr25519Signer, hb: &mut Heartbeat) {
    use sp_core::crypto::Pair as _;
    let sig = signer.pair().sign(signed_message(hb).as_bytes());
    hb.sig = hex::encode(sig.0);
}

/// Verify a hex sr25519 signature over `msg` with a raw 32-byte public key.
pub fn verify_signature(sig_hex: &str, msg: &str, pubkey: &[u8; 32]) -> bool {
    use sp_core::crypto::Pair as _;
    let raw = sig_hex.trim().trim_start_matches("0x");
    let bytes = match hex::decode(raw) {
        Ok(b) if b.len() == 64 => b,
        _ => return false,
    };
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&bytes);
    let sig = sp_core::sr25519::Signature::from_raw(sig);
    let public = sp_core::sr25519::Public::from_raw(*pubkey);
    sp_core::sr25519::Pair::verify(&sig, msg.as_bytes(), &public)
}

/// Decode an SS58 address into its raw 32-byte account public key.
pub fn ss58_to_pubkey(address: &str) -> Option<[u8; 32]> {
    use sp_core::crypto::Ss58Codec;
    sp_core::sr25519::Public::from_ss58check(address.trim())
        .ok()
        .map(|p| p.0)
}

/// Render a raw public key as an SS58 address.
pub fn pubkey_to_ss58(pubkey: &[u8; 32]) -> String {
    use sp_core::crypto::Ss58Codec;
    sp_core::sr25519::Public::from_raw(*pubkey).to_ss58check()
}

/// Shortened address for logs: never the whole value at info level.
pub fn abbreviated(address: &str) -> String {
    let a = address.trim();
    if a.len() <= 10 {
        return "…".to_string();
    }
    format!("{}…{}", &a[..6], &a[a.len() - 4..])
}

/// The AP's store of accepted pushes.
#[derive(Debug, Default)]
pub struct PushRegistry {
    entries: HashMap<(String, String), PushedProvider>,
    last_accept: HashMap<String, u64>,
}

impl PushRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validate, rate-limit and store one push.
    ///
    /// `resolve` returns the **on-chain** public key for an address, or `None`
    /// when the account is unknown / not a registered provider. The AP
    /// implements it with a cached `RegisteredProviders` RPC read; tests pass a
    /// map. A caller-supplied key is never trusted.
    pub fn ingest<F>(&mut self, body: &str, now: u64, mut resolve: F) -> Result<(), IngestError>
    where
        F: FnMut(&str) -> Option<[u8; 32]>,
    {
        if body.len() > MAX_BODY_BYTES {
            return Err(IngestError::TooLarge);
        }
        let hb: Heartbeat =
            serde_json::from_str(body).map_err(|_| IngestError::BadRequest("invalid JSON"))?;

        if crate::seedlist::clean_peer_id(&hb.peer_id).is_none() {
            return Err(IngestError::BadRequest("invalid peer_id"));
        }
        let decoded =
            ss58_to_pubkey(&hb.address).ok_or(IngestError::BadRequest("invalid ss58 address"))?;
        if hb.models.len() > MAX_MODELS {
            return Err(IngestError::BadRequest("too many models"));
        }
        if hb.multiaddrs.len() > MAX_MULTIADDRS {
            return Err(IngestError::BadRequest("too many multiaddrs"));
        }
        for ma in &hb.multiaddrs {
            let Some(clean) = crate::seedlist::clean_published_multiaddr(ma) else {
                return Err(IngestError::BadRequest("invalid multiaddr"));
            };
            // A heartbeat may only advertise addresses that dial THIS peer id.
            let Some((_, tail)) = clean.rsplit_once("/p2p/") else {
                return Err(IngestError::BadRequest("multiaddr without peer id"));
            };
            if tail != hb.peer_id {
                return Err(IngestError::BadRequest("multiaddr peer id mismatch"));
            }
        }

        if now.saturating_sub(hb.ts) > MAX_SKEW_SECS {
            return Err(IngestError::Stale);
        }
        if hb.ts.saturating_sub(now) > MAX_SKEW_SECS {
            return Err(IngestError::Future);
        }

        // SPEC 47: tier is self-reported metadata, not a consensus fact. A
        // pre-SPEC-47 client omits it; infer backbone only if it published a
        // dialable address, otherwise leaf.
        let reachability = match hb.reachability.as_deref() {
            Some("backbone") => "backbone",
            Some("leaf") => "leaf",
            Some(_) => return Err(IngestError::BadRequest("invalid reachability")),
            None if hb.multiaddrs.is_empty() => "leaf",
            None => "backbone",
        };
        let gateway = hb.gateway.unwrap_or(false);

        let Some(onchain) = resolve(&hb.address) else {
            return Err(IngestError::BadSignature);
        };
        // The on-chain key is the authority. For an sr25519 account it IS the
        // SS58 payload; a mismatch means the address names a different key.
        if onchain != decoded {
            return Err(IngestError::BadSignature);
        }
        let msg = signed_message(&hb);
        if !verify_signature(&hb.sig, &msg, &onchain) {
            return Err(IngestError::BadSignature);
        }

        // Rate limit AFTER authentication: an unauthenticated caller cannot
        // spend another provider's budget. Two valid pushes within the window:
        // the second is dropped without touching stored state.
        if let Some(last) = self.last_accept.get(&hb.address).copied() {
            if now.saturating_sub(last) < RATE_LIMIT_SECS {
                return Err(IngestError::RateLimited);
            }
        }

        let key = (hb.peer_id.clone(), hb.address.clone());
        if self.entries.len() >= MAX_ENTRIES && !self.entries.contains_key(&key) {
            self.evict_oldest();
        }
        self.entries.insert(
            key,
            PushedProvider {
                peer_id: hb.peer_id,
                address: hb.address.clone(),
                // A leaf must never be published with a dialable multiaddr
                // (SPEC 47, step 4). Enforce it at the store, so even a buggy
                // or malicious client cannot make a leaf look dialable.
                multiaddrs: if reachability == "leaf" {
                    Vec::new()
                } else {
                    hb.multiaddrs
                        .iter()
                        .filter_map(|m| crate::seedlist::clean_published_multiaddr(m))
                        .collect()
                },
                models: hb.models,
                vram_mb: hb.vram_mb,
                capacity: hb.capacity,
                active_jobs: hb.active_jobs,
                started_at: hb.started_at,
                ts: hb.ts,
                reachability: reachability.to_string(),
                gateway,
                pushed_at: now,
            },
        );
        self.last_accept.insert(hb.address, now);
        Ok(())
    }

    /// Records still inside their TTL, newest first (stable for merging).
    pub fn live(&self, now: u64) -> Vec<&PushedProvider> {
        let mut live: Vec<&PushedProvider> =
            self.entries.values().filter(|e| e.is_live(now)).collect();
        live.sort_by_key(|e| std::cmp::Reverse(e.pushed_at));
        live
    }

    /// Drop expired entries and stale rate-limit marks. Called by the AP's
    /// refresh loop, so a provider that goes silent disappears within a minute.
    pub fn prune(&mut self, now: u64) {
        self.entries.retain(|_, e| e.is_live(now));
        self.last_accept
            .retain(|_, at| now.saturating_sub(*at) <= ACCOUNT_CACHE_SECS);
    }

    fn evict_oldest(&mut self) {
        if let Some(oldest) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.pushed_at)
            .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
    }
}

/// Result of a cache lookup: a cached answer (`Hit`) or a miss that the caller
/// must resolve against the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedKey {
    Hit(Option<[u8; 32]>),
    Miss,
}

/// Five-minute cache of on-chain account lookups.
///
/// Kept separate from [`PushRegistry`] so the (slow, fallible) RPC read never
/// happens while the registry lock is held.
#[derive(Debug, Default)]
pub struct AccountAuthority {
    cache: HashMap<String, (u64, Option<[u8; 32]>)>,
}

impl AccountAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, address: &str, now: u64) -> CachedKey {
        let Some((at, key)) = self.cache.get(address) else {
            return CachedKey::Miss;
        };
        let ttl = if key.is_some() {
            ACCOUNT_CACHE_SECS
        } else {
            ACCOUNT_NEGATIVE_TTL_SECS
        };
        if now.saturating_sub(*at) <= ttl {
            CachedKey::Hit(*key)
        } else {
            CachedKey::Miss
        }
    }

    pub fn store(&mut self, address: &str, now: u64, key: Option<[u8; 32]>) {
        self.cache.insert(address.to_string(), (now, key));
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

/// How often a persistent "rejected heartbeat" condition may be re-stated.
///
/// A peer whose account is not a registered provider keeps pushing on its own
/// heartbeat interval, so the access point's journal filled with the identical
/// line every ~12 s: **6,947 copies in 24 h** from one box on 2026-11-03. The
/// signal is real and must not be hidden — but repeating it verbatim buries
/// every other line in the journal, which is where real faults are found.
/// One notice per address per window, carrying the number suppressed.
pub const REJECT_LOG_INTERVAL_SECS: u64 = 600;

/// Upper bound on tracked addresses, so a scanner walking random (structurally
/// valid) ss58 strings cannot grow this map without limit.
const REJECT_LOG_MAX_ADDRS: usize = 512;

/// What the caller should print for this rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectNotice {
    /// First rejection from this address in the window: log it in full.
    First,
    /// The window elapsed while it kept happening: log a counted summary.
    Summary { suppressed: u64 },
    /// Already told the operator recently: stay quiet.
    Suppressed,
}

/// Per-address throttle for the "not a registered provider" journal line.
#[derive(Default)]
pub struct RejectLog {
    /// address -> (last time it was logged, rejections since then)
    seen: HashMap<String, (u64, u64)>,
}

impl RejectLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one rejection and answers whether it deserves a line.
    pub fn record(&mut self, address: &str, now: u64) -> RejectNotice {
        if let Some((last, suppressed)) = self.seen.get_mut(address) {
            if now >= *last && now - *last >= REJECT_LOG_INTERVAL_SECS {
                let count = *suppressed;
                *last = now;
                *suppressed = 0;
                return RejectNotice::Summary { suppressed: count };
            }
            *suppressed += 1;
            return RejectNotice::Suppressed;
        }
        if self.seen.len() >= REJECT_LOG_MAX_ADDRS {
            // Drop entries that have gone quiet, then start over rather than
            // silently refusing to report a new offender.
            let cutoff = now.saturating_sub(REJECT_LOG_INTERVAL_SECS * 6);
            self.seen.retain(|_, (last, _)| *last >= cutoff);
            if self.seen.len() >= REJECT_LOG_MAX_ADDRS {
                self.seen.clear();
            }
        }
        self.seen.insert(address.to_string(), (now, 0));
        RejectNotice::First
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Provider-side push configuration, resolved from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSettings {
    pub enabled: bool,
    pub interval_secs: u64,
}

impl PushSettings {
    /// Pure parser, so the disable matrix is testable without the process env.
    ///
    /// `GPN_HEARTBEAT_PUSH` is on unless explicitly `0`/`false`/`off`/`no`.
    /// `GPN_HEARTBEAT_PUSH_SECS` is clamped to at least 1 s.
    pub fn parse(push: Option<&str>, secs: Option<&str>) -> Self {
        let enabled = match push.map(|s| s.trim().to_ascii_lowercase()) {
            Some(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no" | "disabled"),
            None => true,
        };
        let interval_secs = secs
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_INTERVAL_SECS);
        Self {
            enabled,
            interval_secs,
        }
    }

    pub fn from_env() -> Self {
        let push = std::env::var("GPN_HEARTBEAT_PUSH").ok();
        let secs = std::env::var("GPN_HEARTBEAT_PUSH_SECS").ok();
        Self::parse(push.as_deref(), secs.as_deref())
    }

    /// `base + jitter`, where `jitter` is in `[-JITTER_SECS, +JITTER_SECS]`.
    pub fn interval_with_jitter(&self, roll: i64) -> Duration {
        let j = JITTER_SECS as i64;
        let offset = if j == 0 {
            0
        } else {
            roll.rem_euclid(2 * j + 1) - j
        };
        let secs = (self.interval_secs as i64 + offset).max(1) as u64;
        Duration::from_secs(secs)
    }
}

/// Run a push loop. Extracted from the binary so "a failed push never stops
/// the loop" is a unit test, not a claim.
///
/// * `max_iters` — `None` in production, `Some(n)` in tests.
/// * `push` — one attempt; errors are logged (at most once per minute) and the
///   loop continues.
/// * `sleep` — injected so tests do not wait.
pub fn run_push_loop<P, S>(
    settings: &PushSettings,
    max_iters: Option<usize>,
    mut push: P,
    mut sleep: S,
) where
    P: FnMut() -> Result<(), String>,
    S: FnMut(Duration),
{
    if !settings.enabled {
        return;
    }
    let mut last_error_log: Option<std::time::Instant> = None;
    let mut roll: i64 = 0;
    let mut done = 0usize;
    loop {
        if let Some(max) = max_iters {
            if done >= max {
                return;
            }
        }
        done += 1;
        if let Err(e) = push() {
            let now = std::time::Instant::now();
            let should_log = match last_error_log {
                None => true,
                Some(at) => now.duration_since(at) >= Duration::from_secs(60),
            };
            if should_log {
                eprintln!(
                    "[heartbeat] push failed: {e} (non-fatal; continuing poll-only until it recovers)"
                );
                last_error_log = Some(now);
            }
        }
        // A deterministic pseudo-roll is enough: jitter only has to decorrelate
        // a fleet, not be cryptographic.
        roll = roll.wrapping_mul(6364136223846793005).wrapping_add(1);
        sleep(settings.interval_with_jitter(roll >> 33));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::signer::Sr25519Signer;

    const PEER: &str = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
    const SEED: &str = "//Alice";

    fn signer() -> Sr25519Signer {
        Sr25519Signer::from_uri(SEED).unwrap()
    }

    fn address() -> String {
        use sp_core::crypto::Ss58Codec;
        sp_core::sr25519::Public::from_raw(signer().public_key()).to_ss58check()
    }

    fn payload(ts: u64) -> Heartbeat {
        Heartbeat {
            peer_id: PEER.to_string(),
            address: address(),
            multiaddrs: vec![format!("/ip4/203.0.113.7/tcp/30333/p2p/{PEER}")],
            reachability: None,
            gateway: None,
            models: vec!["llama3.2:1b".into(), "qwen2.5:1b".into()],
            vram_mb: 8188,
            capacity: 1,
            active_jobs: 0,
            started_at: 1_700_000_000,
            ts,
            sig: String::new(),
        }
    }

    fn signed(ts: u64) -> Heartbeat {
        let mut hb = payload(ts);
        sign_in_place(&signer(), &mut hb);
        hb
    }

    fn resolver(addr: &str) -> Option<[u8; 32]> {
        if addr == address() {
            Some(signer().public_key())
        } else {
            None
        }
    }

    fn ingest(reg: &mut PushRegistry, hb: &Heartbeat, now: u64) -> Result<(), IngestError> {
        reg.ingest(&serde_json::to_string(hb).unwrap(), now, resolver)
    }

    #[test]
    fn canonical_message_is_stable_and_domain_separated() {
        let mut a = payload(100);
        a.models = vec!["b".into(), "a".into()];
        let mut b = payload(100);
        b.models = vec!["a".into(), "b".into()];
        assert_eq!(canonical_message(&a), canonical_message(&b));
        assert!(canonical_message(&a).starts_with("gpn-hb-v1|"));
        // A different field must change the digest.
        b.vram_mb = 4096;
        assert_ne!(canonical_message(&a), canonical_message(&b));
    }

    #[test]
    fn push_accepted_then_expires_with_injectable_clock() {
        let mut reg = PushRegistry::new();
        let hb = signed(1_000);
        ingest(&mut reg, &hb, 1_000).expect("valid push must be accepted");
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.live(1_000).len(), 1);
        assert!(reg.live(1_000)[0].is_live(1_000));
        // Still live at the boundary, gone one second later — no sleeping.
        assert_eq!(reg.live(1_000 + TTL_SECS).len(), 1);
        assert_eq!(reg.live(1_000 + TTL_SECS + 1).len(), 0);
        reg.prune(1_000 + TTL_SECS + 1);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn bad_signature_is_rejected() {
        let mut reg = PushRegistry::new();
        let mut hb = signed(1_000);
        // Flip one byte of the signature.
        let mut raw = hex::decode(hb.sig.trim_start_matches("0x")).unwrap();
        raw[0] ^= 0x01;
        hb.sig = hex::encode(raw);
        assert_eq!(ingest(&mut reg, &hb, 1_000), Err(IngestError::BadSignature));
        assert!(reg.is_empty());
    }

    #[test]
    fn unregistered_account_is_rejected() {
        let mut reg = PushRegistry::new();
        // Validly signed, but the account authority does not know this address
        // (e.g. same key, different SS58 prefix decodes to the same bytes — so
        // use a different key entirely).
        let other = Sr25519Signer::from_uri("//Bob").unwrap();
        let mut hb = payload(1_000);
        hb.address = {
            use sp_core::crypto::Ss58Codec;
            sp_core::sr25519::Public::from_raw(other.public_key()).to_ss58check()
        };
        sign_in_place(&other, &mut hb);
        assert_eq!(ingest(&mut reg, &hb, 1_000), Err(IngestError::BadSignature));
        assert!(reg.is_empty());
    }

    #[test]
    fn stale_timestamp_is_rejected() {
        let mut reg = PushRegistry::new();
        let hb = signed(1_000);
        assert_eq!(
            ingest(&mut reg, &hb, 1_000 + MAX_SKEW_SECS + 1),
            Err(IngestError::Stale)
        );
        assert!(reg.is_empty());
    }

    #[test]
    fn future_timestamp_is_rejected() {
        let mut reg = PushRegistry::new();
        let hb = signed(1_000 + MAX_SKEW_SECS + 1);
        assert_eq!(ingest(&mut reg, &hb, 1_000), Err(IngestError::Future));
        assert!(reg.is_empty());
    }

    #[test]
    fn oversized_body_is_rejected() {
        let mut reg = PushRegistry::new();
        let mut hb = payload(1_000);
        hb.models = vec!["x".repeat(MAX_BODY_BYTES + 1)];
        assert_eq!(ingest(&mut reg, &hb, 1_000), Err(IngestError::TooLarge));
        assert!(reg.is_empty());
    }

    #[test]
    fn too_many_models_is_rejected() {
        let mut reg = PushRegistry::new();
        let mut hb = payload(1_000);
        hb.models = (0..=MAX_MODELS).map(|i| format!("m{i}")).collect();
        assert_eq!(
            ingest(&mut reg, &hb, 1_000),
            Err(IngestError::BadRequest("too many models"))
        );
    }

    #[test]
    fn wrong_ss58_is_rejected() {
        let mut reg = PushRegistry::new();
        let mut hb = payload(1_000);
        hb.address = "not-an-address".into();
        assert_eq!(
            ingest(&mut reg, &hb, 1_000),
            Err(IngestError::BadRequest("invalid ss58 address"))
        );
    }

    #[test]
    fn invalid_peer_id_is_rejected() {
        let mut reg = PushRegistry::new();
        let mut hb = payload(1_000);
        hb.peer_id = "nope".into();
        assert_eq!(
            ingest(&mut reg, &hb, 1_000),
            Err(IngestError::BadRequest("invalid peer_id"))
        );
    }

    #[test]
    fn multiaddr_with_another_peer_id_is_rejected() {
        let mut reg = PushRegistry::new();
        let mut hb = payload(1_000);
        hb.multiaddrs = vec![
            "/ip4/203.0.113.7/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFp"
                .into(),
        ];
        assert_eq!(
            ingest(&mut reg, &hb, 1_000),
            Err(IngestError::BadRequest("multiaddr peer id mismatch"))
        );
    }

    #[test]
    fn rate_limit_drops_the_second_push_and_keeps_state() {
        let mut reg = PushRegistry::new();
        let first = signed(1_000);
        ingest(&mut reg, &first, 1_000).unwrap();
        let stored = reg.live(1_000)[0].clone();

        let mut second = signed(1_001);
        second.models = vec!["different".into()];
        sign_in_place(&signer(), &mut second);
        assert_eq!(
            ingest(&mut reg, &second, 1_001),
            Err(IngestError::RateLimited)
        );
        assert_eq!(reg.len(), 1);
        // The stored record is untouched, not half-overwritten.
        assert_eq!(*reg.live(1_001)[0], stored);

        // After the window a new push is accepted.
        let mut third = signed(1_003);
        third.models = vec!["different".into()];
        sign_in_place(&signer(), &mut third);
        ingest(&mut reg, &third, 1_003).unwrap();
        assert_eq!(reg.live(1_003)[0].models, vec!["different".to_string()]);
    }

    #[test]
    fn registry_cap_evicts_the_oldest_entry() {
        let mut reg = PushRegistry::new();
        // Fill to the cap with directly-inserted records (rate limit would
        // otherwise reject a tight loop from one address).
        for i in 0..MAX_ENTRIES {
            let key = (format!("{PEER}{i}"), address());
            reg.entries.insert(
                key,
                PushedProvider {
                    peer_id: format!("{PEER}{i}"),
                    address: address(),
                    multiaddrs: vec![],
                    models: vec![],
                    vram_mb: 0,
                    capacity: 1,
                    active_jobs: 0,
                    started_at: 0,
                    ts: 1_000,
                    reachability: "backbone".to_string(),
                    gateway: false,
                    pushed_at: i as u64,
                },
            );
        }
        // Newest is `MAX_ENTRIES - 1`; the next ingest must evict `0`.
        ingest(&mut reg, &signed(1_950), 2_000).unwrap();
        assert_eq!(reg.len(), MAX_ENTRIES);
        assert!(!reg.entries.contains_key(&(format!("{PEER}0"), address())));
    }

    #[test]
    fn account_authority_caches_positive_and_negative() {
        let mut auth = AccountAuthority::new();
        assert_eq!(auth.lookup("a", 100), CachedKey::Miss);
        auth.store("a", 100, Some([7u8; 32]));
        assert_eq!(
            auth.lookup("a", 100 + ACCOUNT_CACHE_SECS),
            CachedKey::Hit(Some([7u8; 32]))
        );
        assert_eq!(
            auth.lookup("a", 100 + ACCOUNT_CACHE_SECS + 1),
            CachedKey::Miss
        );

        auth.store("b", 100, None);
        assert_eq!(
            auth.lookup("b", 100 + ACCOUNT_NEGATIVE_TTL_SECS),
            CachedKey::Hit(None)
        );
        assert_eq!(
            auth.lookup("b", 100 + ACCOUNT_NEGATIVE_TTL_SECS + 1),
            CachedKey::Miss
        );
    }

    #[test]
    fn push_settings_disable_matrix() {
        assert!(!PushSettings::parse(Some("0"), None).enabled);
        assert!(!PushSettings::parse(Some("off"), None).enabled);
        assert!(!PushSettings::parse(Some("false"), None).enabled);
        assert!(PushSettings::parse(None, None).enabled);
        assert_eq!(PushSettings::parse(None, None).interval_secs, 10);
        assert_eq!(PushSettings::parse(None, Some("25")).interval_secs, 25);
        // Zero/invalid falls back to the default, never to a busy loop.
        assert_eq!(PushSettings::parse(None, Some("0")).interval_secs, 10);
        assert_eq!(PushSettings::parse(None, Some("junk")).interval_secs, 10);
    }

    #[test]
    fn jitter_stays_within_the_documented_band() {
        let s = PushSettings::parse(None, Some("10"));
        for roll in -20i64..=20 {
            let secs = s.interval_with_jitter(roll).as_secs();
            assert!((8..=12).contains(&secs), "roll {roll} gave {secs}s");
        }
    }

    #[test]
    fn failed_push_does_not_stop_the_loop() {
        let settings = PushSettings::parse(None, Some("10"));
        let mut attempts = 0;
        let mut sleeps = 0;
        run_push_loop(
            &settings,
            Some(3),
            || {
                attempts += 1;
                Err("ap unreachable".to_string())
            },
            |_| sleeps += 1,
        );
        assert_eq!(attempts, 3, "every iteration must run despite failures");
        assert_eq!(sleeps, 3);
    }

    #[test]
    fn disabled_push_loop_never_runs() {
        let settings = PushSettings::parse(Some("0"), None);
        let mut attempts = 0;
        run_push_loop(
            &settings,
            Some(3),
            || {
                attempts += 1;
                Ok(())
            },
            |_| {},
        );
        assert_eq!(attempts, 0);
    }

    #[test]
    fn a_persistent_rejection_is_stated_once_per_window_not_every_heartbeat() {
        // One box pushing every 12 s for an hour must not write 301 lines.
        let mut log = RejectLog::new();
        let start = 1_000u64;
        let interval = 12u64;
        assert_eq!(log.record("5Hgbpi", start), RejectNotice::First);
        let mut lines = 1u64;
        let mut suppressed_total = 0u64;
        let mut summaries = 0u64;
        let mut t = start;
        for _ in 0..300u64 {
            t += interval;
            match log.record("5Hgbpi", t) {
                RejectNotice::Suppressed => {}
                RejectNotice::Summary { suppressed } => {
                    summaries += 1;
                    suppressed_total += suppressed;
                    lines += 1;
                }
                RejectNotice::First => panic!("unchanged condition must not re-report as new"),
            }
        }
        assert_eq!(summaries, 6, "one summary per 10-minute window in an hour");
        assert_eq!(lines, 7, "301 rejections -> 7 journal lines, not 301");
        assert_eq!(
            suppressed_total, 294,
            "the summaries still account for all 300"
        );
        // A different offender is reported immediately: suppression is per address.
        assert_eq!(log.record("5Other", t), RejectNotice::First);
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn the_rejection_log_cannot_grow_without_limit() {
        // A scanner walking random-but-valid addresses must not be a memory leak.
        let mut log = RejectLog::new();
        for i in 0..(REJECT_LOG_MAX_ADDRS as u64 * 3) {
            log.record(&format!("addr-{i}"), 10_000 + i);
        }
        assert!(log.len() <= REJECT_LOG_MAX_ADDRS, "len = {}", log.len());
        // And it still reports a fresh offender after pruning.
        assert_eq!(log.record("addr-fresh", 20_000), RejectNotice::First);
    }

    // ── SPEC 47: reachability tier over the wire ─────────────────────────

    #[test]
    fn a_tiered_heartbeat_uses_the_v2_signature_and_still_verifies() {
        let mut hb = payload(1_000);
        hb.reachability = Some("backbone".into());
        hb.gateway = Some(true);
        sign_in_place(&signer(), &mut hb);
        // It signed the v2 message, not v1.
        assert!(signed_message(&hb).starts_with("gpn-hb-v1|"));
        assert!(signed_message(&hb).contains("|backbone|1"));
        let mut reg = PushRegistry::new();
        ingest(&mut reg, &hb, 1_000).expect("v2 push must verify");
        let row = &reg.live(1_000)[0];
        assert_eq!(row.reachability, "backbone");
        assert!(row.gateway);
    }

    #[test]
    fn a_pre_spec47_heartbeat_still_verifies_under_v1() {
        // No tier fields: the signature is the old v1 message, byte for byte.
        let hb = signed(1_000);
        assert_eq!(signed_message(&hb), canonical_message(&hb));
        let mut reg = PushRegistry::new();
        ingest(&mut reg, &hb, 1_000).expect("old clients must keep working");
        // Inference: it published an endpoint, so it is backbone.
        assert_eq!(reg.live(1_000)[0].reachability, "backbone");
    }

    #[test]
    fn a_leaf_is_never_stored_with_a_dialable_multiaddr() {
        let mut hb = payload(1_000);
        hb.reachability = Some("leaf".into());
        hb.gateway = Some(true);
        sign_in_place(&signer(), &mut hb);
        assert!(!hb.multiaddrs.is_empty(), "client claimed an address");
        let mut reg = PushRegistry::new();
        ingest(&mut reg, &hb, 1_000).expect("valid signature");
        let row = &reg.live(1_000)[0];
        assert_eq!(row.reachability, "leaf");
        assert!(
            row.multiaddrs.is_empty(),
            "a leaf must never be republished with a dialable address"
        );
        // The gateway flag is role metadata and survives a leaf tier.
        assert!(row.gateway);
    }

    #[test]
    fn an_unknown_tier_string_is_rejected() {
        let mut hb = payload(1_000);
        hb.reachability = Some("supernode".into());
        sign_in_place(&signer(), &mut hb);
        let mut reg = PushRegistry::new();
        assert_eq!(
            ingest(&mut reg, &hb, 1_000),
            Err(IngestError::BadRequest("invalid reachability"))
        );
        assert!(reg.is_empty());
    }

    #[test]
    fn a_wire_heartbeat_carries_the_tier_fields() {
        let mut hb = payload(1_000);
        hb.reachability = Some("leaf".into());
        hb.gateway = Some(true);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&hb).unwrap()).unwrap();
        assert_eq!(v["reachability"], "leaf");
        assert_eq!(v["gateway"], true);
        // Absent fields don't appear at all (older clients look unchanged).
        let bare = payload(1_000);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&bare).unwrap()).unwrap();
        assert!(v.get("reachability").is_none());
        assert!(v.get("gateway").is_none());
    }
}
