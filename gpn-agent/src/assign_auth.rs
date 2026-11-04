//! Signed authentication for the provider **pull** endpoint (`GET /assignments`).
//!
//! # Why this exists
//!
//! `/assignments` returns the user's actual prompt, so it must stay
//! authenticated. The first mechanism was a shared static secret
//! (`GPN_ASSIGN_TOKEN`) that lives on the access point — which has no
//! distribution path to a provider on another machine. Measured 2026-11-04:
//! box 1 ran a perfectly good node, yet every `[assign]` pull returned
//! `401 Unauthorized` because the operator never received a token. A shared
//! secret is also the wrong shape for this: it is one credential for the whole
//! fleet, it cannot be rotated per provider, and it proves nothing about *who*
//! is asking.
//!
//! The replacement is a request signed by the provider's **own registered
//! account** — the same key it already signs the push heartbeat with. The AP
//! verifies the signature against the account's on-chain identity and checks
//! that the account is a registered provider. Nothing needs to be distributed.
//!
//! # Wire contract
//!
//! Every pull request carries four headers:
//!
//! | Header | Value |
//! |---|---|
//! | `x-gpn-account` | the signing account, SS58 |
//! | `x-gpn-ts` | unix seconds when the signature was made |
//! | `x-gpn-nonce` | 16 random bytes, lowercase hex (32 chars) |
//! | `x-gpn-sig` | hex sr25519 signature (`0x` optional) |
//!
//! The signature is over the canonical message, `|`-separated and unambiguous:
//!
//! ```text
//! gpn-assign-v1|<account>|<ts>|<nonce>|<method>|<path>|<sha256(body)>
//! ```
//!
//! `path` is the request target **including the query string** — it carries
//! `provider`, `since` and the capability snapshot, so leaving it unsigned
//! would let any caller read another provider's assignments. `body` is empty
//! for the GET poll, so its digest is the SHA-256 of the empty string.
//!
//! # Modes
//!
//! `GPN_ASSIGN_AUTH` selects what the AP accepts:
//!
//! * `both` (**default** during the transition) — a request that presents
//!   signature headers is verified as signed; a legacy request with no
//!   signature headers may use `x-gpn-assign-token` (the same name the legacy
//!   AP already reads, case-insensitively).
//! * `signature` — signatures only; the legacy token is refused (target state).
//! * `token` — legacy shared secret only (kept so an old AP/provider pair keeps
//!   working; deprecated).
//!
//! A provider with no signer (no `GPN_SIGNER`/`SIGNER_URI`/keystore) falls back
//! to the legacy token path and logs once, so an existing deployment does not
//! break the day this lands.
//!
//! # Privacy
//!
//! Nothing here logs a request body or a prompt. Callers log the account
//! (abbreviated), method, path and result only.

use std::collections::{HashMap, VecDeque};

use crate::chain::signer::Sr25519Signer;
use crate::provider_heartbeat::{pubkey_to_ss58, ss58_to_pubkey, verify_signature};

/// Domain string of the canonical signed message.
pub const DOMAIN: &str = "gpn-assign-v1";

/// Maximum accepted clock skew in either direction.
pub const MAX_SKEW_SECS: u64 = 90;

/// How long a nonce is remembered. Must exceed [`MAX_SKEW_SECS`] with margin,
/// otherwise a captured request could be replayed once its nonce expires while
/// its timestamp is still inside the skew window.
pub const NONCE_TTL_SECS: u64 = 180;

/// Upper bound on remembered nonces per account. A provider polls every 25 s,
/// so this is ~2 hours of history; a 20-request burst inside the TTL is normal
/// and never evicts a live nonce.
pub const MAX_NONCES_PER_ACCOUNT: usize = 256;

