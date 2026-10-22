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
        // Not tracked off-chain (would need per-block reward accounting).
        // Reported as null so the dashboard shows "—" instead of a guess.
        "earnings_today": serde_json::Value::Null,
        "earnings_note": "not tracked locally — see the explorer",
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
        assert!(v["models"].as_array().unwrap().is_empty());
    }
}
