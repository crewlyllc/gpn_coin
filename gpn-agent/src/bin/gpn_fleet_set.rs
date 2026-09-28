//! gpn_fleet_set — publish a fleet directive (target network for the whole fleet).
//!
//! Usage:
//!   gpn_fleet_set <network> [url-or-path]
//!
//!   url-or-path defaults to $GPN_FLEET_DIRECTIVE, else ~/.gpn/fleet-directive.json.
//!   - If it starts with http(s):// it is POSTed as JSON (a remote directive endpoint).
//!   - Otherwise it is written as a local JSON file.

use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let network = match args.get(1) {
        Some(n) => n.clone(),
        None => {
            eprintln!("usage: gpn_fleet_set <network> [url-or-path]");
            std::process::exit(2);
        }
    };
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let target = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("GPN_FLEET_DIRECTIVE").ok())
        .unwrap_or_else(|| format!("{home}/.gpn/fleet-directive.json"));

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let directive = json!({
        "target_network": network,
        "updated_at": ts,
        "note": format!("set to {network} via gpn_fleet_set"),
    });
    let body = serde_json::to_string_pretty(&directive).unwrap_or_default();

    if target.starts_with("http://") || target.starts_with("https://") {
        print!("POSTing directive to {target} ... ");
        match reqwest::blocking::Client::new()
            .post(&target)
            .header("content-type", "application/json")
            .body(body)
            .send()
        {
            Ok(r) => println!("{}", r.status()),
            Err(e) => {
                eprintln!("FAILED: {e}");
                std::process::exit(1);
            }
        }
    } else {
        if let Some(dir) = std::path::Path::new(&target).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&target, body) {
            Ok(()) => println!("wrote directive → {target}  (target_network={network})"),
            Err(e) => {
                eprintln!("FAILED to write {target}: {e}");
                std::process::exit(1);
            }
        }
    }
}
