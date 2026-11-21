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
//!   gpn-referrals --export [<p>]  deterministic airdrop input (schema-versioned)
//!   gpn-referrals --registry <p>  read a specific registry file
//!
//! Exit codes: 0 read ok · 2 usage error · 3 the registry could not be parsed.
//! A missing registry is **not** an error: it means no referrals were recorded
//! yet, and saying so plainly beats a confident empty table.

use gpn_agent::referral;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Schema tag of `--export` output. Bumped only when a field's meaning changes;
/// a consumer that does not know this string must refuse the file.
pub const EXPORT_SCHEMA: &str = "gpn-airdrop-input-v1";

const USAGE: &str = "\
usage: gpn-referrals [--all] [--code <code>] [--json] [--registry <path>]
                    [--export [<file>]]

  (no flags)        one line per code: count, first seen, status mix
  --all             every address → code attribution, then the per-code summary
  --code <code>     one code's tally (same fields GET /api/referrals answers)
  --json            machine-readable output
  --export [<f>]    deterministic airdrop input; to <f>, or stdout if omitted
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
    /// `--export` present; the value is the destination file (None = stdout).
    pub export: Option<Option<PathBuf>>,
    pub registry: Option<PathBuf>,
}

pub fn resolve_args(raw: &[String]) -> Result<Invocation, String> {
    let mut inv = Invocation {
        all: false,
        code: None,
        json: false,
        export: None,
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
            "--export" => {
                // The destination is optional: `--export` alone writes to stdout,
                // which is what a pipeline wants (`gpn-referrals --json --registry
                // … | sha256sum` has no temp file to leak).
                let dest = raw.get(i + 1).filter(|v| !v.starts_with('-'));
                if dest.is_some() {
                    i += 1;
                }
                inv.export = Some(dest.map(PathBuf::from));
            }
            "--help" | "-h" | "help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown option `{other}`")),
        }
        i += 1;
    }
    if inv.all && inv.code.is_some() {
        return Err("--all and --code are different views; pick one".to_string());
    }
    if inv.export.is_some() && (inv.all || inv.code.is_some() || inv.json) {
        return Err("--export is its own output; it takes no --all/--code/--json".to_string());
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
    let raw_registry: Option<String> = match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("gpn-referrals: {}: {e}", path.display());
            std::process::exit(3);
        }
    };
    let reg: BTreeMap<String, referral::Entry> = match &raw_registry {
        Some(s) => match serde_json::from_str(s) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("gpn-referrals: {}: {e}", path.display());
                std::process::exit(3);
            }
        },
        None => BTreeMap::new(),
    };

    if let Some(dest) = &inv.export {
        // Byte-identical on a re-run of the same registry: no wall clock, no
        // absolute path, every collection ordered by BTreeMap/address.
        let doc = build_export(&path, &reg, raw_registry.as_deref());
        let text = serde_json::to_string_pretty(&doc).unwrap();
        match dest {
            Some(d) => {
                let tmp = d.with_extension("json.tmp");
                if let Err(e) =
                    std::fs::write(&tmp, format!("{text}\n")).and_then(|_| std::fs::rename(&tmp, d))
                {
                    eprintln!("gpn-referrals: {}: {e}", d.display());
                    std::process::exit(3);
                }
                println!(
                    "wrote {} — {} participants ({} credited), {} referrers, {} self-referral(s) excluded",
                    d.display(),
                    doc["totals"]["participants"],
                    doc["totals"]["credited"],
                    doc["totals"]["referrers"],
                    doc["totals"]["self_referrals"]
                );
            }
            None => println!("{text}"),
        }
        return;
    }

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

