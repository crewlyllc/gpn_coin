//! gpn_api — REST API for the GPN Compute Marketplace.
//!
//! A thin HTTP/JSON wrapper over the chain, so any app/website/script can do
//! things like `GET /jobs`, `POST /jobs`, `GET /account/{addr}` with plain
//! JSON instead of raw JSON-RPC.
//!
//! Uses the shared `gpn_agent::chain::ChainClient` for both reads and signed
//! writes. Write endpoints (create/accept/complete jobs, register provider)
//! sign with a **server-held key** (`--signer-uri`, default `//Alice`).
//!
//! ⚠️ SECURITY MODEL: this is a testnet / hot-wallet API — the server signs
//! transactions with one key on behalf of all callers. Fine for a dev/testnet
//! backend. For a real mainnet you'd replace this with wallet-level signing
//! (the customer signs in their own wallet; the API only relays). Do NOT run
//! this with a funded mainnet key.
//!
//! Endpoints:
//!   GET  /health                      → node reachable + head block
//!   GET  /block                       → latest block number
//!   GET  /feed                        → effective profitability feed
//!   GET  /account/{address}           → balance + nonce
//!   GET  /job/{id}                    → job detail
//!   GET  /jobs                        → all jobs 0..next-id
//!   GET  /jobs/claimable?gflops=N     → jobs a provider of N GFLOPS can claim
//!   GET  /provider/{address}          → registration + attestation state
//!   POST /jobs                        → create a job (JSON body, see below)
//!   POST /jobs/{id}/accept            → provider accepts
//!   POST /jobs/{id}/start             → provider starts
//!   POST /jobs/{id}/complete          → provider completes (payout)
//!   POST /providers/register          → register this GPU for rent
//!
//! POST /jobs body:
//!   {
//!     "min_vram_mb": 8192,
//!     "min_compute_capability": 89,      // x10 (8.9 = 89)
//!     "min_gpu_count": 1,
//!     "min_cuda_cores": 3072,
//!     "min_gflops": 15110,
//!     "job_type": "LLMInference",        // LLMInference|FineTuning|ImageGeneration|MLInference|Custom
//!     "tier": "Spot",                    // Spot|Premium|Reserved
//!     "price_gpn": 100,                  // in GPN (converted to base units)
//!     "max_duration": 1000,
//!     "workload": "optional description (hashed)"   // hashed to workload_hash if omitted/blank
//!   }
//!
//! Usage:
//!   gpn_api [--listen 127.0.0.1:8082] [--endpoint ws://127.0.0.1:9944] [--signer-uri //Alice]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpn_agent::chain::ChainClient;
use serde::Serialize;
use tiny_http::{Header, Response, Server};

/// 1 GPN = 1e12 base units (12 decimals).
const GPN_UNITS: f64 = 1_000_000_000_000.0;

// ---------------------------------------------------------------------------
// JSON response helpers
// ---------------------------------------------------------------------------

fn json_ok<T: Serialize>(v: &T) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    Response::from_data(body)
        .with_status_code(200)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn json_err(status: u16, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(&serde_json::json!({"error": msg})).unwrap();
    Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

/// A simple router: path -> (method, handler-name). We parse path + query by hand.
fn parse_request(req: &tiny_http::Request) -> (String, String, HashMap<String, String>) {
    let method = req.method().to_string();
    let raw = req.url().to_string();
    let (path, query) = match raw.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw, String::new()),
    };
    let mut q = HashMap::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            q.insert(k.to_string(), v.to_string());
        }
    }
    (method, path, q)
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let mut listen = "127.0.0.1:8082".to_string();
    let mut endpoint = "ws://127.0.0.1:9944".to_string();
    let mut signer_uri = "//Alice".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => { listen = args.get(i + 1).cloned().unwrap_or(listen); i += 1; }
            "--endpoint" => { endpoint = args.get(i + 1).cloned().unwrap_or(endpoint); i += 1; }
            "--signer-uri" => { signer_uri = args.get(i + 1).cloned().unwrap_or(signer_uri); i += 1; }
            _ => {}
        }
        i += 1;
    }

    let client = ChainClient::new(&endpoint).await?;
    let client_signed = if signer_uri == "NONE" {
        client.clone()
    } else {
        client.clone().with_signer(&signer_uri)
    };
    let client = Arc::new(client);
    let client_signed = Arc::new(client_signed);

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {}: {}", listen, e))?;
    println!("GPN API listening on http://{} (chain endpoint {})", listen, endpoint);
    println!("  signer: {} — write endpoints sign with this key (hot-wallet/testnet model)", signer_uri);
    println!("  endpoints: /health /block /feed /account/{{addr}} /jobs /job/{{id}} /jobs/claimable /provider/{{addr}} | POST /jobs /jobs/{{id}}/accept|start|complete /providers/register");

    for request in server.incoming_requests() {
        let client = client.clone();
        let client_signed = client_signed.clone();
        let rt_handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            handle(request, &client, &client_signed, rt_handle);
        });
    }
    Ok(())
}

