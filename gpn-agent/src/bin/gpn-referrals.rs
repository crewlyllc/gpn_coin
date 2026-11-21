//! gpn-referrals — the ADMIN view of the referral registry.
//!
//! Row 226 (design `docs/design-referral-codes.md` §4b): attribution is
//! operator-only. There is **no HTTP route** that lists who referred whom:
//! an operator reads their own count through the signed
//! `GET /api/referrals?code=&sig=` on the access point, and the whole registry
//! is readable **only on the AP host**, by this tool, straight off disk.
//!
//! That is the point of the split: a token-protected route would still be a
//! route (something to leak, something to scrape); a file read on the machine
//! that already holds the file is not.
//!
//! Usage:
//!   gpn-referrals                 summary: one line per code (count, first seen)
//!   gpn-referrals --all           every address → code attribution, then the summary
//!   gpn-referrals --code <code>   one code's tally, in the operator's own shape
//!   gpn-referrals --json          machine-readable output for any of the above
//!   gpn-referrals --registry <p>  read a specific registry file
//!
//! Exit codes: 0 read ok · 2 usage error · 3 the registry could not be parsed.
//! A missing registry is **not** an error: it means no referrals were recorded
//! yet, and saying so plainly beats a confident empty table.

use gpn_agent::referral;
use std::collections::BTreeMap;
use std::path::PathBuf;

const USAGE: &str = "\
usage: gpn-referrals [--all] [--code <code>] [--json] [--registry <path>]

  (no flags)        one line per code: count, first seen, status mix
  --all             every address → code attribution, then the per-code summary
  --code <code>     one code's tally (same fields GET /api/referrals answers)
  --json            machine-readable output
  --registry <p>    registry file (default $GPN_REFERRAL_REGISTRY or
                    $GPN_PREFIX/referral-registry.json)

Admin-only by design: run this on the access point's host. The HTTP surface
never lists attributions — operators read their OWN count with a wallet
signature (`gpn_wallet sign <wallet> 'gpn-referrals-v1|<code>'`, then
`GET /api/referrals?code=<code>&sig=<signature>`).
Exit: 0 read ok · 2 usage · 3 registry unreadable/unparseable.
";

#[derive(Debug, PartialEq)]
pub struct Invocation {
    pub all: bool,
    pub code: Option<String>,
    pub json: bool,
    pub registry: Option<PathBuf>,
}

pub fn resolve_args(raw: &[String]) -> Result<Invocation, String> {
    let mut inv = Invocation {
        all: false,
        code: None,
        json: false,
        registry: None,
    };
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--all" => inv.all = true,
            "--json" => inv.json = true,
            "--code" => {
                i += 1;
                let v = raw
                    .get(i)
                    .ok_or_else(|| "--code needs a value".to_string())?;
                inv.code = Some(v.clone());
            }
            "--registry" => {
                i += 1;
                let v = raw
                    .get(i)
                    .ok_or_else(|| "--registry needs a path".to_string())?;
                inv.registry = Some(PathBuf::from(v));
            }
            "--help" | "-h" | "help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown option `{other}`")),
        }
        i += 1;
    }
    if inv.all && inv.code.is_some() {
        return Err("--all and --code are different views; pick one".to_string());
    }
    Ok(inv)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for unix seconds (no chrono in this crate).
