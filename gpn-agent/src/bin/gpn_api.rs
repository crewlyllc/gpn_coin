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
//!   POST /providers/stake             → deposit attestation stake (JSON body)
//!   POST /providers/attest            → submit GPU attestation (JSON body)
//!   POST /providers/withdraw          → withdraw stake (removes attestation)
//!
//! POST /providers/stake body:
//!   { "amount_gpn": 1000 }            // stake in whole GPN (converted to base units)
//!
//! POST /providers/attest body:
//!   {
//!     "gpu_count": 1,
//!     "compute_gflops": 831,         // measured FP32 GFLOPS (run gpn_gpu_compute)
//!     "gpu_info": "RTX 4060 8GB"     // free-text; sha256'd into gpu_info_hash
//!   }
//!   The challenge_response is computed off-chain as sha256(challenge || gpu_info_hash).
//!   NOTE: the pallet stores this response without on-chain verification; real
//!   challenge-response GPU proof is a separate hardening concern.
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
    println!("  endpoints: /health /block /feed /account/{{addr}} /jobs /job/{{id}} /jobs/claimable /provider/{{addr}} | POST /jobs /jobs/{{id}}/accept|start|complete /providers/register|stake|attest|withdraw");

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
            let att = rt.block_on(client.get_attestation_record(addr));
            let challenge = match rt.block_on(client.get_attestation_challenge()) {
                Ok(c) => c,
                Err(_) => [0u8; 32],
            };
            let (gpu_count, gflops, tier) = att.unwrap_or((0, 0, "Unattested".to_string()));
            json_ok(&serde_json::json!({
                "address": addr,
                "provider_registered": reg,
                "staked": staked,
                "attested": attested,
                "tier": tier,
                "gpu_count": gpu_count,
                "compute_gflops": gflops,
                "challenge_hex": hex::encode(challenge),
            }))
        }
        // ---- jobs ----
        ("GET", ["job", id]) => {
            let rt = rt_handle;
            let id: u64 = id.parse().unwrap_or(0);
            match rt.block_on(client.get_job_state(id)) {
                Some((state, price, requestor, provider)) => {
                    // Enrich with off-chain LLM payload if present.
                    let llm = gpn_agent::llm::get_job(id);
                    json_ok(&serde_json::json!({
                        "id": id,
                        "state": state,
                        "price_base": price,
                        "requestor": requestor,
                        "provider": provider,
                        "llm": llm.map(|j| serde_json::json!({
                            "model": j.model,
                            "prompt": j.prompt,
                            "temperature": j.temperature,
                            "max_tokens": j.max_tokens,
                            "status": j.status,
                            "result_hash": j.result_hash,
                            "error": j.error,
                        })),
                    }))
                }
                None => json_err(404, "job not found"),
            }
        }
        // ---- off-chain LLM result lookup ----
        ("GET", ["job", id, "result"]) => {
            let id: u64 = id.parse().unwrap_or(0);
            match gpn_agent::llm::get_job(id) {
                Some(j) => json_ok(&serde_json::json!({
                    "job_id": id,
                    "status": j.status,
                    "model": j.model,
                    "response": j.response,
                    "result_hash": j.result_hash,
                    "error": j.error,
                })),
                None => json_err(404, "no off-chain LLM payload for this job (not an LLM job?)"),
            }
        }
        // ---- generated artifact (image/video file) ----
        ("GET", ["job", id, "artifact"]) => {
            let id: u64 = id.parse().unwrap_or(0);
            let job = match gpn_agent::llm::get_job(id) {
                Some(j) => j,
                None => return json_err(404, "no off-chain payload for this job"),
            };
            let path = match &job.artifact_path {
                Some(p) => p.clone(),
                None => return json_err(404, &format!("artifact not ready yet (status={})", job.status)),
            };
            let media = job.artifact_media_type.unwrap_or_else(|| "application/octet-stream".to_string());
            match std::fs::read(&path) {
                Ok(bytes) => Response::from_data(bytes)
                    .with_status_code(200)
                    .with_header(Header::from_bytes(&b"Content-Type"[..], media.as_bytes()).unwrap()),
                Err(e) => json_err(500, &format!("cannot read artifact {}: {}", path, e)),
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
            let job_type = v.get("job_type").and_then(|x| x.as_str()).unwrap_or("LLMInference");

            // Bridge: for any generative job (LLM / image / video) with a prompt,
            // capture the model/prompt/params in the off-chain store; the chain
            // only carries the workload hash.
            let prompt = v.get("prompt").and_then(|x| x.as_str()).map(|s| s.to_string());
            let model = v.get("model").and_then(|x| x.as_str()).map(|s| s.to_string());
            let is_llm = job_type.eq_ignore_ascii_case("LLMInference");
            let is_gen = (job_type.eq_ignore_ascii_case("ImageGeneration")
                || job_type.eq_ignore_ascii_case("VideoGeneration"))
                && prompt.is_some();
            // Fine-tuning: job carries a dataset (JSON array of {prompt,completion}
            // OR a pre-serialized string) instead of a single prompt.
            let dataset: Option<String> = match v.get("dataset") {
                Some(sv) if sv.is_string() => sv.as_str().map(|s| s.to_string()),
                Some(av) if av.is_array() => serde_json::to_string(av).ok(),
                _ => None,
            };
            let is_ft = job_type.eq_ignore_ascii_case("FineTuning") && dataset.is_some();
            let lora_r: u32 = v.get("lora_r").and_then(|x| x.as_u64()).unwrap_or(16) as u32;
            let ft_epochs: u32 = v.get("epochs").and_then(|x| x.as_u64()).unwrap_or(2) as u32;
            let is_bridged = (is_llm || is_gen) && prompt.is_some() || is_ft;
            let temperature: f32 = v.get("temperature").and_then(|x| x.as_f64()).unwrap_or(0.7) as f32;
            let max_tokens: u32 = v.get("max_tokens").and_then(|x| x.as_u64()).unwrap_or(256) as u32;
            let gen_kind = if job_type.eq_ignore_ascii_case("ImageGeneration") { "image" } else { "video" };
            let width: u32 = v.get("width").and_then(|x| x.as_u64()).unwrap_or(512) as u32;
            let height: u32 = v.get("height").and_then(|x| x.as_u64()).unwrap_or(512) as u32;
            let steps: u32 = v.get("steps").and_then(|x| x.as_u64()).unwrap_or(30) as u32;
            let seed = v.get("seed").and_then(|x| x.as_i64());
            let frames: u32 = v.get("frames").and_then(|x| x.as_u64()).unwrap_or(16) as u32;
            let fps: u32 = v.get("fps").and_then(|x| x.as_u64()).unwrap_or(8) as u32;
            let negative = v.get("negative_prompt").and_then(|x| x.as_str());
            let default_model = if is_gen { "sd-turbo" } else if is_ft { "HuggingFaceTB/SmolLM2-135M-Instruct" } else { "qwen2.5:1.5b" };
            let model = model.unwrap_or_else(|| default_model.to_string());

            let hash_src: String = if is_bridged {
                if is_ft { dataset.clone().unwrap() } else { prompt.clone().unwrap() }
            } else { workload.to_string() };
            let workload_hash = hash_workload(&hash_src);

            let rt = rt_handle;
            let res = rt.block_on(client_signed.create_job(
                g("min_vram_mb", 8192) as u32,
                g("min_compute_capability", 89) as u16,
                g("min_gpu_count", 1) as u8,
                g("min_cuda_cores", 3072) as u32,
                g("min_gflops", 800),
                job_type,
                v.get("tier").and_then(|x| x.as_str()).unwrap_or("Spot"),
                price_base,
                g("max_duration", 1000) as u32,
                workload_hash,
            ));
            match res {
                Ok(_) => {
                    let next = rt.block_on(client.get_next_job_id());
                    let job_id = next.saturating_sub(1);
                    // Store the off-chain payload keyed by on-chain job id.
                    let job = if is_ft {
                        dataset.as_ref().map(|d| {
                            gpn_agent::llm::LlmJob::new_finetune(&model, d, lora_r, ft_epochs)
                        })
                    } else {
                        prompt.as_ref().map(|p| {
                            if is_llm {
                                gpn_agent::llm::LlmJob::new_llm(&model, p, temperature, max_tokens)
                            } else if is_gen {
                                gpn_agent::llm::LlmJob::new_gen(
                                    gen_kind, &model, p, negative, width, height, steps, seed, frames, fps,
                                )
                            } else {
                                gpn_agent::llm::LlmJob::new_llm(&model, p, temperature, max_tokens)
                            }
                        })
                    };
                    if let Some(job) = job {
                        let _ = gpn_agent::llm::put_job(job_id, job);
                    }
                    json_ok(&serde_json::json!({"ok": true, "job_id": job_id, "price_gpn": price_gpn, "bridged": is_bridged}))
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
        // ---- attestation onboarding (signed) ----
        ("POST", ["providers", "stake"]) => {
            let body = read_body(req);
            let v: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_err(400, &format!("bad JSON: {}", e)),
            };
            let amount_gpn: f64 = v.get("amount_gpn").and_then(|x| x.as_f64()).unwrap_or(1000.0);
            let amount_base: u128 = (amount_gpn * GPN_UNITS as f64).round() as u128;
            let rt = rt_handle;
            let res = rt.block_on(client_signed.deposit_stake(amount_base));
            match res {
                Ok(_) => json_ok(&serde_json::json!({"ok": true, "staked_gpn": amount_gpn})),
                Err(e) => json_err(500, &format!("{}", e)),
            }
        }
        ("POST", ["providers", "attest"]) => {
            let body = read_body(req);
            let v: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_err(400, &format!("bad JSON: {}", e)),
            };
            let gpu_count: u8 = v.get("gpu_count").and_then(|x| x.as_u64()).unwrap_or(1) as u8;
            let compute_gflops: u64 = v.get("compute_gflops").and_then(|x| x.as_u64()).unwrap_or(0);
            let gpu_info = v.get("gpu_info").and_then(|x| x.as_str()).unwrap_or("gpn-gpu");
            if compute_gflops == 0 {
                return json_err(400, "compute_gflops is required (run gpn_gpu_compute to measure it)");
            }
            let rt = rt_handle;
            // gpu_info_hash = sha256(gpu_info); challenge_response = sha256(challenge || gpu_info_hash).
            let gpu_info_hash = sha256(gpu_info.as_bytes());
            let challenge = match rt.block_on(client.get_attestation_challenge()) {
                Ok(c) => c,
                Err(_) => [0u8; 32],
            };
            let mut seed = Vec::with_capacity(64);
            seed.extend_from_slice(&challenge);
            seed.extend_from_slice(&gpu_info_hash);
            let challenge_response = sha256(&seed);
            let res = rt.block_on(client_signed.submit_attestation(
                challenge_response, gpu_info_hash, gpu_count, compute_gflops,
            ));
            match res {
                Ok(_) => json_ok(&serde_json::json!({
                    "ok": true, "gpu_count": gpu_count, "compute_gflops": compute_gflops,
                    "gpu_info_hash_hex": hex::encode(gpu_info_hash),
                })),
                Err(e) => json_err(500, &format!("{}", e)),
            }
        }
        ("POST", ["providers", "withdraw"]) => {
            let rt = rt_handle;
            let res = rt.block_on(client_signed.withdraw_stake());
            match res {
                Ok(_) => json_ok(&serde_json::json!({"ok": true, "stake_withdrawn": true})),
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

/// sha2-256 over arbitrary bytes, into a 32-byte array. Used for gpu_info_hash
/// and the (off-chain computed) challenge_response in the attestation flow.
fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}
