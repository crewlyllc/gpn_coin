//! Abuse & safety guardrails (item #22) — build-order **step 4: report path + ban lists**.
//!
//! Steps 1–3 shipped the Acceptable Use Policy, the access-point input
//! moderation and the provider-side policy hooks (`crate::moderation`,
//! `crate::provider_policy`). This module is the *supporting machinery* from
//! `design-abuse-safety-guardrails.md` §3:
//!
//! - a **report / flag path** — anyone (user, provider, AP, third party) can
//!   report an account / provider / job; reports are appended to a JSONL log
//!   that AP + provider ops read; and
//! - a **denylist** — accounts / providers / IPs an operator can *enforce* at
//!   the access point (refuse service) and at the provider (refuse to run a
//!   banned requestor's job). The design's "account + provider bans" + "per-AP
//!   denylists".
//!
//! Everything here is **off-chain and dependency-free** — no genesis, no runtime,
//! no `set_code`. A self-hosted node's own GPU is out of reach (design §0), so
//! this is an operator-owned policy layer, not a network-wide one.
//!
//! **Privacy (design §5):** a report records the *target* + a category — never
//! the prompt or response text. Ban lists hold identifiers only.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The kinds of target a report / ban may name.
pub const KINDS: &[&str] = &["account", "provider", "job", "ip"];

/// Canonicalise an account (or provider address) for comparison. An `0x`-prefixed
/// 32-byte hex chain key becomes its ss58 form (so a ban file may hold either
/// form and still match whatever the caller or the chain hands us); anything else
/// is returned trimmed.
pub fn canonical_account(s: &str) -> String {
    let t = s.trim();
    if t.len() == 66 && (t.starts_with("0x") || t.starts_with("0X")) {
        if let Ok(bytes) = hex::decode(&t[2..]) {
            if bytes.len() == 32 {
                use sp_core::crypto::Ss58Codec;
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                return sp_core::sr25519::Public::from_raw(arr).to_ss58check();
            }
        }
    }
    t.to_string()
}

/// An operator-owned denylist. Persisted as a small JSON file; also extendable
/// from the environment so a deploy can seed bans without editing files.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Denylist {
    /// Banned account ids (ss58 or `0x` hex; normalised on add).
    #[serde(default)]
    pub accounts: Vec<String>,
    /// Banned provider addresses (same form as accounts).
    #[serde(default)]
    pub providers: Vec<String>,
    /// Banned client IPs (exact match).
    #[serde(default)]
    pub ips: Vec<String>,
}

impl Denylist {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when nothing is banned.
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.providers.is_empty() && self.ips.is_empty()
    }

    /// The default denylist file: `GPN_DENY_FILE` or `~/.gpn/denylist.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("GPN_DENY_FILE") {
            if !p.trim().is_empty() {
                return PathBuf::from(p);
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(format!("{home}/.gpn/denylist.json"))
    }

    /// Load from `path`. A missing or malformed file yields an empty list (a
    /// ban list must never crash the AP/provider — fail *open* to "no bans",
    /// which the caller can surface as a warning).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Denylist>(&s).ok())
            .unwrap_or_default()
    }

    /// Load the default file **and** merge `GPN_DENY_ACCOUNTS` /
    /// `GPN_DENY_PROVIDERS` / `GPN_DENY_IPS` (comma-separated) on top.
    pub fn from_env() -> Self {
        let mut d = Self::load(&Self::default_path());
        d.merge_env();
        d
    }

    /// Merge the `GPN_DENY_*` environment variables into this list.
    pub fn merge_env(&mut self) {
        for v in parse_env_list(std::env::var("GPN_DENY_ACCOUNTS").ok()) {
            self.add_account(&v);
        }
        for v in parse_env_list(std::env::var("GPN_DENY_PROVIDERS").ok()) {
            self.add_provider(&v);
        }
        for v in parse_env_list(std::env::var("GPN_DENY_IPS").ok()) {
            self.add_ip(&v);
        }
    }

    /// Add an account to the denylist. Returns true if it was newly added.
    pub fn add_account(&mut self, a: &str) -> bool {
        push_unique(&mut self.accounts, canonical_account(a))
    }

    /// Add a provider to the denylist. Returns true if it was newly added.
    pub fn add_provider(&mut self, p: &str) -> bool {
        push_unique(&mut self.providers, canonical_account(p))
    }

    /// Add an IP to the denylist. Returns true if it was newly added.
    pub fn add_ip(&mut self, ip: &str) -> bool {
        push_unique(&mut self.ips, ip.trim().to_string())
    }

    /// Remove an account / provider / IP. Returns true if something was removed.
    pub fn remove(&mut self, kind: &str, value: &str) -> bool {
        let (v, vv) = match kind {
            "account" => (canonical_account(value), &mut self.accounts),
            "provider" => (canonical_account(value), &mut self.providers),
            "ip" => (value.trim().to_string(), &mut self.ips),
            _ => return false,
        };
        let before = vv.len();
        vv.retain(|x| x != &v);
        vv.len() != before
    }

    /// True when `a` (any form) is a banned account.
    pub fn is_account_banned(&self, a: &str) -> bool {
        let c = canonical_account(a);
        !c.is_empty() && self.accounts.iter().any(|x| x == &c)
    }

    /// True when `p` (any form) is a banned provider.
    pub fn is_provider_banned(&self, p: &str) -> bool {
        let c = canonical_account(p);
        !c.is_empty() && self.providers.iter().any(|x| x == &c)
    }

    /// True when `ip` is a banned client IP.
    pub fn is_ip_banned(&self, ip: &str) -> bool {
        let c = ip.trim();
        !c.is_empty() && self.ips.iter().any(|x| x == c)
    }

    /// Persist to `path` (creates the parent directory). Compact JSON.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let s = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(path, s)
    }
}

