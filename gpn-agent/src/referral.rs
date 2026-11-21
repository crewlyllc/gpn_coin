//! Referral capture (data collection only).
//!
//! At mainnet launch a share of the genesis-unlocked treasury goes to testnet
//! participants, with a slice to whoever referred them. The amounts are not
//! decided yet, but **the attribution cannot be reconstructed later** — so the
//! referral code a node was installed with is recorded now, once, and carried on
//! every heartbeat.
//!
//! # What this module does NOT do
//!
//! No chain calls, no runtime changes, no rewards. A referral code is not
//! personal data (it is an opaque 4–32 char token, chosen by the referrer), and
//! everything here is optional: a node installed without `--ref` behaves
//! byte-for-byte as it did before this module existed.
//!
//! # Storage — one place, write-once
//!
//! The code lives in **one plain text file**: `$GPN_PREFIX/referral`
//! (default `~/.gpn/referral`), one line, the code as given. It is *not* in
//! `node.toml` because `node.toml` is regenerated/adjusted by tooling while this
//! value must never change once set, and because the installer (a shell script)
//! has to write it before the node binary ever runs.
//!
//! Write-once is enforced by the filesystem: [`write_once`] uses `create_new`,
//! which fails atomically if the file already exists. Any later `--ref` is
//! ignored (with a log line) — a node cannot rewrite its own referrer after the
//! fact, which is the whole point of collecting early.
//!
//! # Format
//!
//! Two shapes are accepted, and they are told apart by their prefix:
//!
//! * **Derived** (the default since 0.3.65, row 223) — `GPN` followed by nine
//!   base32 characters and a checksum, i.e. `GPN7K2X9QMD4T`, displayed as
//!   `GPN-7K2X-9QMD-4T`. The payload is a 45-bit fingerprint of
//!   `blake2b-256("gpn-referral-v1" || account id)` of the REFERRER's wallet, so
//!   it can be recomputed and checked by anyone holding that address — no
//!   registry needed. See [`derive_code`] / [`verify_for_address`].
//! * **Legacy** — a typed 4–32 character string from `[A-Za-z0-9_-]`, kept
//!   working forever and labelled `legacy` in the registry. Comparison is
//!   case-insensitive ([`normalize`]); the value is **stored as given** so an
//!   operator can see what they typed.
//!
//! A `GPN`-prefixed code is always parsed as derived: its checksum is checked
//! locally ([`parse_code`]), so a typo is rejected with the character that was
//! expected rather than silently stored as some other wallet's code.
//!
//! # AP-side registry
//!
//! The access point records the code per provider address on FIRST sighting
//! ([`record_once`]) into `$GPN_PREFIX/referral-registry.json`:
//!
//! ```json
//! { "5Grw…": { "ref": "GPN-7K2X-9QMD-4T", "first_seen": 1764000000,
//!              "code": "GPN7K2X9QMD4T", "derived_from": "5Fhn…", "status": "verified" } }
//! ```
//!
//! `status` is one of [`STATUS_VERIFIED`], [`STATUS_UNVERIFIED`],
//! [`STATUS_LEGACY`] or [`STATUS_INVALID_FOR_ADDRESS`] — see
//! [`record_once_checked`]. An old registry file (pre-0.3.65) loads unchanged
//! and every entry reads as `legacy`.
//!
//! First write wins — a provider that later publishes a different code does not
//! change the recorded attribution (that would make referrals retroactively
//! rewritable, exactly what write-once storage prevents on the node side).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Shortest accepted code.
pub const MIN_LEN: usize = 4;
/// Longest accepted code.
pub const MAX_LEN: usize = 32;
/// File name (under `$GPN_PREFIX`) holding the node's code.
pub const REF_FILE: &str = "referral";
/// File name (under `$GPN_PREFIX`) holding the AP's address → code registry.
pub const REGISTRY_FILE: &str = "referral-registry.json";

/// Why a candidate code was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefError {
    /// Nothing (or only whitespace) was supplied.
    Empty,
    /// Shorter than [`MIN_LEN`].
    TooShort,
    /// Longer than [`MAX_LEN`].
    TooLong,
    /// A character outside `[A-Za-z0-9_-]`.
    BadChar(char),
}

impl std::fmt::Display for RefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefError::Empty => write!(f, "empty referral code"),
            RefError::TooShort => write!(
                f,
                "referral code too short (need {MIN_LEN}–{MAX_LEN} chars)"
            ),
            RefError::TooLong => {
                write!(f, "referral code too long (need {MIN_LEN}–{MAX_LEN} chars)")
            }
            RefError::BadChar(c) => write!(
                f,
                "referral code contains {c:?} — only letters, digits, '-' and '_' are allowed"
            ),
        }
    }
}

