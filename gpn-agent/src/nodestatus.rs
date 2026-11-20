//! Status probes + status-document builder shared by the local dashboards.
//!
//! `gpn_node` (single-binary supervisor) and `gpn_fleet` (fleet supervisor) are
//! two different launchers, but the dashboard should say the same thing on
//! both: node/peer/height/GPU/job state, read over the node's own JSON-RPC and
//! the local GPU probe. This module holds those reads so neither supervisor
//! grows its own copy of the HTTP plumbing.
//!
//! Everything here is **best effort and short-timeout**: a dashboard must never
//! hang or lie because the node is starting, the GPU probe is slow, or ollama
//! is down. Unreachable ⇒ `None`/empty, rendered as `—`/`null` by the page.

use std::time::Duration;

/// Cheap HTTP client builder: the dashboard polls, so keep the timeout small.
fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()
        .ok()
}

/// `system_health` → `(peers, is_syncing)`. `None` when the RPC is unreachable.
pub fn health(rpc_port: u16) -> Option<(u64, bool)> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"system_health","params":[]});
    let resp: serde_json::Value = client()?
        .post(format!("http://127.0.0.1:{rpc_port}"))
        .json(&body)
        .send()
        .ok()?
        .json()
        .ok()?;
    let r = resp.get("result")?;
    let peers = r.get("peers")?.as_u64()?;
    let syncing = r
        .get("isSyncing")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some((peers, syncing))
}

/// Best block number from `chain_getHeader`.
pub fn best_block(rpc_port: u16) -> Option<u64> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"chain_getHeader","params":[]});
    let resp: serde_json::Value = client()?
        .post(format!("http://127.0.0.1:{rpc_port}"))
        .json(&body)
        .send()
        .ok()?
        .json()
        .ok()?;
    let hex = resp.get("result")?.get("number")?.as_str()?;
    u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
}

/// GPU name + VRAM/utilisation as one-line strings for the dashboard.
///
/// `GPUMonitor` falls back to a placeholder when no tool answers, so an
/// "Unknown GPU" is reported as *not detected* rather than as hardware.
pub fn gpu_summary() -> (String, String) {
    let Ok(mon) = crate::gpu::GPUMonitor::new() else {
        return ("not detected".into(), String::new());
    };
    match mon.get_gpu_info() {
        Ok(info) if info.model_name != "Unknown GPU" => {
            let used = info.vram_used_mb;
            let total = info.vram_total_bytes / (1024 * 1024);
            (
                info.model_name,
                format!(
                    "VRAM {used}/{total} MiB · {}% util · {}C",
                    info.gpu_utilization_percent, info.temperature_celsius as u32
                ),
            )
        }
        _ => ("not detected".into(), String::new()),
    }
}

/// The account's **total free balance** (item #211 (b)) — the number an
/// operator needs next to "Earnings today", which is a delta and cannot answer
/// "how much do I have?".
///
/// Read over the node's own RPC (subxt, `System::Account`) with a short
/// timeout, cached for [`BALANCE_TTL`] so a page polling every couple of
/// seconds is not a stream of websocket handshakes. `None` = could not read
/// (node starting, RPC down) and the page renders `—`; it never guesses.
pub fn total_balance_gpn(rpc_port: u16, account: &str) -> Option<f64> {
    account_snapshot(rpc_port, account).map(|(balance, _nonce)| balance)
}

/// One cached `System::Account` read shared by the balance card and the
/// earnings ledger (item #211/#212) — two panels, one RPC call.
pub fn account_snapshot(rpc_port: u16, account: &str) -> Option<(f64, u64)> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, String, Option<(f64, u64)>)>> = Mutex::new(None);

    if account.trim().is_empty() {
        return None;
    }
    let mut cache = match CACHE.lock() {
        Ok(c) => c,
        Err(p) => p.into_inner(),
    };
    if let Some((at, who, val)) = cache.as_ref() {
        let ttl = if val.is_some() {
            BALANCE_TTL
        } else {
            BALANCE_RETRY
        };
        if who == account && at.elapsed() < ttl {
            return *val;
        }
    }
    let endpoint = format!("ws://127.0.0.1:{rpc_port}");
    let read = crate::chain::read_balance_blocking(&endpoint, account, BALANCE_TIMEOUT)
        .ok()
        .map(|i| (i.balance, i.nonce));
    *cache = Some((Instant::now(), account.to_string(), read));
    read
}

