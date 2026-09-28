//! gpn_scheduler — an GPN **access point**: a public job API that ROUTES work to
//! the best provider instead of making callers know provider IPs.
//!
//! A caller hits one URL (`POST /jobs`); the scheduler:
//!   1. filters providers by COMPATIBILITY (attested GFLOPS >= job min_gflops),
//!   2. scores the survivors by LATENCY + LOAD (from each provider's /status heartbeat),
//!   3. picks the best, creates the on-chain job (escrow), and DISPATCHES it to that
//!      provider (via a shared assignment store the provider polls).
//!
//! Multiple access points can run — they share the on-chain state; the assignment
//! store is the only shared off-chain bit. Any caller can use any access point.
//!
//! Usage:
//!   gpn_scheduler --listen <ip:port> --signer-uri <seed> [--endpoint <ws>]
//!                 [--providers <url,url,...>] [--assign-store <path>]
//! Env: LISTEN, SIGNER_URI, ENDPOINT, PROVIDERS, GPN_ASSIGN_STORE

use gpn_agent::chain::ChainClient;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Response, Server};

/// A known provider endpoint + its last-seen health.
#[derive(Clone)]
struct Provider {
    status_url: String,
    address: String,
    gflops: u64,
    models: Vec<String>,
    active: u64,
    capacity: u64,
    latency_ms: u64,
    last_seen: Option<Instant>,
}

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn jpost(url: &str) -> Option<Value> {
    // Allow `--providers http://host:port` (append /status) or a full URL.
    let url = if url.contains("/status") {
        url.to_string()
    } else {
        format!("{}/status", url.trim_end_matches('/'))
    };
    let t0 = Instant::now();
    let body = reqwest::blocking::Client::new()
        .get(&url)
        .timeout(Duration::from_millis(2000))
        .send()
        .ok()?
        .text()
        .ok()?;
    let v: Value = serde_json::from_str(&body).ok()?;
    let mut v = v;
    v["__latency_ms"] = json!(t0.elapsed().as_millis() as u64);
    Some(v)
}

/// Poll every provider's /status into the shared registry.
fn refresh(registry: &Arc<Mutex<Vec<Provider>>>, urls: &Arc<Vec<String>>) {
    let reg = registry.clone();
    let urls = urls.clone();
    std::thread::spawn(move || loop {
        let mut out = Vec::new();
        for u in urls.iter() {
            let v = jpost(u);
            let p = match v {
                Some(v) => Provider {
                    status_url: u.clone(),
                    address: v.get("address").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    gflops: v.get("gflops").and_then(|x| x.as_u64()).unwrap_or(0),
                    models: v
                        .get("models")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                    active: v.get("active").and_then(|x| x.as_u64()).unwrap_or(0),
                    capacity: v.get("capacity").and_then(|x| x.as_u64()).unwrap_or(1).max(1),
                    latency_ms: v.get("__latency_ms").and_then(|x| x.as_u64()).unwrap_or(9999),
                    last_seen: Some(Instant::now()),
                },
                None => Provider {
                    status_url: u.clone(),
                    address: String::new(),
                    gflops: 0,
                    models: vec![],
                    active: 0,
                    capacity: 0,
                    latency_ms: 9999,
                    last_seen: None,
                },
            };
            out.push(p);
        }
        *reg.lock().unwrap() = out;
        std::thread::sleep(Duration::from_secs(5));
    });
}

fn pick(registry: &[Provider], min_gflops: u64, model: &str) -> Option<Provider> {
    // 1. Compatibility: gflops >= min, model present, and it has free capacity.
    let mut candidates: Vec<&Provider> = registry
        .iter()
        .filter(|p| p.last_seen.is_some())
        .filter(|p| p.gflops >= min_gflops)
        .filter(|p| model.is_empty() || p.models.iter().any(|m| m == model))
        .filter(|p| p.active < p.capacity)
        .collect();
    if candidates.is_empty() {
        // relax the capacity filter
        candidates = registry
            .iter()
            .filter(|p| p.last_seen.is_some() && p.gflops >= min_gflops)
            .collect();
    }
    // 2. Score: prefer least-loaded, then lowest latency.
    candidates.sort_by_key(|p| (p.active * 1000 + p.latency_ms / 10));
    candidates.first().map(|p| (*p).clone())
}