/// Push `v` if non-empty and not already present. Returns whether it was added.
fn push_unique(v: &mut Vec<String>, val: String) -> bool {
    if val.is_empty() || v.iter().any(|x| x == &val) {
        return false;
    }
    v.push(val);
    true
}

/// Split a comma-separated env list into trimmed, non-empty entries.
fn parse_env_list(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A single abuse report. Content-free by construction (design §5).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Report {
    /// Opaque id for follow-up.
    pub report_id: String,
    /// Unix seconds.
    pub ts: u64,
    /// What kind of target: `account` | `provider` | `job` | `ip`.
    pub kind: String,
    /// The target (address / status URL / job id / IP).
    pub target: String,
    /// Optional prohibited-use category (e.g. `csam`, `weapons_mass_casualty`).
    #[serde(default)]
    pub category: Option<String>,
    /// Optional free-text reason (operator-supplied; never prompt content).
    #[serde(default)]
    pub reason: Option<String>,
    /// Who filed it (reporter IP or `loopback`).
    #[serde(default)]
    pub source: Option<String>,
}

impl Report {
    /// Validate the report's shape. `Ok(())` when it may be recorded.
    pub fn validate(&self) -> Result<(), String> {
        if self.target.trim().is_empty() {
            return Err("report target is required".into());
        }
        if !KINDS.contains(&self.kind.as_str()) {
            return Err(format!(
                "unknown report kind '{}' (expected one of {})",
                self.kind,
                KINDS.join(", ")
            ));
        }
        Ok(())
    }
}

/// Unix seconds.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A short opaque report id (8 random bytes, hex; time-based fallback).
pub fn new_report_id() -> String {
    use std::io::Read;
    let mut buf = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            return hex::encode(buf);
        }
    }
    format!("{:016x}", now_secs())
}

/// The default reports file: `GPN_REPORTS_FILE` or `~/.gpn/reports.jsonl`.
pub fn reports_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_REPORTS_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(format!("{home}/.gpn/reports.jsonl"))
}

/// Append one report as a JSON line (creates the parent directory).
pub fn append_report(path: &Path, r: &Report) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let line = serde_json::to_string(r).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Read up to `limit` most-recent reports, newest first. Malformed lines are
/// skipped. A missing file yields an empty list.
pub fn read_reports(path: &Path, limit: usize) -> Vec<Report> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<Report> = s
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Report>(l).ok())
        .collect();
    out.reverse();
    out.truncate(limit);
    out
}