/// How long a dashboard balance read is reused.
const BALANCE_TTL: Duration = Duration::from_secs(30);
/// How soon a failed read may be retried: subxt's first connect fetches
/// metadata, so a cold read legitimately takes seconds, and a node that has
/// just come up must not show a stale dash for a whole minute.
const BALANCE_RETRY: Duration = Duration::from_secs(5);
/// A dashboard poll must never hang on a starting node.
const BALANCE_TIMEOUT: Duration = Duration::from_secs(5);

/// Today's earnings (item #212): chain balance growth since the day's baseline,
/// or the reason there is no usable figure.
///
/// The figure is folded into a persisted day ledger ([`crate::earnings`]) on
/// every call, so a node that is running accumulates the day even when nobody
/// has the dashboard open. `(value, note)` — `None` means "show a dash".
pub fn earnings_today(rpc_port: u16, account: &str) -> (Option<f64>, String) {
    match account_snapshot(rpc_port, account) {
        Some((balance, nonce)) => {
            crate::earnings::sample(&earnings_base(), crate::earnings::now_ms(), balance, nonce)
        }
        None => (
            None,
            "no figure: the node did not answer a balance read".to_string(),
        ),
    }
}

/// Where the day ledger lives: `GPN_BASE_PATH` if set (the same override the
/// node honours), else the OS default base path.
fn earnings_base() -> std::path::PathBuf {
    std::env::var("GPN_BASE_PATH")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::firstrun::default_base_path)
}

/// Models the local ollama reports as present (best effort, short timeout).
pub fn ollama_models(ollama_url: &str) -> Vec<String> {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(700))
        .build()
    else {
        return Vec::new();
    };
    let url = format!("{}/api/tags", ollama_url.trim_end_matches('/'));
    let Some(v) = client
        .get(url)
        .send()
        .ok()
        .and_then(|r| r.json::<serde_json::Value>().ok())
    else {
        return Vec::new();
    };
    let mut out: Vec<String> = v
        .get("models")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.truncate(12);
    out
}

/// Inputs the supervisor knows that the probes cannot infer.
pub struct StatusParams<'a> {
    /// Is the node child process up? (supervisor-owned)
    pub node_up: bool,
    /// Active network / chain name.
    pub chain: &'a str,
    /// Node JSON-RPC port.
    pub rpc_port: u16,
    /// Balance/identity account shown on the panel.
    pub account: &'a str,
    /// Version-standby: syncing but not mining (update cutoff).
    pub standby: bool,
    /// Per-child rows for the panel (label/up/pid/…). May be empty.
    pub children: Vec<serde_json::Value>,
    /// ollama base URL for the model inventory.
    pub ollama: &'a str,
}