/// Build the airdrop input document.
///
/// Contract (design `docs/design-referral-codes.md` §7c):
/// * `participants` carries EVERY registry entry, including ones that will not be
///   paid (`invalid_for_address`), each with `credited` so the evidence and the
///   payout input are the same file;
/// * payout totals count only `credited` participants;
/// * self-referrals are listed from the sidecar and are never credited;
/// * output depends only on the input bytes — no clock, no absolute path — so a
///   re-run on the same registry is byte-identical.
fn build_export(
    registry: &Path,
    reg: &BTreeMap<String, referral::Entry>,
    raw_registry: Option<&str>,
) -> serde_json::Value {
    let mut digest = Sha256::new();
    digest.update(raw_registry.unwrap_or("").as_bytes());
    let registry_sha256 = format!("{:x}", digest.finalize());

    let mut credited = 0usize;
    let mut excluded = 0usize;
    let mut participants: Vec<serde_json::Value> = Vec::new();
    for (addr, e) in reg {
        let is_credited = e.status != referral::STATUS_INVALID_FOR_ADDRESS;
        if is_credited {
            credited += 1;
        } else {
            excluded += 1;
        }
        participants.push(serde_json::json!({
            "address": addr,
            "referred_by": canonical(&e.r#ref),
            "referred_by_address": e.derived_from,
            "status": e.status,
            "derivation_verified": e.status == referral::STATUS_VERIFIED,
            "credited": is_credited,
            "first_seen": e.first_seen,
            "last_seen": referral::last_seen_of(e),
        }));
    }

    // Referrers: credited referrals grouped by code, each with the wallet the
    // code recomputes from when the entry proved it.
    let mut referrers: Vec<serde_json::Value> = Vec::new();
    for (code, view) in by_code(reg) {
        let entries: Vec<&referral::Entry> = reg
            .values()
            .filter(|e| e.status != referral::STATUS_INVALID_FOR_ADDRESS)
            .filter(|e| canonical(&e.r#ref) == code)
            .collect();
        let verified = entries
            .iter()
            .filter(|e| e.status == referral::STATUS_VERIFIED)
            .count();
        let mut owner: Option<String> = None;
        for e in &entries {
            if e.status == referral::STATUS_VERIFIED {
                if let Some(a) = &e.derived_from {
                    owner = Some(a.clone());
                    break;
                }
            }
        }
        referrers.push(serde_json::json!({
            "address": owner,
            "code": code,
            "referred": view.referred,
            "verified_referrals": verified,
        }));
    }

    let self_path = referral::self_referrals_path(registry);
    let self_refs: Vec<serde_json::Value> = referral::load_self_referrals(&self_path)
        .iter()
        .map(|(addr, e)| {
            serde_json::json!({
                "address": addr,
                "referred_by": canonical(&e.r#ref),
                "first_seen": e.first_seen,
                "last_seen": e.last_seen,
                "attempts": e.attempts,
                "credited": false,
            })
        })
        .collect();

    serde_json::json!({
        "schema": EXPORT_SCHEMA,
        "registry_file": registry.file_name().and_then(|n| n.to_str()).unwrap_or("referral-registry.json"),
        "registry_present": raw_registry.is_some(),
        "registry_sha256": registry_sha256,
        "totals": {
            "participants": participants.len(),
            "credited": credited,
            "excluded_invalid_for_address": excluded,
            "self_referrals": self_refs.len(),
            "referrers": referrers.len(),
        },
        "participants": participants,
        "referrers": referrers,
        "self_referrals": self_refs,
    })
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
                export: None,
                registry: None
            }
        );
        assert_eq!(
            resolve_args(&["--export".into()]).unwrap().export,
            Some(None),
            "--export alone means stdout"
        );
        assert_eq!(
            resolve_args(&["--export".into(), "out.json".into()])
                .unwrap()
                .export,
            Some(Some(PathBuf::from("out.json")))
        );
        // A following flag is not a destination (and must not be swallowed).
        let inv = resolve_args(&["--export".into(), "--registry".into(), "r.json".into()]).unwrap();
        assert_eq!(inv.export, Some(None));
        assert_eq!(inv.registry, Some(PathBuf::from("r.json")));
        assert!(resolve_args(&["--export".into(), "--json".into()]).is_err());
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
            last_seen: 0,
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

    fn td(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("gpn-ref-export-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Row 227's contract, end to end: dedupe, determinism, and evidence that
    /// never reaches a payout total.
    #[test]
    fn export_is_deterministic_and_pays_only_credited_entries() {
        const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
        const BOB: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";
        let dir = td("airdrop");
        let p = dir.join("referral-registry.json");
        let alice = referral::derive_code(ALICE).unwrap();

        // One operator who reinstalled five times: ONE participant row, with
        // last_seen moving and first_seen fixed.
        for t in 100..105u64 {
            referral::record_once_checked(&p, "5Node1", &alice, Some(ALICE), None, t).unwrap();
        }
        // A node that upgraded before its installer knew --ref: attributed,
        // but not provable.
        referral::record_once_checked(&p, "5Node2", &alice, None, None, 200).unwrap();
        // A replayed code: written as evidence, never credited.
        referral::record_once_checked(&p, "5Replay", &alice, Some(BOB), None, 300).unwrap();
        // A pre-0.3.65 typed code: counted, flagged as unproven.
        referral::record_once_checked(&p, "5Legacy", "team-alpha", None, None, 400).unwrap();
        // A self-referral: evidence in the sidecar, never a participant.
        referral::record_once_checked(&p, ALICE, &alice, Some(ALICE), None, 500).unwrap();

        let raw = std::fs::read_to_string(&p).unwrap();
        let reg = referral::load_registry(&p);
        let doc = build_export(&p, &reg, Some(&raw));
        let text = serde_json::to_string_pretty(&doc).unwrap();

        assert_eq!(doc["schema"], EXPORT_SCHEMA);
        assert_eq!(doc["registry_present"], true);
        assert_eq!(doc["registry_file"], "referral-registry.json");
        assert_eq!(doc["registry_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(doc["totals"]["participants"], 4, "5 records → 1 row");
        assert_eq!(doc["totals"]["credited"], 3);
        assert_eq!(doc["totals"]["excluded_invalid_for_address"], 1);
        assert_eq!(doc["totals"]["self_referrals"], 1);
        assert_eq!(doc["totals"]["referrers"], 2);

        let part = |addr: &str| {
            doc["participants"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["address"] == addr)
                .unwrap()
                .clone()
        };
        let n1 = part("5Node1");
        assert_eq!(n1["first_seen"], 100);
        assert_eq!(n1["last_seen"], 104, "last sighting wins, not the first");
        assert_eq!(n1["credited"], true);
        assert_eq!(n1["derivation_verified"], true);
        assert_eq!(n1["referred_by_address"], ALICE);
        assert_eq!(n1["referred_by"], referral::display_form(&alice));

        let n2 = part("5Node2");
        assert_eq!(n2["derivation_verified"], false);
        assert_eq!(n2["credited"], true, "unverified is still attributed");

        let replay = part("5Replay");
        assert_eq!(replay["credited"], false);
        assert_eq!(replay["status"], referral::STATUS_INVALID_FOR_ADDRESS);

        // The self-referral is present as evidence and absent from participants.
        let selfs = doc["self_referrals"].as_array().unwrap();
        assert_eq!(selfs.len(), 1);
        assert_eq!(selfs[0]["address"], ALICE);
        assert_eq!(selfs[0]["credited"], false);
        assert!(doc["participants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["address"] != ALICE));

        // Byte-identical on a re-run of the same registry.
        let again = serde_json::to_string_pretty(&build_export(
            &p,
            &referral::load_registry(&p),
            Some(&std::fs::read_to_string(&p).unwrap()),
        ))
        .unwrap();
        assert_eq!(text, again, "the export must be reproducible");
    }

    #[test]
    fn export_of_a_missing_registry_is_empty_not_an_error() {
        let dir = td("empty");
        let p = dir.join("referral-registry.json");
        let doc = build_export(&p, &BTreeMap::new(), None);
        assert_eq!(doc["registry_present"], false);
        assert_eq!(doc["totals"]["participants"], 0);
        assert_eq!(doc["participants"].as_array().unwrap().len(), 0);
        assert_eq!(doc["self_referrals"].as_array().unwrap().len(), 0);
    }
}