impl std::error::Error for RefError {}

/// Validate a candidate code. Returns it **as given** (only surrounding
/// whitespace removed), or the reason it was rejected.
pub fn validate(raw: &str) -> Result<String, RefError> {
    let code = raw.trim();
    if code.is_empty() {
        return Err(RefError::Empty);
    }
    // Length is counted in characters, not bytes, so a non-ASCII code gets the
    // "bad character" error the operator can act on rather than a length one.
    let n = code.chars().count();
    if n < MIN_LEN {
        return Err(RefError::TooShort);
    }
    if n > MAX_LEN {
        return Err(RefError::TooLong);
    }
    for c in code.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(RefError::BadChar(c));
        }
    }
    Ok(code.to_string())
}

/// Canonical form used for COMPARISON only (never for storage): lowercase.
pub fn normalize(code: &str) -> String {
    code.trim().to_ascii_lowercase()
}

/// `$GPN_PREFIX`, defaulting to `~/.gpn`.
///
/// `HOME` is the Unix convention; Windows shells set `USERPROFILE` (and often
/// not `HOME`), so both are consulted — otherwise `gpn_node.exe --ref` would
/// write to `./.gpn/referral` while the PowerShell installer writes to
/// `%USERPROFILE%\.gpn\referral`, and a Windows operator would end up with two
/// different referral files.
pub fn prefix_dir() -> PathBuf {
    std::env::var_os("GPN_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".gpn")
        })
}

/// Path of the node's referral file: `$GPN_REFERRAL_FILE` if set (tests, odd
/// deployments), else `$GPN_PREFIX/referral`.
pub fn default_path() -> PathBuf {
    std::env::var_os("GPN_REFERRAL_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| prefix_dir().join(REF_FILE))
}

/// Path of the AP's address → code registry: `$GPN_REFERRAL_REGISTRY` if set,
/// else `$GPN_PREFIX/referral-registry.json`.
pub fn registry_path() -> PathBuf {
    std::env::var_os("GPN_REFERRAL_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|| prefix_dir().join(REGISTRY_FILE))
}

/// Outcome of [`write_once`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Freshly stored.
    Written(String),
    /// A value already existed; the new one was NOT stored.
    AlreadySet(String),
    /// The candidate was malformed; nothing was written.
    Invalid(RefError),
}

impl WriteOutcome {
    /// The code now on disk, if any.
    pub fn code(&self) -> Option<&str> {
        match self {
            WriteOutcome::Written(c) | WriteOutcome::AlreadySet(c) => Some(c),
            WriteOutcome::Invalid(_) => None,
        }
    }
}

/// Read the stored code (first non-empty line, trimmed). `None` when the file
/// is missing or blank — a node without a referral, which is the normal case.
pub fn read(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    let line = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.to_string())
}

/// Store `raw` **once**. If a value already exists it is returned unchanged and
/// nothing is written — this is the immutability guarantee, and it is atomic
/// (`create_new`), so two concurrent installs cannot both win.
///
/// An invalid candidate is never an error the caller must handle as fatal: it is
/// reported as [`WriteOutcome::Invalid`] so the installer can warn and continue.
pub fn write_once(path: &Path, raw: &str) -> std::io::Result<WriteOutcome> {
    let code = match validate(raw) {
        Ok(c) => c,
        Err(e) => return Ok(WriteOutcome::Invalid(e)),
    };
    store_once(path, &code)
}

/// [`write_once`] without the referral-code format rule: the same atomic
/// `create_new` write-once, for values that are not codes (the referrer's ss58
/// address, which is 48 characters and would fail [`validate`] for length).
pub fn store_once(path: &Path, value: &str) -> std::io::Result<WriteOutcome> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(WriteOutcome::Invalid(RefError::Empty));
    }
    let code = value.to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            // Trailing newline: the file stays a well-formed text file for
            // `cat`/`grep` and for a plain `$(cat …)` in a shell script.
            f.write_all(format!("{code}\n").as_bytes())?;
            f.sync_all().ok();
            Ok(WriteOutcome::Written(code))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(WriteOutcome::AlreadySet(
            // A blank/corrupt existing file is still "already set": refusing to
            // overwrite it is safer than silently accepting a new referrer.
            read(path).unwrap_or_default(),
        )),
        Err(e) => Err(e),
    }
}