/// Header names — the public contract.
pub const H_ACCOUNT: &str = "x-gpn-account";
pub const H_TS: &str = "x-gpn-ts";
pub const H_NONCE: &str = "x-gpn-nonce";
pub const H_SIG: &str = "x-gpn-sig";
/// Legacy shared secret (still read in `both`/`token` mode).
pub const H_TOKEN: &str = "x-gpn-assign-token";

/// Env var selecting the accepted auth mechanism.
pub const AUTH_MODE_ENV: &str = "GPN_ASSIGN_AUTH";

/// Which mechanism(s) the access point accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Signature only. The target state.
    Signature,
    /// Legacy shared secret only. Deprecated.
    Token,
    /// Signature when presented, else the legacy token (default).
    Both,
}

impl AuthMode {
    /// Parse `GPN_ASSIGN_AUTH`. Unknown values fall back to [`AuthMode::Both`]
    /// so a typo can never silently lock the whole fleet out of work.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "signature" | "sig" => AuthMode::Signature,
            "token" => AuthMode::Token,
            _ => AuthMode::Both,
        }
    }

    /// Resolve from the environment (default `both`).
    pub fn from_env() -> Self {
        match std::env::var(AUTH_MODE_ENV) {
            Ok(raw) => {
                let mode = Self::parse(&raw);
                if !matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "signature" | "sig" | "token" | "both"
                ) {
                    eprintln!(
                        "[assign] unknown {AUTH_MODE_ENV}={raw:?} — using `both` \
                         (valid: signature | token | both)"
                    );
                }
                mode
            }
            Err(_) => AuthMode::Both,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::Signature => "signature",
            AuthMode::Token => "token",
            AuthMode::Both => "both",
        }
    }
}

/// Which mechanism actually admitted a request (for the AP's log line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Signature,
    Token,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMethod::Signature => "signature",
            AuthMethod::Token => "token",
        }
    }
}

/// Why a pull request was refused. Every variant maps to HTTP 401 with a
/// specific `reason` — never a bare 401, because "unauthorized" with no cause
/// is exactly the failure mode that cost a day of debugging on box 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignAuthError {
    /// The signing account is not a registered provider on chain.
    Unregistered,
    /// `ts` older than [`MAX_SKEW_SECS`].
    StaleTs,
    /// `ts` further in the future than [`MAX_SKEW_SECS`].
    FutureTs,
    /// The nonce was already used inside its TTL.
    ReplayedNonce,
    /// Signature does not verify against the account's on-chain key.
    BadSignature,
    /// Credentials the selected mode requires were not presented.
    MissingCredentials,
    /// Legacy token missing or wrong.
    BadToken,
    /// The signed account is not the provider named in the query string.
    ProviderMismatch,
    /// `ts` / `nonce` are not well formed.
    Malformed(&'static str),
}

impl AssignAuthError {
    /// Stable machine-readable reason (goes in the JSON body and the log).
    pub fn reason(&self) -> &'static str {
        match self {
            AssignAuthError::Unregistered => "unregistered",
            AssignAuthError::StaleTs => "stale_ts",
            AssignAuthError::FutureTs => "future_ts",
            AssignAuthError::ReplayedNonce => "replayed_nonce",
            AssignAuthError::BadSignature => "bad_signature",
            AssignAuthError::MissingCredentials => "missing_credentials",
            AssignAuthError::BadToken => "bad_token",
            AssignAuthError::ProviderMismatch => "provider_mismatch",
            AssignAuthError::Malformed(why) => why,
        }
    }

    /// All refusals are 401: the caller is not allowed to read assignments.
    pub fn status_code(&self) -> u16 {
        401
    }
}

/// The credential material presented by a pull request.
#[derive(Debug, Clone, Copy, Default)]
pub struct Credentials<'a> {
    pub account: &'a str,
    pub ts: u64,
    pub nonce: &'a str,
    pub sig: &'a str,
    /// HTTP method, uppercase (`GET`).
    pub method: &'a str,
    /// Request target **including** the query string (`/assignments?provider=…`).
    pub target: &'a str,
    pub body: &'a [u8],
    /// Legacy shared secret, when presented.
    pub token: &'a str,
}