/// Read **all** reports (no truncation), oldest first — for the transparency
/// summary, which needs the full population and the time range.
pub fn read_all_reports(path: &Path) -> Vec<Report> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Report>(l).ok())
        .collect()
}

/// A transparency summary of abuse-handling activity: report counts by kind and
/// by category, the time range, and the number of *active* bans. Content-free
/// (design §5) — it counts metadata, never prompts. Pure, so it is unit-tested
/// and the same function backs the `gpn_abuse_report` operator CLI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// Total reports on file.
    pub total_reports: usize,
    /// Reports by `kind` (account/provider/job/ip), sorted by count desc.
    pub by_kind: Vec<(String, usize)>,
    /// Reports by `category`, sorted by count desc (uncategorised grouped as `""`).
    pub by_category: Vec<(String, usize)>,
    /// Oldest report timestamp (unix secs), 0 when none.
    pub first_ts: u64,
    /// Newest report timestamp (unix secs), 0 when none.
    pub last_ts: u64,
    /// Active banned accounts.
    pub banned_accounts: usize,
    /// Active banned providers.
    pub banned_providers: usize,
    /// Active banned IPs.
    pub banned_ips: usize,
}

/// Build a [`Summary`] from the reports and the current denylist. Ties in the
/// counts are broken alphabetically so the output is deterministic.
pub fn summarize(reports: &[Report], denylist: &Denylist) -> Summary {
    use std::collections::BTreeMap;
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    let mut first_ts = 0u64;
    let mut last_ts = 0u64;
    for r in reports {
        *by_kind.entry(r.kind.clone()).or_insert(0) += 1;
        *by_category
            .entry(r.category.clone().unwrap_or_default())
            .or_insert(0) += 1;
        if first_ts == 0 || r.ts < first_ts {
            first_ts = r.ts;
        }
        if r.ts > last_ts {
            last_ts = r.ts;
        }
    }
    let rank = |m: BTreeMap<String, usize>| {
        let mut v: Vec<(String, usize)> = m.into_iter().collect();
        // Highest count first; alphabetical tiebreak (already sorted by key).
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    };
    Summary {
        total_reports: reports.len(),
        by_kind: rank(by_kind),
        by_category: rank(by_category),
        first_ts,
        last_ts,
        banned_accounts: denylist.accounts.len(),
        banned_providers: denylist.providers.len(),
        banned_ips: denylist.ips.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "gpn-abuse-test-{}-{}.json",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    fn canonical_account_converts_hex_to_ss58_and_passes_through() {
        let hex = "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";
        let ss58 = canonical_account(hex);
        assert!(ss58.starts_with('5'), "expected ss58, got {ss58}");
        // Idempotent on an ss58 value.
        assert_eq!(canonical_account(&ss58), ss58);
        // Non-address text is just trimmed.
        assert_eq!(canonical_account("  hello  "), "hello");
    }

    #[test]
    fn denylist_add_dedup_and_empty_is_rejected() {
        let mut d = Denylist::new();
        assert!(d.is_empty());
        assert!(d.add_account("alice"));
        assert!(!d.add_account("alice"), "duplicate must not re-add");
        assert!(!d.add_account("   "), "blank entry must not be added");
        assert_eq!(d.accounts.len(), 1);
    }

    #[test]
    fn denylist_membership_hex_and_ss58_match() {
        let mut d = Denylist::new();
        let hex = "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";
        d.add_account(hex);
        // The ss58 form of the same key must match the hex ban.
        assert!(d.is_account_banned(&canonical_account(hex)));
        assert!(d.is_account_banned(hex));
        assert!(!d.is_account_banned("5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty"));
    }

    #[test]
    fn denylist_provider_and_ip_and_removal() {
        let mut d = Denylist::new();
        d.add_provider("5EnqSp5Q");
        d.add_ip("203.0.113.7");
        assert!(d.is_provider_banned("5EnqSp5Q"));
        assert!(d.is_ip_banned("203.0.113.7"));
        assert!(!d.is_ip_banned("198.51.100.1"));
        assert!(d.remove("ip", "203.0.113.7"));
        assert!(!d.is_ip_banned("203.0.113.7"));
        assert!(!d.remove("ip", "203.0.113.7"), "second removal is a no-op");
        assert!(!d.is_empty());
    }

    #[test]
    fn denylist_roundtrips_through_a_file() {
        let path = tmp("roundtrip");
        let _ = std::fs::remove_file(&path);
        let mut d = Denylist::new();
        d.add_account("alice");
        d.add_ip("10.0.0.9");
        d.save(&path).unwrap();
        let back = Denylist::load(&path);
        assert_eq!(back, d);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn denylist_load_missing_file_is_empty_not_fatal() {
        let path = tmp("missing");
        let _ = std::fs::remove_file(&path);
        assert!(Denylist::load(&path).is_empty());
    }

    #[test]
    fn empty_strings_are_never_banned() {
        let d = Denylist::new();
        assert!(!d.is_account_banned(""));
        assert!(!d.is_provider_banned("  "));
        assert!(!d.is_ip_banned(""));
    }

    #[test]
    fn report_validate_checks_kind_and_target() {
        let mut r = Report {
            report_id: "abc".into(),
            ts: 0,
            kind: "account".into(),
            target: "5Alice".into(),
            category: Some("csam".into()),
            reason: None,
            source: None,
        };
        assert!(r.validate().is_ok());
        r.kind = "nonsense".into();
        assert!(r.validate().is_err());
        r.kind = "account".into();
        r.target = "   ".into();
        assert!(r.validate().is_err());
    }

    #[test]
    fn reports_roundtrip_newest_first() {
        let path = tmp("reports");
        let _ = std::fs::remove_file(&path);
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let r = Report {
                report_id: (*id).to_string(),
                ts: i as u64,
                kind: "job".into(),
                target: format!("{i}"),
                category: None,
                reason: None,
                source: Some("loopback".into()),
            };
            append_report(&path, &r).unwrap();
        }
        let back = read_reports(&path, 10);
        assert_eq!(
            back.iter()
                .map(|r| r.report_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "b", "a"]
        );
        assert_eq!(read_reports(&path, 2).len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn summarize_counts_kinds_categories_range_and_bans() {
        let mk = |id: &str, ts: u64, kind: &str, cat: Option<&str>| Report {
            report_id: id.into(),
            ts,
            kind: kind.into(),
            target: format!("t-{id}"),
            category: cat.map(|c| c.to_string()),
            reason: None,
            source: None,
        };
        let reports = vec![
            mk("a", 100, "account", Some("csam")),
            mk("b", 300, "account", Some("csam")),
            mk("c", 200, "ip", Some("weapons_mass_casualty")),
            mk("d", 400, "job", None),
        ];
        let mut d = Denylist::new();
        d.add_account("alice");
        d.add_provider("bob");
        d.add_ip("10.0.0.1");
        let s = summarize(&reports, &d);
        assert_eq!(s.total_reports, 4);
        assert_eq!(
            s.by_kind,
            vec![("account".into(), 2), ("ip".into(), 1), ("job".into(), 1)]
        );
        assert_eq!(
            s.by_category,
            vec![
                ("csam".into(), 2),
                ("".into(), 1),
                ("weapons_mass_casualty".into(), 1)
            ]
        );
        assert_eq!((s.first_ts, s.last_ts), (100, 400));
        assert_eq!(
            (s.banned_accounts, s.banned_providers, s.banned_ips),
            (1, 1, 1)
        );
    }

    #[test]
    fn summarize_empty_is_zeroed() {
        let s = summarize(&[], &Denylist::new());
        assert_eq!(s, Summary::default());
        assert_eq!(s.total_reports, 0);
    }

    #[test]
    fn read_all_reports_preserves_order_and_skips_malformed() {
        let path = tmp("all-reports");
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "{\"report_id\":\"a\",\"ts\":1,\"kind\":\"job\",\"target\":\"1\"}\nnot json\n\n{\"report_id\":\"b\",\"ts\":2,\"kind\":\"ip\",\"target\":\"2\"}\n",
        )
        .unwrap();
        let all = read_all_reports(&path);
        assert_eq!(
            all.iter().map(|r| r.report_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn new_report_id_is_nonempty_and_varies() {
        let a = new_report_id();
        let b = new_report_id();
        assert!(!a.is_empty());
        assert_ne!(a, b, "ids should differ across calls");
    }
}