fn write_assignment(store: &str, job_id: u64, provider: &str) -> std::io::Result<()> {
    let mut m: serde_json::Map<String, Value> = std::fs::read_to_string(store)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    m.insert(job_id.to_string(), json!(provider));
    std::fs::write(store, serde_json::to_string_pretty(&Value::Object(m))?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let listen = opt(&args, "--listen")
        .or_else(|| std::env::var("LISTEN").ok())
        .unwrap_or_else(|| "0.0.0.0:8092".to_string());
    let signer_uri = opt(&args, "--signer-uri")
        .or_else(|| std::env::var("SIGNER_URI").ok())
        .unwrap_or_else(|| "//Alice".to_string());
    let endpoint = opt(&args, "--endpoint")
        .or_else(|| std::env::var("ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let providers_csv = opt(&args, "--providers")
        .or_else(|| std::env::var("PROVIDERS").ok())
        .unwrap_or_default();
    let assign_store = opt(&args, "--assign-store")
        .or_else(|| std::env::var("GPN_ASSIGN_STORE").ok())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            format!("{home}/.gpn/assignments.json")
        });
    let urls: Vec<String> = providers_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Build the chain client (synchronous runtime for tiny_http handler).
    let rt = tokio::runtime::Runtime::new()?;
    let client = Arc::new(rt.block_on(ChainClient::new(&endpoint))?.with_signer(&signer_uri));

    let registry = Arc::new(Mutex::new(Vec::<Provider>::new()));
    refresh(&registry, &Arc::new(urls.clone()));

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    println!("GPN access point (scheduler) listening on http://{listen}");
    println!("  chain: {endpoint}");
    println!("  providers: {} ({})", urls.len(), urls.join(", "));
    println!("  assign store: {assign_store}");
    println!("  endpoints: /health /providers /jobs /job/{{id}} | POST /jobs");

    let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        let method = req.method().clone();
        let mut body = String::new();
        let _ = req.as_reader().read_to_string(&mut body);
        let path: Vec<&str> = url.trim_start_matches('/').split('/').filter(|s| !s.is_empty()).collect();

        let resp = match (&method, path.as_slice()) {
            (Method::Get, ["health"]) => {
                let head = rt.block_on(async { 0 });
                let _ = head;
                json!({"status":"ok","providers": registry.lock().unwrap().len()})
            }
            (Method::Get, ["providers"]) => {
                let reg = registry.lock().unwrap();
                json!({ "providers": reg.iter().map(|p| json!({
                    "status_url": p.status_url, "gflops": p.gflops, "models": p.models,
                    "active": p.active, "capacity": p.capacity, "latency_ms": p.latency_ms,
                    "healthy": p.last_seen.is_some()
                })).collect::<Vec<_>>() })
            }
            (Method::Post, ["jobs"]) => {
                let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let g = |k: &str, d: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d);
                let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("llama3.2:1b").to_string();
                let min_gflops = g("min_gflops", 150);
                let price_gpn = v.get("price_gpn").and_then(|x| x.as_f64()).unwrap_or(10.0);
                let job_type = v.get("job_type").and_then(|x| x.as_str()).unwrap_or("LLMInference");
                let prompt = v.get("prompt").and_then(|x| x.as_str()).map(|s| s.to_string());

                // choose the best provider
                let reg = registry.lock().unwrap().clone();
                let chosen = pick(&reg, min_gflops, "");
                let provider = match chosen {
                    Some(p) => p,
                    None => {
                        let r = Response::from_string(json!({"ok":false,"error":"no compatible provider available"}).to_string())
                            .with_header(hdr.clone()).with_status_code(503);
                        let _ = req.respond(r);
                        continue;
                    }
                };

                // create the on-chain job (escrow) as the access-point signer
                // workload hash must be non-zero (marketplace rejects all-zeros)
                let workload_hash = {
                    use sha2::{Digest, Sha256};
                    let mut h = Sha256::new();
                    h.update(prompt.as_deref().unwrap_or("").as_bytes());
                    h.update(model.as_bytes());
                    let mut o = [0u8; 32];
                    o.copy_from_slice(&h.finalize());
                    o
                };
                let res = rt.block_on(client.create_job(
                    g("min_vram_mb", 8192) as u32,
                    g("min_compute_capability", 75) as u16,
                    g("min_gpu_count", 1) as u8,
                    g("min_cuda_cores", 0) as u32,
                    min_gflops, job_type, "Spot",
                    (price_gpn * 1e12) as u128, 1000, workload_hash,
                ));
                match res {
                    Ok(_) => {
                        let next = rt.block_on(client.get_next_job_id());
                        let id = next.saturating_sub(1);
                        // store the off-chain payload (prompt/model) if present
                        if let Some(p) = prompt.as_ref() {
                            let job = gpn_agent::llm::LlmJob::new_llm(&model, p, 0.7, 256);
                            let _ = gpn_agent::llm::LlmStore::new().put(id, job);
                        }
                        // dispatch: record which provider (by address) should claim it
                        let _ = write_assignment(&assign_store, id, &provider.address);
                        json!({"ok":true,"job_id":id,"routed_to":provider.address,
                               "status_url":provider.status_url,
                               "latency_ms":provider.latency_ms,"load":format!("{}/{}",provider.active,provider.capacity)})
                    }
                    Err(e) => json!({"ok":false,"error":format!("{e}")}),
                }
            }
            (Method::Get, ["jobs"]) => {
                let next = rt.block_on(client.get_next_job_id());
                let mut jobs = vec![];
                for id in 0..next {
                    if let Some((state, _p, _r, prov)) = rt.block_on(client.get_job_state(id)) {
                        jobs.push(json!({"id":id,"state":state,"provider":prov}));
                    }
                }
                json!({"jobs":jobs})
            }
            (Method::Get, ["job", id]) => {
                let idn: u64 = id.parse().unwrap_or(0);
                match rt.block_on(client.get_job_state(idn)) {
                    Some((state, price, req_, prov)) => json!({"id":idn,"state":state,"price_base":price.to_string(),"requestor":req_,"provider":prov}),
                    None => json!({"error":"not found"}),
                }
            }
            _ => json!({"error":"not found"}),
        };

        let code = if resp.get("error").is_some() { 404 } else { 200 };
        let _ = req.respond(Response::from_string(resp.to_string()).with_header(hdr.clone()).with_status_code(code));
    }
    Ok(())
}
