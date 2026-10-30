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
//! 4–32 characters from `[A-Za-z0-9_-]`. Comparison is case-insensitive
//! ([`normalize`]); the value is **stored as given** so an operator can see what
//! they typed.
//!
//! # AP-side registry
//!
//! The access point records the code per provider address on FIRST sighting
//! ([`record_once`]) into `$GPN_PREFIX/referral-registry.json`:
//!
//! ```json
//! { "5Grw…": { "ref": "team-alpha", "first_seen": 1764000000 } }
//! ```
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
}

/// Outcome of [`record_once`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// New attribution written.
    New(String),
    /// The address already had a code; this one changed nothing.
    Existing(String),
    /// The candidate was malformed.
    Invalid(RefError),
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
    let address = address.trim();
    if address.is_empty() || raw.trim().is_empty() {
        return Ok(RecordOutcome::Skipped);
    }
    let code = match validate(raw) {
        Ok(c) => c,
        Err(e) => return Ok(RecordOutcome::Invalid(e)),
    };
    if is_self_referral(&code, Some(address), peer_id) {
        return Ok(RecordOutcome::SelfReferral(code));
    }
    let _lock = crate::llm::StoreLock::acquire(path)?;
    let mut map = load_registry(path);
    if let Some(have) = map.get(address) {
        return Ok(RecordOutcome::Existing(have.r#ref.clone()));
    }
    map.insert(
        address.to_string(),
        Entry {
            r#ref: code.clone(),
            first_seen: now,
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
    Ok(RecordOutcome::New(code))
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