/// Derive the code a node would have if it referred itself: the last 12
/// characters of its libp2p peer id, lowercased. Peer ids are base58, so the
/// result is always within the accepted charset. Used only to REJECT
/// self-referral, never to assign a code.
pub fn peer_ref_code(peer_id: &str) -> Option<String> {
    let pid = peer_id.trim();
    if pid.len() < 12 {
        return None;
    }
    let tail: String = pid.chars().skip(pid.chars().count() - 12).collect();
    if tail.is_empty() {
        return None;
    }
    Some(tail.to_ascii_lowercase())
}

/// Is `code` the node's own address or its own peer-id-derived code?
///
/// Case-insensitive. A node that refers itself would inflate the referrer's
/// attribution out of thin air, so the AP refuses it.
pub fn is_self_referral(code: &str, address: Option<&str>, peer_id: Option<&str>) -> bool {
    let want = normalize(code);
    if let Some(a) = address {
        let a = a.trim();
        if !a.is_empty() && normalize(a) == want {
            return true;
        }
    }
    if let Some(pid) = peer_id {
        if let Some(own) = peer_ref_code(pid) {
            if own == want {
                return true;
            }
        }
    }
    false
}

/// One address's recorded attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// The code, as the node published it.
    pub r#ref: String,
    /// Unix seconds when the AP first saw this address with that code.
    pub first_seen: u64,
    /// Canonical compact form, set only for a derived code that parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The ss58 address the code recomputes from, when the node published one.
    /// This is the REFERRER's address, not the attributed node's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    /// `verified` (a derived code that recomputed from `derived_from`),
    /// `unverified` (derived, but no address was published to check it),
    /// `legacy` (a typed string, uncheckable by construction), or
    /// `invalid_for_address` (a published address that does NOT produce this
    /// code — recorded as evidence, never credited).
    #[serde(default = "status_legacy")]
    pub status: String,
}

fn status_legacy() -> String {
    "legacy".to_string()
}

/// `Entry::status` for a derived code whose address was published and checked.
pub const STATUS_VERIFIED: &str = "verified";
/// `Entry::status` for a derived code published with no address to check.
pub const STATUS_UNVERIFIED: &str = "unverified";
/// `Entry::status` for a typed (pre-0.3.65) code. Never a failure: it is simply
/// not checkable, and the airdrop export says so with `derivation_verified`.
pub const STATUS_LEGACY: &str = "legacy";
/// `Entry::status` for a code that does not match the address the node itself
/// published. Replayed someone else's code: kept, never credited.
pub const STATUS_INVALID_FOR_ADDRESS: &str = "invalid_for_address";

/// Outcome of [`record_once`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// New attribution written.
    New(String),
    /// The address already had a code; this one changed nothing.
    Existing(String),
    /// The candidate was malformed.
    Invalid(RefError),
    /// A `GPN…` code that did not parse — bad checksum / length / character.
    /// Carries the operator-facing message (a checksum error names the
    /// expected character).
    MalformedCode(String),
    /// The published referrer address does not produce this code. Recorded as
    /// evidence under [`STATUS_INVALID_FOR_ADDRESS`], never credited.
    InvalidForAddress(String),
    /// The code named this node itself.
    SelfReferral(String),
    /// Nothing to record (empty address or code).
    Skipped,
}

/// Load the registry (`{}` when absent or unreadable — never an error: snapshot
/// tooling must work on a machine that never ran an AP).
pub fn load_registry(path: &Path) -> BTreeMap<String, Entry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record `raw` for `address` — **first write wins**, atomically under the
/// cross-process lock used by the rest of the shared `~/.gpn` state.
///
/// A code is only stored if it validates and does not name the node itself
/// (`peer_id` is optional; when absent only the address check applies).
pub fn record_once(
    path: &Path,
    address: &str,
    raw: &str,
    peer_id: Option<&str>,
    now: u64,
) -> std::io::Result<RecordOutcome> {
    // No referrer address supplied: the legacy behaviour, and the only caller
    // shape that existed before row 223. `InvalidForAddress` is unreachable
    // from here by construction.
    record_once_checked(path, address, raw, None, peer_id, now)
}

