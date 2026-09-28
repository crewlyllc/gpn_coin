//! gpn_provider_status — a tiny HTTP heartbeat for a provider, polled by the
//! access-point scheduler to learn capabilities + load + latency.
//!
//! `GET /status` → `{ address, gflops, models[], active, capacity }`
//!
//! Usage:
//!   gpn_provider_status --listen <ip:port> --address <ss58> --gflops <n>
//!                       [--models <m1,m2>] [--capacity <n>] [--active <n>]
//! Env: LISTEN, ADDRESS, GFLOPS, MODELS, CAPACITY, ACTIVE

use serde_json::json;
use tiny_http::{Header, Method, Response, Server};

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let listen = opt(&args, "--listen").or_else(|| std::env::var("LISTEN").ok()).unwrap_or_else(|| "0.0.0.0:8093".into());
    let address = opt(&args, "--address").or_else(|| std::env::var("ADDRESS").ok()).unwrap_or_default();
    let gflops: u64 = opt(&args, "--gflops").or_else(|| std::env::var("GFLOPS").ok()).and_then(|s| s.parse().ok()).unwrap_or(0);
    let models_csv = opt(&args, "--models").or_else(|| std::env::var("MODELS").ok()).unwrap_or_else(|| "llama3.2:1b".into());
    let capacity: u64 = opt(&args, "--capacity").or_else(|| std::env::var("CAPACITY").ok()).and_then(|s| s.parse().ok()).unwrap_or(1);
    let active: u64 = opt(&args, "--active").or_else(|| std::env::var("ACTIVE").ok()).and_then(|s| s.parse().ok()).unwrap_or(0);
    let models: Vec<String> = models_csv.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    println!("GPN provider heartbeat on http://{listen} — address={address} gflops={gflops} models={:?} active={active}/{capacity}", models);

    let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    for req in server.incoming_requests() {
        let body = match (req.method(), req.url()) {
            (Method::Get, "/status") => json!({
                "address": address, "gflops": gflops, "models": models,
                "active": active, "capacity": capacity.max(1)
            }),
            (Method::Get, "/health") => json!({"status":"ok"}),
            _ => json!({"error":"not found"}),
        };
        let _ = req.respond(Response::from_string(body.to_string()).with_header(hdr.clone()));
    }
    Ok(())
}