pub fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days, shifted to the Unix epoch.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let inv = match resolve_args(&raw) {
        Ok(i) => i,
        Err(e) => {
            // Usage text is not an error: print it and exit 0 for --help.
            if e.starts_with("usage:") {
                print!("{e}");
                std::process::exit(0);
            }
            eprintln!("gpn-referrals: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let path = inv.registry.clone().unwrap_or_else(referral::registry_path);
    let reg: BTreeMap<String, referral::Entry> = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("gpn-referrals: {}: {e}", path.display());
                std::process::exit(3);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => {
            eprintln!("gpn-referrals: {}: {e}", path.display());
            std::process::exit(3);
        }
    };

    if inv.json {
        let out = match &inv.code {
            Some(c) => serde_json::to_value(referral::count_for_code(&reg, c)).unwrap(),
            None => {
                let mut codes: BTreeMap<String, serde_json::Value> = BTreeMap::new();
                for (code, view) in by_code(&reg) {
                    codes.insert(code, serde_json::to_value(view).unwrap());
                }
                let mut v = serde_json::json!({
                    "registry": path.display().to_string(),
                    "addresses": reg.len(),
                    "codes": codes,
                });
                if inv.all {
                    v["attributions"] = serde_json::json!(reg
                        .iter()
                        .map(|(addr, e)| serde_json::json!({
                            "address": addr,
                            "ref": e.r#ref,
                            "code": e.code,
                            "derived_from": e.derived_from,
                            "status": e.status,
                            "first_seen": e.first_seen,
                        }))
                        .collect::<Vec<_>>());
                }
                return_json(v);
                return;
            }
        };
        return_json(out);
        return;
    }

    if reg.is_empty() {
        println!(
            "no referrals recorded yet — {} does not exist or holds no entries",
            path.display()
        );
        return;
    }

    if let Some(code) = &inv.code {
        let view = referral::count_for_code(&reg, code);
        println!("code       {}", view.r#code);
        println!("referred   {}", view.referred);
        match view.first_seen {
            Some(t) => println!("first seen {} ({} epoch)", utc_stamp(t), t),
            None => println!("first seen —"),
        }
        for seen in &view.recent {
            println!("  {}", utc_stamp(seen.first_seen));
        }
        if view.referred == 0 {
            println!(
                "(no credited referrals: an entry recorded as `{}` is a replayed code and is never credited)",
                referral::STATUS_INVALID_FOR_ADDRESS
            );
        }
        return;
    }

    if inv.all {
        println!(
            "# attributions — {} ({} addresses)",
            path.display(),
            reg.len()
        );
        for (addr, e) in &reg {
            println!(
                "{addr}  {:<24} {:<14} {:<20} first {}{}",
                canonical(&e.r#ref),
                e.status,
                e.derived_from.clone().unwrap_or_else(|| "-".into()),
                utc_stamp(e.first_seen),
                if e.status == referral::STATUS_INVALID_FOR_ADDRESS {
                    "  [NOT CREDITED: code does not derive from that address]"
                } else {
                    ""
                }
            );
        }
        println!();
    }

    println!(
        "# codes — {} ({} addresses, {} codes)",
        path.display(),
        reg.len(),
        by_code(&reg).len()
    );
    for (code, view) in by_code(&reg) {
        println!(
            "{:<18} {:>4} referred   first {}",
            code,
            view.referred,
            view.first_seen.map(utc_stamp).unwrap_or_else(|| "-".into())
        );
    }
}

/// Tally the registry per code, newest-first by first appearance.
///
/// Codes are grouped by their CANONICAL form: a registry written before row 223
/// stores the display spelling (`GPN-AX3R-YC7C-KV`) while a later entry stores
/// the compact one (`GPNAX3RYC7CKV`) — same code, and it must appear once.
fn by_code(reg: &BTreeMap<String, referral::Entry>) -> Vec<(String, referral::CountView)> {
    let mut out: Vec<(String, referral::CountView)> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in reg.values() {
        if e.status == referral::STATUS_INVALID_FOR_ADDRESS {
            continue;
        }
        let view = referral::count_for_code(reg, &e.r#ref);
        if seen.insert(view.r#code.clone()) {
            out.push((view.r#code.clone(), view));
        }
    }
    out.sort_by(|a, b| b.1.referred.cmp(&a.1.referred).then(a.0.cmp(&b.0)));
    out
}

/// Display form of a stored code (unchanged for a legacy typed string).
fn canonical(raw: &str) -> String {
    match referral::parse_code(raw) {
        Ok(compact) => referral::display_form(&compact),
        Err(_) => raw.trim().to_string(),
    }
}

fn return_json(v: serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_stable_and_flags_are_never_reinterpreted() {
        assert_eq!(
            resolve_args(&["--all".into()]).unwrap(),
            Invocation {
                all: true,
                code: None,
                json: false,
                registry: None
            }
        );
        let i =
            resolve_args(&["--code".into(), "GPN-AX3R-YC7C-KV".into(), "--json".into()]).unwrap();
        assert_eq!(i.code.as_deref(), Some("GPN-AX3R-YC7C-KV"));
        assert!(i.json);
        assert!(resolve_args(&["--code".into()]).is_err());
        assert!(resolve_args(&["--all".into(), "--code".into(), "x".into()]).is_err());
        assert!(resolve_args(&["--nope".into()]).is_err());
    }

    #[test]
    fn codes_are_grouped_by_canonical_form_not_stored_spelling() {
        // A registry written before row 223 stores the display spelling; a later
        // entry stores the compact one. Same code — it must appear once.
        let mut reg: BTreeMap<String, referral::Entry> = BTreeMap::new();
        let entry = |r: &str, t: u64| referral::Entry {
            r#ref: r.to_string(),
            first_seen: t,
            code: referral::parse_code(r).ok(),
            derived_from: None,
            status: referral::STATUS_UNVERIFIED.to_string(),
        };
        reg.insert("5A".into(), entry("GPN-AX3R-YC7C-KV", 10));
        reg.insert("5B".into(), entry("GPNAX3RYC7CKV", 20));
        reg.insert("5C".into(), entry("GPN-ZYCU-4W49-6R", 30));
        let tally = by_code(&reg);
        assert_eq!(tally.len(), 2, "{tally:?}");
        assert_eq!(tally[0].0, "GPN-AX3R-YC7C-KV");
        assert_eq!(tally[0].1.referred, 2);
        assert_eq!(canonical("GPNAX3RYC7CKV"), "GPN-AX3R-YC7C-KV");
        // A legacy typed string is shown as typed.
        assert_eq!(canonical("team-alpha"), "team-alpha");
    }

    #[test]
    fn timestamps_are_utc_and_correct_across_a_year_boundary() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1764000000), "2025-11-24T16:00:00Z");
        // 2024-02-29T12:34:56Z — a leap day, the case a naive /365 gets wrong.
        assert_eq!(utc_stamp(1709210096), "2024-02-29T12:34:56Z");
    }
}