/// [`record_once`] with the REFERRER's address, so a derived code can be
/// checked instead of merely stored.
///
/// `ref_address` is the ss58 address the presenting node says the code was
/// derived from (it publishes `ref_address` beside `ref`). Outcomes:
///
/// * derived code + matching address → [`STATUS_VERIFIED`], `derived_from` set
/// * derived code + mismatched address → [`STATUS_INVALID_FOR_ADDRESS`], the
///   entry is WRITTEN (it is evidence of a replayed code) and
///   [`RecordOutcome::InvalidForAddress`] is returned so the caller can log it
/// * derived code + no address → [`STATUS_UNVERIFIED`] (still attributed: a
///   node that upgrades before its installer learns the flag must not lose its
///   referral)
/// * a non-`GPN` string → [`STATUS_LEGACY`], unchanged from pre-0.3.65
pub fn record_once_checked(
    path: &Path,
    address: &str,
    raw: &str,
    ref_address: Option<&str>,
    peer_id: Option<&str>,
    now: u64,
) -> std::io::Result<RecordOutcome> {
    let address = address.trim();
    if address.is_empty() || raw.trim().is_empty() {
        return Ok(RecordOutcome::Skipped);
    }

    // Classify the string first: a `GPN…` code is checked, anything else is a
    // legacy typed code and follows the old validate() path.
    let derived = match parse_code(raw) {
        Ok(code) => Some(code),
        Err(CodeError::NotDerived) => None,
        Err(e) => return Ok(RecordOutcome::MalformedCode(e.to_string())),
    };

    let (stored, status, derived_from) = match derived {
        Some(ref code) => {
            // Self-referral: the code computed from THIS node's own address.
            if let Ok(own) = derive_code(address) {
                if &own == code {
                    return Ok(RecordOutcome::SelfReferral(display_form(&code)));
                }
            }
            let check = ref_address.map(str::trim).filter(|a| !a.is_empty());
            match check {
                Some(a) => match derive_code(a) {
                    Ok(expect) if &expect == code => {
                        (display_form(&code), STATUS_VERIFIED, Some(a.to_string()))
                    }
                    Ok(_) | Err(_) => (
                        display_form(&code),
                        STATUS_INVALID_FOR_ADDRESS,
                        Some(a.to_string()),
                    ),
                },
                None => (display_form(&code), STATUS_UNVERIFIED, None),
            }
        }
        None => match validate(raw) {
            Ok(c) => (c, STATUS_LEGACY, None),
            Err(e) => return Ok(RecordOutcome::Invalid(e)),
        },
    };

    if status == STATUS_LEGACY && is_self_referral(&stored, Some(address), peer_id) {
        return Ok(RecordOutcome::SelfReferral(stored));
    }

    let _lock = crate::llm::StoreLock::acquire(path)?;
    let mut map = load_registry(path);
    if let Some(have) = map.get(address) {
        return Ok(RecordOutcome::Existing(have.r#ref.clone()));
    }
    let canonical = derived.clone();
    map.insert(
        address.to_string(),
        Entry {
            r#ref: stored.clone(),
            first_seen: now,
            code: canonical,
            derived_from,
            status: status.to_string(),
        },
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Stage then rename: a truncated registry would silently lose every
    // attribution collected so far, and this file is the only copy.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&map)?)?;
    std::fs::rename(&tmp, path)?;
    if status == STATUS_INVALID_FOR_ADDRESS {
        // Written above: the entry is evidence. Report it so the AP log names
        // the address rather than silently treating it as attributed.
        return Ok(RecordOutcome::InvalidForAddress(stored));
    }
    Ok(RecordOutcome::New(stored))
}

// ── derived codes (row 223, design docs/design-referral-codes.md) ───────────

/// Base32 alphabet with `I`, `O`, `0` and `1` removed, so a code can be read
/// aloud and typed back from a screenshot. 32 symbols, so five bits each.
pub const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// Every derived code starts with this.
pub const CODE_PREFIX: &str = "GPN";
/// Payload characters after the prefix: 9 × 5 = 45 bits, ~2⁻⁴⁵ collisions.
pub const CODE_PAYLOAD_LEN: usize = 9;
/// Full compact length: `GPN` + payload + checksum character.
pub const CODE_LEN: usize = 3 + CODE_PAYLOAD_LEN + 1;
/// Domain string for the derivation digest. Versioned so a future scheme can
/// coexist instead of invalidating every code in the wild.
pub const DERIVATION_DOMAIN: &[u8] = b"gpn-referral-v1";
const CHECKSUM_DOMAIN: &[u8] = b"gpn-referral-check-v1";

/// Why a string was not a usable derived code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    /// No `GPN` prefix: a legacy typed code, not an error.
    NotDerived,
    /// Prefix present, wrong length.
    BadLength(usize),
    /// Prefix present, a character outside the alphabet.
    BadChar(char),
    /// A single character in the checksum position disagreed with the payload.
    Checksum { expected: char, found: char },
    /// The address it claims cannot be turned into a code at all.
    NotAnAccount(String),
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeError::NotDerived => write!(f, "not a GPN referral code"),
            CodeError::BadLength(n) => write!(
                f,
                "referral code is {n} characters; a GPN code is exactly {CODE_LEN} (dashes optional)"
            ),
            CodeError::BadChar(c) => write!(
                f,
                "referral code contains {c:?}; the alphabet is {}",
                String::from_utf8_lossy(ALPHABET)
            ),
            CodeError::Checksum { expected, found } => write!(
                f,
                "referral code checksum is '{found}' but this code's checksum is '{expected}' — check for a typo"
            ),
            CodeError::NotAnAccount(e) => write!(f, "not an ss58 account: {e}"),
        }
    }
}

