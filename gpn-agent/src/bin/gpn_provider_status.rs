//! gpn_provider_status — a tiny HTTP heartbeat for a provider, polled by the
//! access-point scheduler to learn capabilities + load + latency.
//!
//! `GET /status` → `{ address, gflops, models[], can_pull, active, capacity }`
//!
//! `models[]` is read LIVE from the local Ollama (`GET /api/tags`), so the network
//! always sees what this machine actually has. `can_pull` = Ollama is reachable,
//! i.e. this provider could download a requested model on demand.
//!
//! Usage:
//!   gpn_provider_status --listen <ip:port> --address <ss58> --gflops <n>
//!                       [--models <m1,m2>] [--capacity <n>] [--active <n>]
//!                       [--ollama http://127.0.0.1:11434]
//! Env: LISTEN, ADDRESS, GFLOPS, MODELS, CAPACITY, ACTIVE, GPN_OLLAMA

use serde_json::json;
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server};

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

/// Live model inventory from Ollama. Returns (models, can_pull).
fn ollama_models(base: &str, fallback: &[String]) -> (Vec<String>, bool) {
    let cli = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(2500))
        .build();
    match cli.and_then(|c| c.get(format!("{base}/api/tags")).send()) {
        Ok(r) => {
            let v: serde_json::Value = r.json().unwrap_or_default();
            let names: Vec<String> = v["models"]
                .as_array()
                .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            if names.is_empty() { (fallback.to_vec(), true) } else { (names, true) }
        }
        Err(_) => (fallback.to_vec(), false),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let listen = opt(&args, "--listen").or_else(|| std::env::var("LISTEN").ok()).unwrap_or_else(|| "0.0.0.0:8093".into());
    let address = opt(&args, "--address").or_else(|| std::env::var("ADDRESS").ok()).unwrap_or_default();
    let gflops: u64 = opt(&args, "--gflops").or_else(|| std::env::var("GFLOPS").ok()).and_then(|s| s.parse().ok()).unwrap_or(0);
    let models_csv = opt(&args, "--models").or_else(|| std::env::var("MODELS").ok()).unwrap_or_else(|| "llama3.2:1b".into());
    let capacity: u64 = opt(&args, "--capacity").or_else(|| std::env::var("CAPACITY").ok()).and_then(|s| s.parse().ok()).unwrap_or(1);
    let active: u64 = opt(&args, "--active").or_else(|| std::env::var("ACTIVE").ok()).and_then(|s| s.parse().ok()).unwrap_or(0);
    let ollama = opt(&args, "--ollama").or_else(|| std::env::var("GPN_OLLAMA").ok()).unwrap_or_else(|| "http://127.0.0.1:11434".into());
    // Real compute economics for the rate card:
    //   --tok-per-s          the provider's MEASURED throughput (tokens/sec)
    //   --owner-cost-usd-hr  the provider's own cost/hr ($/kWh + amortized GPU)
    let tok_per_s: f64 = opt(&args, "--tok-per-s")
        .or_else(|| std::env::var("GPN_TOK_PER_S").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(40.0);
    let owner_cost_usd_hr: f64 = opt(&args, "--owner-cost-usd-hr")
        .or_else(|| std::env::var("GPN_OWNER_COST_USD_HR").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.03);
    // Memory bandwidth (GB/s) — the other half of the roofline. A hardware spec.
    let mem_bw_gbps: f64 = opt(&args, "--mem-bw-gbps")
        .or_else(|| std::env::var("GPN_MEM_BW_GBPS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(250.0);
    let models: Vec<String> = models_csv.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    println!("GPN provider heartbeat on http://{listen} — address={address} gflops={gflops} models={:?} active={active}/{capacity}", models);

    let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    for req in server.incoming_requests() {
        let body = match (req.method(), req.url()) {
            (Method::Get, "/status") => {
                let (models_now, can_pull) = ollama_models(&ollama, &models);
                json!({
                    "address": address, "gflops": gflops, "models": models_now,
                    "can_pull": can_pull, "active": active, "capacity": capacity.max(1),
                    "tok_per_s": tok_per_s, "owner_cost_usd_hr": owner_cost_usd_hr,
                    "mem_bw_gbps": mem_bw_gbps
                })
            }
            (Method::Get, "/health") => json!({"status":"ok"}),
            _ => json!({"error":"not found"}),
        };
        let _ = req.respond(Response::from_string(body.to_string()).with_header(hdr.clone()));
    }
    Ok(())
}