impl Credentials<'_> {
    /// Does this request present a usable set of signature headers?
    pub fn has_signature(&self) -> bool {
        !self.account.trim().is_empty()
            && !self.nonce.trim().is_empty()
            && !self.sig.trim().is_empty()
            && self.ts > 0
    }
}

/// The canonical message signed by the provider.
pub fn canonical_message(
    account: &str,
    ts: u64,
    nonce: &str,
    method: &str,
    target: &str,
    body: &[u8],
) -> String {
    format!(
        "{DOMAIN}|{}|{}|{}|{}|{}|{}",
        account.trim(),
        ts,
        nonce.trim(),
        method.trim().to_ascii_uppercase(),
        target,
        sha256_bytes_hex(body)
    )
}

/// SHA-256 of raw bytes, lowercase hex.
pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Build the four signature headers for one request.
///
/// Returns `(name, value)` pairs ready for `RequestBuilder::header`. Kept pure
/// (the caller supplies `ts` and `nonce`) so the wire format is testable
/// without a clock or an RNG.
pub fn sign_headers(
    signer: &Sr25519Signer,
    account: &str,
    method: &str,
    target: &str,
    body: &[u8],
    ts: u64,
    nonce: &str,
) -> Vec<(String, String)> {
    use sp_core::crypto::Pair as _;
    let msg = canonical_message(account, ts, nonce, method, target, body);
    let sig = signer.pair().sign(msg.as_bytes());
    vec![
        (H_ACCOUNT.to_string(), account.to_string()),
        (H_TS.to_string(), ts.to_string()),
        (H_NONCE.to_string(), nonce.to_string()),
        (H_SIG.to_string(), hex::encode(sig.0)),
    ]
}

/// 16 random bytes as lowercase hex — a fresh nonce per request.
pub fn new_nonce() -> Option<String> {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).ok()?;
    Some(hex::encode(buf))
}

/// Canonicalise a provider identifier (`0x`-hex or ss58) to ss58.
pub fn account_ss58(address: &str) -> Option<String> {
    let a = address.trim();
    if let Some(raw) = a.strip_prefix("0x") {
        let bytes: [u8; 32] = hex::decode(raw).ok()?.try_into().ok()?;
        return Some(pubkey_to_ss58(&bytes));
    }
    ss58_to_pubkey(a).map(|p| pubkey_to_ss58(&p))
}