impl std::error::Error for CodeError {}

fn alphabet_index(c: char) -> Option<u8> {
    ALPHABET
        .iter()
        .position(|&b| b as char == c)
        .map(|i| i as u8)
}

/// MSB-first base32 of `bytes`, truncated to `chars` symbols.
fn base32_prefix(bytes: &[u8], chars: usize) -> String {
    let mut out = String::with_capacity(chars);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 && out.len() < chars {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 0x1f) as usize] as char);
        }
        if out.len() >= chars {
            break;
        }
    }
    out
}

/// Payload (the nine characters after `GPN`) of a derived code.
pub fn payload_of(ss58_address: &str) -> Result<String, CodeError> {
    let pubkey = crate::firstrun::ss58_to_public(ss58_address).map_err(CodeError::NotAnAccount)?;
    let mut buf = Vec::with_capacity(DERIVATION_DOMAIN.len() + 32);
    buf.extend_from_slice(DERIVATION_DOMAIN);
    buf.extend_from_slice(&pubkey);
    let digest = sp_core::blake2_256(&buf);
    Ok(base32_prefix(&digest, CODE_PAYLOAD_LEN))
}

/// The checksum character over the nine payload characters.
///
/// `blake2b-256("gpn-referral-check-v1" || payload)[0] mod 32`, so it is
/// reproducible offline and a substitution anywhere in the payload always
/// changes it.
pub fn checksum_of(payload: &str) -> char {
    let mut buf = Vec::with_capacity(CHECKSUM_DOMAIN.len() + payload.len());
    buf.extend_from_slice(CHECKSUM_DOMAIN);
    buf.extend_from_slice(payload.as_bytes());
    let d = sp_core::blake2_256(&buf);
    ALPHABET[(d[0] & 0x1f) as usize] as char
}

/// The compact code for an address: `GPN` + 9 payload + checksum = 13 chars.
pub fn derive_code(ss58_address: &str) -> Result<String, CodeError> {
    let payload = payload_of(ss58_address)?;
    let mut code = String::with_capacity(CODE_LEN);
    code.push_str(CODE_PREFIX);
    code.push_str(&payload);
    code.push(checksum_of(&payload));
    Ok(code)
}

/// Display form: `GPN-7K2X-9QMD-4T` (groups of four, from the payload).
///
/// `parse_code` accepts both forms, so this is presentation only.
pub fn display_form(compact: &str) -> String {
    let body = compact
        .trim()
        .to_ascii_uppercase()
        .trim_start_matches(CODE_PREFIX)
        .replace('-', "");
    if body.len() != CODE_PAYLOAD_LEN + 1 {
        return compact.to_string();
    }
    let mut groups: Vec<String> = body
        .chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect())
        .collect();
    groups.insert(0, CODE_PREFIX.to_string());
    groups.join("-")
}

/// Parse either form and verify the checksum, returning the compact code.
///
/// Returns [`CodeError::NotDerived`] when the string has no `GPN` prefix — that
/// is a legacy typed code, and it is the caller's job to fall back, not an
/// error to show an operator.
pub fn parse_code(raw: &str) -> Result<String, CodeError> {
    let up = raw.trim().to_ascii_uppercase().replace('-', "");
    if !up.starts_with(CODE_PREFIX) {
        return Err(CodeError::NotDerived);
    }
    let body = &up[CODE_PREFIX.len()..];
    let n = body.chars().count();
    if n != CODE_PAYLOAD_LEN + 1 {
        return Err(CodeError::BadLength(body.len() + CODE_PREFIX.len()));
    }
    for c in body.chars() {
        if alphabet_index(c).is_none() {
            return Err(CodeError::BadChar(c));
        }
    }
    let payload: String = body.chars().take(CODE_PAYLOAD_LEN).collect();
    let found = body.chars().last().unwrap();
    let expected = checksum_of(&payload);
    if found != expected {
        return Err(CodeError::Checksum { expected, found });
    }
    Ok(format!("{CODE_PREFIX}{body}"))
}