/// The JSON document behind `GET /api/status` (the dashboard page's data).
pub fn status_doc(p: &StatusParams<'_>) -> serde_json::Value {
    use serde_json::json;

    let (peers, syncing) = match health(p.rpc_port) {
        Some((peers, s)) => (Some(peers), s),
        None => (None, false),
    };
    let best = best_block(p.rpc_port);
    let idle = crate::idle::current();
    let idle_active = idle.is_some();
    // Mining is on when the node is up, not idle, and not in version standby.
    let mining = p.node_up && !idle_active && !p.standby;
    let (gpu, vram) = gpu_summary();
    // One GPU, one job: the provider holds this flag for a job's whole duration.
    let job_running = crate::gpu_yield::yield_flag_path().exists();
    let job = if job_running {
        json!({"kind": "ai job", "id": null, "model": ""})
    } else {
        serde_json::Value::Null
    };
    // Item #212: one cached chain read for the panel's earnings figure.
    let (earnings, earnings_note) = earnings_today(p.rpc_port, p.account);
    let earnings_value = earnings.map_or(serde_json::Value::Null, |b| json!(b));

    json!({
        "node": if p.node_up { "up" } else { "down" },
        "peers": peers,
        "syncing": syncing,
        "best": best,
        "mining": mining,
        "standby": p.standby,
        "gpu": gpu,
        "vram": vram,
        "current_job": job,
        "models": ollama_models(p.ollama),
        "children": p.children,
        "chain": p.chain,
        "rpc_port": p.rpc_port,
        "account": p.account,
        // Item #212: a chain-observed figure with its source and window, or a
        // dash plus the reason. `earnings_today` is null when unreadable — the
        // page never invents a number and never shows a plausible zero.
        "earnings_today": earnings_value,
        "earnings_source": "chain",
        "earnings_window": crate::earnings::WINDOW_LABEL,
        "earnings_note": earnings_note,
        // Item #211 (b): a *total* beside the delta, read from the chain.
        "total_balance": total_balance_gpn(p.rpc_port, p.account)
            .map_or(serde_json::Value::Null, |b| serde_json::json!(b)),
        "balance_note": "free balance on chain, read from this node's RPC",
        "spec": env!("CARGO_PKG_VERSION"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_rpc_is_none_not_a_panic() {
        // Port 1 is never a Substrate RPC; both probes must degrade quietly.
        assert!(health(1).is_none());
        assert!(best_block(1).is_none());
    }

    /// Live probe, opt-in: `GPN_TEST_ACCOUNT=<ss58> cargo test --release --lib
    /// live_status_doc -- --ignored --nocapture`. Prints the real numbers the
    /// panel would show, twice, so the ledger's delta can be compared to the
    /// chain balance's delta.
    #[test]
    #[ignore = "reads the live local node; opt in with GPN_TEST_ACCOUNT"]
    fn live_status_doc_carries_chain_balance_and_earnings() {
        let Ok(account) = std::env::var("GPN_TEST_ACCOUNT") else {
            eprintln!("GPN_TEST_ACCOUNT not set — nothing to probe");
            return;
        };
        let dir = std::env::temp_dir().join(format!("gpn-live-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("GPN_BASE_PATH", &dir);
        let p = StatusParams {
            node_up: true,
            chain: "testnet",
            rpc_port: 9944,
            account: &account,
            standby: false,
            children: vec![],
            ollama: "http://127.0.0.1:1",
        };
        let first = status_doc(&p);
        println!(
            "t0 total_balance={:?} earnings={:?} note={:?} window={:?}",
            first["total_balance"],
            first["earnings_today"],
            first["earnings_note"],
            first["earnings_window"]
        );
        std::thread::sleep(std::time::Duration::from_secs(40));
        let second = status_doc(&p);
        println!(
            "t1 total_balance={:?} earnings={:?} note={:?}",
            second["total_balance"], second["earnings_today"], second["earnings_note"]
        );
        assert!(first["earnings_window"].as_str().unwrap().contains("UTC"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_doc_reports_a_down_node_as_down_and_not_mining() {
        let p = StatusParams {
            node_up: false,
            chain: "testnet",
            rpc_port: 1,
            account: "5Test",
            standby: false,
            children: vec![],
            ollama: "http://127.0.0.1:1",
        };
        let v = status_doc(&p);
        assert_eq!(v["node"], "down");
        assert_eq!(v["mining"], false);
        assert_eq!(v["peers"], serde_json::Value::Null);
        assert_eq!(v["best"], serde_json::Value::Null);
        // The dashboard must never invent earnings.
        assert_eq!(v["earnings_today"], serde_json::Value::Null);
        assert!(v["earnings_note"]
            .as_str()
            .unwrap()
            .starts_with("no figure:"));
        assert_eq!(v["earnings_window"], crate::earnings::WINDOW_LABEL);
        // Item #211: the total is present and unreadable-⇒-null (port 1 has no
        // RPC), so the page shows a dash rather than a fabricated 0 GPN.
        assert_eq!(v["total_balance"], serde_json::Value::Null);
        assert!(v["balance_note"].as_str().unwrap().contains("chain"));
        assert!(v["models"].as_array().unwrap().is_empty());
    }
}