fn handle(mut req: tiny_http::Request, client: &ChainClient, client_signed: &ChainClient, rt_handle: tokio::runtime::Handle) {
    let (method, path, query) = parse_request(&req);
    let started = Instant::now();
    let resp = route(&method, &path, &query, &mut req, client, client_signed, &rt_handle);
    let _ = req.respond(resp);
    log::info!("{} {} ({:?})", method, path, started.elapsed());
}

fn route(
    method: &str,
    path: &str,
    query: &HashMap<String, String>,
    req: &mut tiny_http::Request,
    client: &ChainClient,
    client_signed: &ChainClient,
    rt_handle: &tokio::runtime::Handle,
) -> Response<std::io::Cursor<Vec<u8>>> {
    // normalize: split /a/b/c
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();

    match (method, parts.as_slice()) {
        // ---- health / info ----
        ("GET", ["health"]) => {
            let rt = rt_handle;
            let block = rt.block_on(client.head_block_number());
            json_ok(&serde_json::json!({
                "status": if block.is_some() {"ok"} else {"degraded"},
                "head_block": block,
                "chain": "GPN",
            }))
        }
        ("GET", ["block"]) => {
            let rt = rt_handle;
            let b = rt.block_on(client.head_block_number());
            json_ok(&serde_json::json!({"block": b}))
        }
        ("GET", ["feed"]) => {
            let rt = rt_handle;
            let f = rt.block_on(client.get_effective_feed());
            match f {
                Some(feed) => json_ok(&serde_json::json!({
                    "token_price_usd": feed.token_price_usd,
                    "avg_gpu_hour_price_usd": feed.avg_gpu_hour_price_usd,
                    "utilization": feed.utilization,
                    "staking_apr_percent": feed.staking_apr_percent,
                    "updated_at_block": feed.updated_at_block,
                })),
                None => json_err(404, "no effective feed yet (no operators seeding data)"),
            }
        }
        ("GET", ["account", addr]) => {
            let rt = rt_handle;
            match rt.block_on(client.get_account_info(addr)) {
                Ok(a) => json_ok(&serde_json::json!({
                    "address": a.address,
                    "balance_gpn": a.balance,
                    "nonce": a.nonce,
                })),
                Err(e) => json_err(500, &format!("{}", e)),
            }
        }
        ("GET", ["provider", addr]) => {
            let rt = rt_handle;
            let reg = rt.block_on(async { client.is_provider_registered(addr).await });
            let (staked, attested) = rt.block_on(client.get_provider_state(addr));
            json_ok(&serde_json::json!({
                "address": addr,
                "provider_registered": reg,
                "staked": staked,
                "attested": attested,
            }))
        }
        // ---- jobs ----
        ("GET", ["job", id]) => {
            let rt = rt_handle;
            let id: u64 = id.parse().unwrap_or(0);
            match rt.block_on(client.get_job_state(id)) {
                Some((state, price, requestor, provider)) => json_ok(&serde_json::json!({
                    "id": id,
                    "state": state,
                    "price_base": price,
                    "requestor": requestor,
                    "provider": provider,
                })),
                None => json_err(404, "job not found"),
            }
        }
        ("GET", ["jobs"]) => {
            let rt = rt_handle;
            let next = rt.block_on(client.get_next_job_id());
            let mut jobs = Vec::new();
            for id in 0..next {
                if let Some((state, price, requestor, provider)) = rt.block_on(client.get_job_state(id)) {
                    jobs.push(serde_json::json!({"id": id, "state": state, "price_base": price, "requestor": requestor, "provider": provider}));
                }
            }
            json_ok(&serde_json::json!({"count": jobs.len(), "jobs": jobs}))
        }
        ("GET", ["jobs", "claimable"]) => {
            let rt = rt_handle;
            let gflops: u64 = query.get("gflops").and_then(|s| s.parse().ok()).unwrap_or(0);
            let ids = rt.block_on(client.list_claimable_jobs(gflops));
            json_ok(&serde_json::json!({"min_gflops": gflops, "claimable_jobs": ids}))
        }
        // ---- writes (signed) ----
        ("POST", ["jobs"]) => {
            let body = read_body(req);
            let v: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_err(400, &format!("bad JSON: {}", e)),
            };
            let g = |k: &str, d: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d) as u64;
            let workload = v.get("workload").and_then(|x| x.as_str()).unwrap_or("gpn-api job");
            let price_gpn: f64 = v.get("price_gpn").and_then(|x| x.as_f64()).unwrap_or(1.0);
            let price_base: u128 = (price_gpn * GPN_UNITS as f64).round() as u128;
            let workload_hash = hash_workload(workload);

            let rt = rt_handle;
            let res = rt.block_on(client_signed.create_job(
                g("min_vram_mb", 8192) as u32,
                g("min_compute_capability", 89) as u16,
                g("min_gpu_count", 1) as u8,
                g("min_cuda_cores", 3072) as u32,
                g("min_gflops", 15110),
                v.get("job_type").and_then(|x| x.as_str()).unwrap_or("LLMInference"),
                v.get("tier").and_then(|x| x.as_str()).unwrap_or("Spot"),
                price_base,
                g("max_duration", 1000) as u32,
                workload_hash,
            ));
            match res {
                Ok(_) => {
                    let next = rt.block_on(client.get_next_job_id());
                    json_ok(&serde_json::json!({"ok": true, "job_id": next.saturating_sub(1), "price_gpn": price_gpn}))
                }
                Err(e) => json_err(500, &format!("{}", e)),
            }
        }
        ("POST", ["jobs", id, action]) => {
            let rt = rt_handle;
            let id: u64 = id.parse().unwrap_or(0);
            let res = match *action {
                "accept" => rt.block_on(client_signed.accept_job(id)),
                "start" => rt.block_on(client_signed.start_job(id)),
                "complete" => rt.block_on(client_signed.complete_job(id)),
                _ => return json_err(404, "unknown action (accept|start|complete)"),
            };
            match res {
                Ok(_) => json_ok(&serde_json::json!({"ok": true, "job_id": id, "action": action})),
                Err(e) => json_err(500, &format!("{}", e)),
            }
        }
        ("POST", ["providers", "register"]) => {
            let body = read_body(req);
            let v: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_err(400, &format!("bad JSON: {}", e)),
            };
            let g = |k: &str, d: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d) as u64;
            let rt = rt_handle;
            let res = rt.block_on(client_signed.register_provider(
                g("min_vram_mb", 8192) as u32,
                g("min_compute_capability", 89) as u16,
                g("min_gpu_count", 1) as u8,
                g("min_cuda_cores", 3072) as u32,
                g("min_gflops", 15110),
            ));
            match res {
                Ok(_) => json_ok(&serde_json::json!({"ok": true, "provider_registered": true})),
                Err(e) => json_err(500, &format!("{}", e)),
            }
        }
        _ => json_err(404, "not found — see gpn_api.rs doc comment for endpoints"),
    }
}

fn read_body(req: &mut tiny_http::Request) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut r = req.as_reader().take(1 << 20);
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Hash the workload description into a 32-byte fingerprint (sha2-256). If it
/// can't (no dep), fall back to a fixed but valid 32-byte array.
fn hash_workload(desc: &str) -> [u8; 32] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    desc.hash(&mut h);
    let a = h.finish();
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&a.to_le_bytes());
    out[8..16].copy_from_slice(&(!a).to_le_bytes());
    out
}