/// Recompute the code from `ss58_address` and compare it with `code`.
///
/// This is the check that makes a code verifiable without a registry: the AP
/// can confirm that a presented code really belongs to the address the node
/// published.
pub fn verify_for_address(code: &str, ss58_address: &str) -> Result<String, CodeError> {
    let compact = parse_code(code)?;
    let expect = derive_code(ss58_address)?;
    if compact != expect {
        return Err(CodeError::NotAnAccount(format!(
            "code {code} does not derive from {ss58_address} (expected {})",
            display_form(&expect)
        )));
    }
    Ok(compact)
}

/// `$GPN_PREFIX/referral-address` — the REFERRER's ss58 address, written beside
/// the code when the installer supplied one. Absent for legacy installs.
pub const REF_ADDRESS_FILE: &str = "referral-address";

/// Path of the referrer-address file (tests override with a sibling temp dir).
pub fn address_path(default_code_path: &Path) -> PathBuf {
    default_code_path.with_file_name(REF_ADDRESS_FILE)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn td(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("gpn-ref-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ── format ──────────────────────────────────────────────────────────────

    #[test]
    fn valid_codes_are_accepted_including_the_boundaries() {
        assert_eq!(validate("abcd").unwrap(), "abcd");
        assert_eq!(validate("A-Z_9").unwrap(), "A-Z_9");
        assert_eq!(validate(&"x".repeat(MAX_LEN)).unwrap(), "x".repeat(MAX_LEN));
        // Surrounding whitespace is not part of the code.
        assert_eq!(validate("  team-alpha  ").unwrap(), "team-alpha");
    }

    #[test]
    fn invalid_codes_are_refused_with_a_reason() {
        assert_eq!(validate(""), Err(RefError::Empty));
        assert_eq!(validate("   "), Err(RefError::Empty));
        assert_eq!(validate("abc"), Err(RefError::TooShort));
        assert_eq!(validate(&"x".repeat(MAX_LEN + 1)), Err(RefError::TooLong));
        assert_eq!(validate("abc def"), Err(RefError::BadChar(' ')));
        assert_eq!(validate("abc!def"), Err(RefError::BadChar('!')));
        // Non-ASCII is a bad character, not a length error.
        assert_eq!(validate("ab€d"), Err(RefError::BadChar('€')));
        // Display is human-readable (the installer prints it).
        assert!(RefError::TooLong.to_string().contains("32"));
    }

    #[test]
    fn normalization_is_lowercase_and_does_not_touch_storage() {
        let dir = td("norm");
        let p = dir.join(REF_FILE);
        let out = write_once(&p, "Team-Alpha").unwrap();
        assert_eq!(out, WriteOutcome::Written("Team-Alpha".into()));
        // Stored as given…
        assert_eq!(read(&p).as_deref(), Some("Team-Alpha"));
        // …compared lowercased.
        assert_eq!(normalize(&read(&p).unwrap()), "team-alpha");
    }

    // ── write-once ──────────────────────────────────────────────────────────

    #[test]
    fn write_once_stores_the_first_value_and_ignores_every_later_one() {
        let dir = td("once");
        let p = dir.join(REF_FILE);
        assert_eq!(
            write_once(&p, "first-ref").unwrap(),
            WriteOutcome::Written("first-ref".into())
        );
        assert_eq!(
            write_once(&p, "second-ref").unwrap(),
            WriteOutcome::AlreadySet("first-ref".into())
        );
        // Even a different case cannot rewrite it.
        assert_eq!(
            write_once(&p, "FIRST-REF").unwrap(),
            WriteOutcome::AlreadySet("first-ref".into())
        );
        assert_eq!(read(&p).as_deref(), Some("first-ref"));
    }

    #[test]
    fn an_invalid_value_never_creates_the_file() {
        let dir = td("invalid");
        let p = dir.join(REF_FILE);
        assert_eq!(
            write_once(&p, "no").unwrap(),
            WriteOutcome::Invalid(RefError::TooShort)
        );
        assert!(!p.exists());
        // A later valid value still wins: a rejected code does not burn the slot.
        assert_eq!(
            write_once(&p, "good-ref").unwrap(),
            WriteOutcome::Written("good-ref".into())
        );
        assert_eq!(read(&p).as_deref(), Some("good-ref"));
    }

    #[test]
    fn write_once_creates_the_prefix_dir_and_survives_a_blank_existing_file() {
        let dir = td("blank").join("nested").join(".gpn");
        let p = dir.join(REF_FILE);
        assert_eq!(
            write_once(&p, "nested-ref").unwrap(),
            WriteOutcome::Written("nested-ref".into())
        );
        // A hand-truncated file must still not be rewritten (write-once wins
        // over convenience — re-referring is exactly what this prevents).
        std::fs::write(&p, "").unwrap();
        assert_eq!(read(&p), None);
        assert_eq!(
            write_once(&p, "other-ref").unwrap(),
            WriteOutcome::AlreadySet(String::new())
        );
        assert_eq!(read(&p), None);
    }

    #[test]
    fn no_file_means_no_referral() {
        let dir = td("absent");
        assert_eq!(read(&dir.join(REF_FILE)), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── self-referral ───────────────────────────────────────────────────────

    #[test]
    fn self_referral_is_detected_by_address_or_peer_id_tail() {
        let addr = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        let own = peer_ref_code(pid).unwrap();
        assert_eq!(own.len(), 12);
        assert!(own.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(is_self_referral(&own, Some("5Other"), Some(pid)));
        assert!(is_self_referral(&own.to_ascii_uppercase(), None, Some(pid)));
        assert!(is_self_referral(addr, Some(addr), None));
        assert!(!is_self_referral("team-alpha", Some(addr), Some(pid)));
        // Too short to derive: no match rather than a panic.
        assert_eq!(peer_ref_code("12D3Koo"), None);
    }

    // ── AP registry ─────────────────────────────────────────────────────────

    #[test]
    fn registry_records_first_value_and_never_overwrites() {
        let dir = td("registry");
        let p = dir.join(REGISTRY_FILE);
        let addr = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
        assert_eq!(
            record_once(&p, addr, "team-alpha", None, 1000).unwrap(),
            RecordOutcome::New("team-alpha".into())
        );
        assert_eq!(
            record_once(&p, addr, "other-team", None, 2000).unwrap(),
            RecordOutcome::Existing("team-alpha".into())
        );
        let reg = load_registry(&p);
        assert_eq!(reg.len(), 1);
        let e = reg.get(addr).unwrap();
        assert_eq!(e.r#ref, "team-alpha");
        assert_eq!(e.first_seen, 1000, "first_seen is not rewritten either");
    }

    #[test]
    fn registry_rejects_self_referral_and_bad_codes_without_writing() {
        let dir = td("registry-reject");
        let p = dir.join(REGISTRY_FILE);
        let addr = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        let own = peer_ref_code(pid).unwrap();
        assert_eq!(
            record_once(&p, addr, &own, Some(pid), 1).unwrap(),
            RecordOutcome::SelfReferral(own)
        );
        assert_eq!(
            record_once(&p, addr, "no", None, 1).unwrap(),
            RecordOutcome::Invalid(RefError::TooShort)
        );
        assert_eq!(
            record_once(&p, "", "team-alpha", None, 1).unwrap(),
            RecordOutcome::Skipped
        );
        assert!(load_registry(&p).is_empty());
        // The same address can still be attributed properly afterwards.
        assert_eq!(
            record_once(&p, addr, "team-alpha", None, 5).unwrap(),
            RecordOutcome::New("team-alpha".into())
        );
    }

    // ── derived codes (row 223) ──────────────────────────────────────────────

    const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
    const BOB: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";

    #[test]
    fn the_same_wallet_always_reproduces_its_code_and_different_wallets_differ() {
        let a1 = derive_code(ALICE).unwrap();
        let a2 = derive_code(ALICE).unwrap();
        let b = derive_code(BOB).unwrap();
        assert_eq!(a1, a2, "derivation must be deterministic across reinstalls");
        assert_ne!(a1, b, "two wallets must not share a code");
        assert_eq!(a1.len(), CODE_LEN);
        assert!(a1.starts_with("GPN"));
        assert_eq!(parse_code(&a1).unwrap(), a1);
        assert_eq!(payload_of(ALICE).unwrap(), payload_of(ALICE).unwrap());
    }

    #[test]
    fn display_form_round_trips_through_the_parser() {
        let code = derive_code(ALICE).unwrap();
        let shown = display_form(&code);
        assert!(shown.starts_with("GPN-"), "{shown}");
        assert_eq!(shown.matches('-').count(), 3, "{shown}");
        let body: String = shown.chars().filter(|c| *c != '-').collect();
        assert_eq!(body, code);
        assert_eq!(parse_code(&shown).unwrap(), code);
        assert_eq!(parse_code(&shown.to_ascii_lowercase()).unwrap(), code);
    }

    #[test]
    fn a_checksum_typo_is_rejected_and_names_the_expected_character() {
        let code = derive_code(ALICE).unwrap();
        let mut chars: Vec<char> = code.chars().collect();
        let last = *chars.last().unwrap();
        let wrong = if last == '2' { '3' } else { '2' };
        *chars.last_mut().unwrap() = wrong;
        let typo: String = chars.into_iter().collect();
        match parse_code(&typo) {
            Err(CodeError::Checksum { expected, found }) => {
                assert_eq!(found, wrong);
                assert_eq!(expected, last);
                let msg = CodeError::Checksum { expected, found }.to_string();
                assert!(msg.contains(&expected.to_string()), "{msg}");
                assert!(msg.contains("checksum"), "{msg}");
            }
            other => panic!("expected a checksum error, got {other:?}"),
        }
    }

    #[test]
    fn a_non_gpn_string_is_not_a_derivation_error_it_is_legacy() {
        assert_eq!(parse_code("team-alpha"), Err(CodeError::NotDerived));
        assert_eq!(parse_code("GPN-SHORT"), Err(CodeError::BadLength(8)));
    }

    #[test]
    fn verify_for_address_accepts_the_owner_and_refuses_anyone_else() {
        let code = derive_code(ALICE).unwrap();
        assert_eq!(verify_for_address(&code, ALICE).unwrap(), code);
        assert!(verify_for_address(&display_form(&code), ALICE).is_ok());
        assert!(verify_for_address(&code, BOB).is_err());
        assert!(derive_code("not-an-address").is_err());
    }

    #[test]
    fn ap_records_verified_invalid_for_address_unverified_and_legacy_apart() {
        let dir = td("derived-registry");
        let p = dir.join(REGISTRY_FILE);
        let code = derive_code(ALICE).unwrap();

        // Alice's code presented by Bob's node, with Alice's address: verified.
        assert_eq!(
            record_once_checked(&p, BOB, &code, Some(ALICE), None, 10).unwrap(),
            RecordOutcome::New(display_form(&code))
        );
        let reg = load_registry(&p);
        assert_eq!(reg[BOB].status, STATUS_VERIFIED);
        assert_eq!(reg[BOB].derived_from.as_deref(), Some(ALICE));
        assert_eq!(reg[BOB].code.as_deref(), Some(code.as_str()));

        // Same code, replayed with a DIFFERENT address: written as evidence,
        // never credited as verified.
        assert_eq!(
            record_once_checked(&p, "5Replay", &code, Some(BOB), None, 11).unwrap(),
            RecordOutcome::InvalidForAddress(display_form(&code))
        );
        let reg = load_registry(&p);
        assert_eq!(reg["5Replay"].status, STATUS_INVALID_FOR_ADDRESS);
        assert!(reg.contains_key("5Replay"), "must not be silently dropped");

        // A derived code with no address published is still attributed, but
        // labelled as not proven.
        assert_eq!(
            record_once_checked(&p, "5NoAddr", &code, None, None, 12).unwrap(),
            RecordOutcome::New(display_form(&code))
        );
        assert_eq!(load_registry(&p)["5NoAddr"].status, STATUS_UNVERIFIED);

        // A legacy typed code keeps working, labelled legacy.
        assert_eq!(
            record_once_checked(&p, "5Legacy", "team-alpha", None, None, 13).unwrap(),
            RecordOutcome::New("team-alpha".into())
        );
        let reg = load_registry(&p);
        assert_eq!(reg["5Legacy"].status, STATUS_LEGACY);
        assert!(reg["5Legacy"].code.is_none());
    }

    #[test]
    fn a_registry_written_before_row_223_still_loads_as_legacy() {
        let dir = td("old-registry");
        let p = dir.join(REGISTRY_FILE);
        std::fs::write(
            &p,
            r#"{"5OldAddr":{"ref":"team-alpha","first_seen":1700000000}}"#,
        )
        .unwrap();
        let reg = load_registry(&p);
        let e = reg.get("5OldAddr").unwrap();
        assert_eq!(e.status, STATUS_LEGACY);
        assert_eq!(e.r#ref, "team-alpha");
        assert_eq!(e.first_seen, 1700000000);
    }

    #[test]
    fn registry_is_shared_across_addresses_and_survives_a_reload() {
        let dir = td("registry-multi");
        let p = dir.join(REGISTRY_FILE);
        record_once(&p, "5AAA", "team-alpha", None, 10).unwrap();
        record_once(&p, "5BBB", "team-beta", None, 11).unwrap();
        record_once(&p, "5CCC", "team-alpha", None, 12).unwrap();
        let reg = load_registry(&p);
        assert_eq!(reg.len(), 3);
        assert_eq!(reg["5AAA"].r#ref, "team-alpha");
        assert_eq!(reg["5BBB"].r#ref, "team-beta");
    }
}
