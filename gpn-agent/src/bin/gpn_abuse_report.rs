//! gpn_abuse_report — operator CLI that prints the **abuse-transparency summary**
//! (item #22 build-order step 5).
//!
//! It reads the same files the access point and providers enforce from:
//!   - the report log (`GPN_REPORTS_FILE`, default `~/.gpn/reports.jsonl`), and
//!   - the shared denylist (`GPN_DENY_FILE`, default `~/.gpn/denylist.json`),
//!
//! and prints content-free counts (reports by kind/category, the time range, and
//! the number of active bans) — the numbers an annual transparency note is built
//! from (`design-abuse-safety-guardrails.md` §3). It prints **metadata only**:
//! never a prompt or response (design §5).
//!
//! This is an **off-chain, read-only** tool — no genesis, no runtime, no
//! `set_code`, no network. It touches only local files.
//!
//! Usage:
//!   gpn_abuse_report                 # human-readable summary
//!   gpn_abuse_report --json          # machine-readable (for a transparency-note build)
//!   gpn_abuse_report --reports <f> --deny <f>

use gpn_agent::abuse;

fn main() {
    let mut json = false;
    let mut reports_path = abuse::reports_path();
    let mut deny_path = abuse::Denylist::default_path();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json = true,
            "--reports" => {
                if let Some(p) = args.next() {
                    reports_path = p.into();
                }
            }
            "--deny" => {
                if let Some(p) = args.next() {
                    deny_path = p.into();
                }
            }
            "-h" | "--help" => {
                println!(
                    "gpn_abuse_report [--json] [--reports <file>] [--deny <file>]\n\
                     Prints the content-free abuse-transparency summary."
                );
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let reports = abuse::read_all_reports(&reports_path);
    let denylist = abuse::Denylist::load(&deny_path);
    let s = abuse::summarize(&reports, &denylist);

    if json {
        println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
        return;
    }

    let fmt_ts = |ts: u64| {
        if ts == 0 {
            "—".to_string()
        } else {
            ts.to_string()
        }
    };
    println!("GPN abuse transparency summary");
    println!("  reports file : {}", reports_path.display());
    println!("  denylist file: {}", deny_path.display());
    println!("  total reports: {}", s.total_reports);
    println!("  time range   : {} .. {}", fmt_ts(s.first_ts), fmt_ts(s.last_ts));
    println!("  by kind      :");
    for (k, n) in &s.by_kind {
        println!("    {k:<10} {n}");
    }
    println!("  by category  :");
    for (c, n) in &s.by_category {
        let label = if c.is_empty() { "(uncategorised)" } else { c };
        println!("    {label:<24} {n}");
    }
    println!(
        "  active bans  : accounts={} providers={} ips={}",
        s.banned_accounts, s.banned_providers, s.banned_ips
    );
}