/// Do two identifiers name the same account? Used to bind the signed account
/// to the `provider` in the query string.
pub fn same_account(a: &str, b: &str) -> bool {
    match (account_ss58(a), account_ss58(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Resolve the provider's signing key from the same precedence the rest of the
/// agent uses: `GPN_SIGNER`, `SIGNER_URI` (set for supervised provider
/// children), `GPN_SIGNER_FILE`, then `~/.gpn/keystore.json`.
///
/// `None` means "this process cannot sign" — the caller falls back to the
/// legacy token path.
pub fn signer_from_env() -> Option<Sr25519Signer> {
    for var in ["GPN_SIGNER", "SIGNER_URI"] {
        if let Ok(uri) = std::env::var(var) {
            let uri = uri.trim().to_string();
            if uri.is_empty() {
                continue;
            }
            match Sr25519Signer::from_uri(&uri) {
                Ok(s) => return Some(s),
                Err(e) => {
                    eprintln!("[assign] {var} is unusable: {e} — trying the next source");
                }
            }
        }
    }
    let path = std::env::var("GPN_SIGNER_FILE")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            let p = std::path::PathBuf::from(format!("{home}/.gpn/keystore.json"));
            p.exists().then_some(p)
        });
    let path = path?;
    let ks = crate::firstrun::read_keystore(&path).ok()?;
    Sr25519Signer::from_uri(&ks.mnemonic).ok()
}

/// Per-account replay protection: a bounded ring buffer of `(nonce, expiry)`.
#[derive(Debug, Default)]
pub struct ReplayCache {
    seen: HashMap<String, VecDeque<(String, u64)>>,
}

impl ReplayCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `nonce` for `account`, returning `false` when it was already seen
    /// inside its TTL (i.e. a replay).
    ///
    /// Call this **after** the signature verifies, so an unauthenticated caller
    /// cannot fill the buffer with garbage and evict a real nonce.
    pub fn check_and_record(&mut self, account: &str, nonce: &str, now: u64) -> bool {
        let entry = self.seen.entry(account.to_string()).or_default();
        while let Some(front) = entry.front() {
            if front.1 <= now {
                entry.pop_front();
            } else {
                break;
            }
        }
        let nonce = nonce.trim().to_ascii_lowercase();
        if entry.iter().any(|(n, _)| *n == nonce) {
            return false;
        }
        if entry.len() >= MAX_NONCES_PER_ACCOUNT {
            entry.pop_front();
        }
        entry.push_back((nonce, now.saturating_add(NONCE_TTL_SECS)));
        true
    }

    /// Drop expired nonces for every account (housekeeping, optional).
    pub fn prune(&mut self, now: u64) {
        self.seen.retain(|_, q| {
            while let Some(front) = q.front() {
                if front.1 <= now {
                    q.pop_front();
                } else {
                    break;
                }
            }
            !q.is_empty()
        });
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    pub fn nonce_count(&self, account: &str) -> usize {
        self.seen.get(account).map(|q| q.len()).unwrap_or(0)
    }
}

/// Constant-time string comparison for the legacy token.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn nonce_is_well_formed(nonce: &str) -> bool {
    let n = nonce.trim();
    (8..=64).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Authorize one pull request.
///
/// `resolve` returns the **on-chain** public key of `account`, or `None` when
/// the account is unknown / not a registered provider. The AP implements it
/// with a cached `RegisteredProviders` read; tests pass a map. A caller-supplied
/// key is never trusted.
pub fn authorize<F>(
    mode: AuthMode,
    creds: &Credentials<'_>,
    now: u64,
    expected_token: &str,
    resolve: F,
    replay: &mut ReplayCache,
) -> Result<AuthMethod, AssignAuthError>
where
    F: FnMut(&str) -> Option<[u8; 32]>,
{
    match mode {
        AuthMode::Token => check_token(creds.token, expected_token),
        AuthMode::Signature => verify_signed(creds, now, resolve, replay),
        AuthMode::Both => {
            if creds.has_signature() {
                verify_signed(creds, now, resolve, replay)
            } else {
                check_token(creds.token, expected_token)
            }
        }
    }
}

/// Legacy path. An unset expected token keeps the old permissive behaviour
/// (`empty ⇒ open`, loopback-style) so an existing deployment does not break.
fn check_token(presented: &str, expected: &str) -> Result<AuthMethod, AssignAuthError> {
    if expected.is_empty() {
        return Ok(AuthMethod::Token);
    }
    if presented.trim().is_empty() {
        return Err(AssignAuthError::MissingCredentials);
    }
    if ct_eq(expected, presented.trim()) {
        Ok(AuthMethod::Token)
    } else {
        Err(AssignAuthError::BadToken)
    }
}

fn verify_signed<F>(
    creds: &Credentials<'_>,
    now: u64,
    mut resolve: F,
    replay: &mut ReplayCache,
) -> Result<AuthMethod, AssignAuthError>
where
    F: FnMut(&str) -> Option<[u8; 32]>,
{
    if !creds.has_signature() {
        return Err(AssignAuthError::MissingCredentials);
    }
    if now.saturating_sub(creds.ts) > MAX_SKEW_SECS {
        return Err(AssignAuthError::StaleTs);
    }
    if creds.ts.saturating_sub(now) > MAX_SKEW_SECS {
        return Err(AssignAuthError::FutureTs);
    }
    if !nonce_is_well_formed(creds.nonce) {
        return Err(AssignAuthError::Malformed("bad_nonce"));
    }
    let account = creds.account.trim();
    let Some(onchain) = resolve(account) else {
        return Err(AssignAuthError::Unregistered);
    };
    let Some(decoded) = ss58_to_pubkey(account) else {
        // The header must be SS58; a bare hex key would sidestep the identity
        // check the on-chain lookup just performed.
        return Err(AssignAuthError::Unregistered);
    };
    if onchain != decoded {
        return Err(AssignAuthError::BadSignature);
    }
    let msg = canonical_message(
        account,
        creds.ts,
        creds.nonce,
        creds.method,
        creds.target,
        creds.body,
    );
    if !verify_signature(creds.sig, &msg, &onchain) {
        return Err(AssignAuthError::BadSignature);
    }
    // Authenticated: only now may this request consume replay budget.
    if !replay.check_and_record(account, creds.nonce, now) {
        return Err(AssignAuthError::ReplayedNonce);
    }
    Ok(AuthMethod::Signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::{crypto::Pair as _, sr25519};

    fn pair(uri: &str) -> sr25519::Pair {
        sr25519::Pair::from_string(uri, None).expect("dev key")
    }

    fn ss58(p: &sr25519::Pair) -> String {
        pubkey_to_ss58(&p.public().0)
    }

    fn signer(p: &sr25519::Pair) -> Sr25519Signer {
        Sr25519Signer::from_pair(p.clone())
    }

    /// One signed request, owning its strings so tests can vary one field.
    struct Signed {
        account: String,
        ts: u64,
        nonce: String,
        sig: String,
        target: String,
        body: Vec<u8>,
    }

    impl Signed {
        fn new(
            p: &sr25519::Pair,
            account: &str,
            target: &str,
            body: &[u8],
            ts: u64,
            nonce: &str,
        ) -> Self {
            let headers = sign_headers(&signer(p), account, "GET", target, body, ts, nonce);
            Self {
                account: account.to_string(),
                ts,
                nonce: nonce.to_string(),
                sig: headers[3].1.clone(),
                target: target.to_string(),
                body: body.to_vec(),
            }
        }

        fn creds(&self) -> Credentials<'_> {
            Credentials {
                account: &self.account,
                ts: self.ts,
                nonce: &self.nonce,
                sig: &self.sig,
                method: "GET",
                target: &self.target,
                body: &self.body,
                token: "",
            }
        }
    }

    fn resolve_for(p: &sr25519::Pair, registered: bool) -> impl FnMut(&str) -> Option<[u8; 32]> {
        let key = p.public().0;
        let addr = ss58(p);
        move |a: &str| {
            if registered && a == addr {
                Some(key)
            } else {
                None
            }
        }
    }

    const TARGET: &str =
        "/assignments?provider=5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY&since=0&wait=25000";

    #[test]
    fn canonical_message_is_the_documented_format() {
        let m = canonical_message("5Abc", 1_700_000_000, "00ff", "get", TARGET, b"");
        assert_eq!(
            m,
            format!(
                "gpn-assign-v1|5Abc|1700000000|00ff|GET|{TARGET}|{}",
                sha256_bytes_hex(b"")
            )
        );
        assert_eq!(
            sha256_bytes_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // Field boundaries are unambiguous: a body change changes the message.
        assert_ne!(
            m,
            canonical_message("5Abc", 1_700_000_000, "00ff", "get", TARGET, b"x")
        );
    }

    #[test]
    fn a_valid_signature_is_accepted() {
        let p = pair("//Alice");
        let account = ss58(&p);
        let c = Signed::new(&p, &account, TARGET, b"", 1_700_000_000, "a1b2c3d4e5f60718");
        let mut replay = ReplayCache::new();
        let out = authorize(
            AuthMode::Signature,
            &c.creds(),
            1_700_000_010,
            "",
            resolve_for(&p, true),
            &mut replay,
        );
        assert_eq!(out, Ok(AuthMethod::Signature));
    }

    #[test]
    fn an_unregistered_account_is_refused_as_unregistered() {
        let p = pair("//Alice");
        let account = ss58(&p);
        let c = Signed::new(&p, &account, TARGET, b"", 1_700_000_000, "a1b2c3d4e5f60718");
        let err = authorize(
            AuthMode::Signature,
            &c.creds(),
            1_700_000_000,
            "",
            resolve_for(&p, false),
            &mut ReplayCache::new(),
        )
        .unwrap_err();
        assert_eq!(err, AssignAuthError::Unregistered);
        assert_eq!(err.reason(), "unregistered");
        assert_eq!(err.status_code(), 401);
    }

    #[test]
    fn stale_and_future_timestamps_are_refused_distinctly() {
        let p = pair("//Alice");
        let account = ss58(&p);
        let now = 1_700_000_000u64;
        let stale = Signed::new(
            &p,
            &account,
            TARGET,
            b"",
            now - MAX_SKEW_SECS - 1,
            "a1b2c3d4e5f60718",
        );
        let e = authorize(
            AuthMode::Signature,
            &stale.creds(),
            now,
            "",
            resolve_for(&p, true),
            &mut ReplayCache::new(),
        )
        .unwrap_err();
        assert_eq!(e.reason(), "stale_ts");

        let future = Signed::new(
            &p,
            &account,
            TARGET,
            b"",
            now + MAX_SKEW_SECS + 1,
            "a1b2c3d4e5f60719",
        );
        let e = authorize(
            AuthMode::Signature,
            &future.creds(),
            now,
            "",
            resolve_for(&p, true),
            &mut ReplayCache::new(),
        )
        .unwrap_err();
        assert_eq!(e.reason(), "future_ts");

        // The boundary is inclusive.
        let edge = Signed::new(
            &p,
            &account,
            TARGET,
            b"",
            now - MAX_SKEW_SECS,
            "a1b2c3d4e5f6071a",
        );
        assert!(authorize(
            AuthMode::Signature,
            &edge.creds(),
            now,
            "",
            resolve_for(&p, true),
            &mut ReplayCache::new(),
        )
        .is_ok());
    }

    #[test]
    fn a_replayed_nonce_is_refused_then_accepted_after_its_window() {
        let p = pair("//Alice");
        let account = ss58(&p);
        let now = 1_700_000_000u64;
        let mut replay = ReplayCache::new();
        // The cache itself: first use true, immediate replay false, after the
        // TTL true again.
        assert!(replay.check_and_record(&account, "deadbeefdeadbeef", now));
        assert!(!replay.check_and_record(&account, "deadbeefdeadbeef", now + 1));
        assert!(replay.check_and_record(&account, "deadbeefdeadbeef", now + NONCE_TTL_SECS + 1));
        const { assert!(NONCE_TTL_SECS > MAX_SKEW_SECS) };

        // Through `authorize`, the same signed request twice: accepted, then
        // replayed_nonce.
        let c = Signed::new(&p, &account, TARGET, b"", now, "0a0b0c0d0e0f1011");
        let mut replay = ReplayCache::new();
        assert!(authorize(
            AuthMode::Signature,
            &c.creds(),
            now,
            "",
            resolve_for(&p, true),
            &mut replay
        )
        .is_ok());
        let e = authorize(
            AuthMode::Signature,
            &c.creds(),
            now + 1,
            "",
            resolve_for(&p, true),
            &mut replay,
        )
        .unwrap_err();
        assert_eq!(e.reason(), "replayed_nonce");
    }

    #[test]
    fn a_tampered_body_or_path_is_refused() {
        let p = pair("//Alice");
        let account = ss58(&p);
        let now = 1_700_000_000u64;
        let c = Signed::new(&p, &account, TARGET, b"{\"a\":1}", now, "a1b2c3d4e5f60718");
        // Same signature, different body.
        let mut tampered = c.creds();
        tampered.body = b"{\"a\":2}";
        let mut replay = ReplayCache::new();
        let e = authorize(
            AuthMode::Signature,
            &tampered,
            now,
            "",
            resolve_for(&p, true),
            &mut replay,
        )
        .unwrap_err();
        assert_eq!(e.reason(), "bad_signature");
        // A nonce consumed by a rejected request must not be burned.
        assert_eq!(replay.nonce_count(&account), 0);

        // Same signature, different provider in the query string: the account
        // binding is a separate check, but the signature already covers it.
        let other = "/assignments?provider=5Other&since=0&wait=25000";
        let mut moved = c.creds();
        moved.target = other;
        let e = authorize(
            AuthMode::Signature,
            &moved,
            now,
            "",
            resolve_for(&p, true),
            &mut ReplayCache::new(),
        )
        .unwrap_err();
        assert_eq!(e.reason(), "bad_signature");
    }

    #[test]
    fn a_signature_from_the_wrong_key_is_refused() {
        let alice = pair("//Alice");
        let bob = pair("//Bob");
        let account = ss58(&alice);
        // Signed by Bob, claims Alice.
        let headers = sign_headers(
            &signer(&bob),
            &account,
            "GET",
            TARGET,
            b"",
            1_700_000_000,
            "a1b2c3d4e5f60718",
        );
        let sig = headers[3].1.clone();
        let c = Credentials {
            account: &account,
            ts: 1_700_000_000,
            nonce: "a1b2c3d4e5f60718",
            sig: &sig,
            method: "GET",
            target: TARGET,
            body: b"",
            token: "",
        };
        let e = authorize(
            AuthMode::Signature,
            &c,
            1_700_000_000,
            "",
            resolve_for(&alice, true),
            &mut ReplayCache::new(),
        )
        .unwrap_err();
        assert_eq!(e.reason(), "bad_signature");
    }

    #[test]
    fn the_legacy_token_still_works_in_both_and_token_mode_and_not_in_signature() {
        let token = "s3cr3t";
        let unsigned = Credentials {
            token: "s3cr3t",
            method: "GET",
            target: TARGET,
            ..Default::default()
        };
        for mode in [AuthMode::Both, AuthMode::Token] {
            assert_eq!(
                authorize(mode, &unsigned, 0, token, |_| None, &mut ReplayCache::new()),
                Ok(AuthMethod::Token),
                "{mode:?} must accept the legacy token"
            );
        }
        let e = authorize(
            AuthMode::Signature,
            &unsigned,
            0,
            token,
            |_| None,
            &mut ReplayCache::new(),
        )
        .unwrap_err();
        assert_eq!(e.reason(), "missing_credentials");

        // Wrong token is refused everywhere it is accepted.
        let wrong = Credentials {
            token: "nope",
            ..unsigned
        };
        assert_eq!(
            authorize(
                AuthMode::Both,
                &wrong,
                0,
                token,
                |_| None,
                &mut ReplayCache::new()
            )
            .unwrap_err()
            .reason(),
            "bad_token"
        );

        // Legacy permissive: no expected token configured = open, unchanged.
        assert_eq!(
            authorize(
                AuthMode::Both,
                &Credentials::default(),
                0,
                "",
                |_| None,
                &mut ReplayCache::new()
            ),
            Ok(AuthMethod::Token)
        );
    }

    #[test]
    fn an_unsigned_request_with_no_token_is_refused() {
        let e = authorize(
            AuthMode::Both,
            &Credentials {
                method: "GET",
                target: TARGET,
                ..Default::default()
            },
            0,
            "configured-token",
            |_| None,
            &mut ReplayCache::new(),
        )
        .unwrap_err();
        assert_eq!(e.reason(), "missing_credentials");
    }

    #[test]
    fn mode_parsing_defaults_to_both() {
        assert_eq!(AuthMode::parse("signature"), AuthMode::Signature);
        assert_eq!(AuthMode::parse(" SIG "), AuthMode::Signature);
        assert_eq!(AuthMode::parse("token"), AuthMode::Token);
        assert_eq!(AuthMode::parse("both"), AuthMode::Both);
        assert_eq!(AuthMode::parse("nonsense"), AuthMode::Both);
        assert_eq!(AuthMode::parse(""), AuthMode::Both);
    }

    #[test]
    fn account_binding_accepts_hex_and_ss58_for_the_same_key() {
        let p = pair("//Alice");
        let s = ss58(&p);
        let hexed = format!("0x{}", hex::encode(p.public().0));
        assert_eq!(account_ss58(&hexed).as_deref(), Some(s.as_str()));
        assert_eq!(account_ss58(&s).as_deref(), Some(s.as_str()));
        assert!(same_account(&hexed, &s));
        assert!(!same_account(&hexed, &ss58(&pair("//Bob"))));
        assert!(!same_account("nonsense", &s));
    }

    #[test]
    fn provider_headers_round_trip_into_credentials() {
        // Mirrors what the provider sends and the AP reads, without HTTP.
        let p = pair("//Alice");
        let account = ss58(&p);
        let body = b"{\"caps\":{}}";
        let headers = sign_headers(
            &signer(&p),
            &account,
            "GET",
            TARGET,
            body,
            1_700_000_000,
            "0011223344556677",
        );
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .unwrap()
        };
        let c = Credentials {
            account: get(H_ACCOUNT),
            ts: get(H_TS).parse().unwrap(),
            nonce: get(H_NONCE),
            sig: get(H_SIG),
            method: "GET",
            target: TARGET,
            body,
            token: "",
        };
        assert!(c.has_signature());
        assert!(authorize(
            AuthMode::Both,
            &c,
            1_700_000_000,
            "unused",
            resolve_for(&p, true),
            &mut ReplayCache::new()
        )
        .is_ok());
    }

    #[test]
    fn a_malformed_nonce_is_refused_without_consuming_state() {
        let p = pair("//Alice");
        let account = ss58(&p);
        let headers = sign_headers(
            &signer(&p),
            &account,
            "GET",
            TARGET,
            b"",
            1_700_000_000,
            "not-hex!!",
        );
        let sig = headers[3].1.clone();
        let c = Credentials {
            account: &account,
            ts: 1_700_000_000,
            nonce: "not-hex!!",
            sig: &sig,
            method: "GET",
            target: TARGET,
            body: b"",
            token: "",
        };
        let mut replay = ReplayCache::new();
        let e = authorize(
            AuthMode::Signature,
            &c,
            1_700_000_000,
            "",
            resolve_for(&p, true),
            &mut replay,
        )
        .unwrap_err();
        assert_eq!(e.reason(), "bad_nonce");
        assert_eq!(replay.nonce_count(&account), 0);
    }

    #[test]
    fn new_nonce_is_32_hex_chars_and_unique() {
        let a = new_nonce().expect("os rng");
        let b = new_nonce().expect("os rng");
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn replay_cache_prunes_expired_entries() {
        let mut r = ReplayCache::new();
        assert!(r.check_and_record("A", "aaaaaaaaaaaaaaaa", 100));
        assert!(r.check_and_record("B", "bbbbbbbbbbbbbbbb", 100));
        r.prune(100 + NONCE_TTL_SECS + 1);
        assert!(r.is_empty());
        assert!(r.check_and_record("A", "aaaaaaaaaaaaaaaa", 100 + NONCE_TTL_SECS + 1));
    }
}
