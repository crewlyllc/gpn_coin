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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Response, Server};

/// Job ids this AP created recently, with creation time. Used by the stale-job
/// janitor to reclaim its own orphaned `Pending` jobs, which otherwise hold a
/// global `MaxActiveJobs` slot forever (there is no on-chain expiry — see
/// artifacts/project7/job-lifecycle-gap-analysis-2026-10-05.md).
fn recent_jobs() -> &'static Mutex<Vec<(u64, Instant, u64)>> {
    static R: OnceLock<Mutex<Vec<(u64, Instant, u64)>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

fn record_created(id: u64, max_tokens: u64) {
    recent_jobs().lock().unwrap().push((id, Instant::now(), max_tokens));
}

/// Policy for how long a job may stay `Pending` before the janitor reclaims it.
/// Scales with the requested output size (a job that asked for more tokens may
/// legitimately take longer), clamped to `[MIN, MAX]` so tiny jobs are reclaimed
/// promptly and huge ones can't hold a slot unboundedly. The throughput floor is
/// deliberately conservative (a slow provider is the case we're guarding against).
const JANITOR_BASE_SECS: u64 = 60; // claim→accept→start→finality overhead + slack
const JANITOR_TOK_PER_S: u64 = 20; // conservative provider throughput floor
const JANITOR_SLACK: u64 = 2; // 2× headroom
const JANITOR_MIN_SECS: u64 = 120; // never reclaim faster than this
const JANITOR_MAX_SECS: u64 = 900; // but never wait longer than this
fn stale_secs(max_tokens: u64) -> u64 {
    let gen = (max_tokens / JANITOR_TOK_PER_S.max(1)) * JANITOR_SLACK;
    (JANITOR_BASE_SECS + gen).clamp(JANITOR_MIN_SECS, JANITOR_MAX_SECS)
}

/// Every 30s, cancel our own `Pending` jobs that have sat Pending longer than
/// [`stale_secs`] (token-scaled). Cancels only Pending (on-chain guard);
/// advanced/terminal jobs are dropped from tracking. Uses the shared nonce
/// stream (`cancel_job_fast`) so it can't desync request-path txs.
fn spawn_janitor(client: Arc<ChainClient>, rt: Arc<tokio::runtime::Runtime>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let now = Instant::now();
        let mut due: Vec<(u64, u64)> = Vec::new();
        {
            let mut g = recent_jobs().lock().unwrap();
            let mut keep: Vec<(u64, Instant, u64)> = Vec::with_capacity(g.len());
            for (id, t, mt) in g.drain(..) {
                if now.duration_since(t).as_secs() > stale_secs(mt) {
                    due.push((id, mt));
                } else {
                    keep.push((id, t, mt));
                }
            }
            *g = keep;
        }
        for (id, mt) in due {
            match rt.block_on(client.get_job_state(id)) {
                Some((state, _, _, _)) if state == "Pending" => {
                    match rt.block_on(client.cancel_job_fast(id)) {
                        Ok(()) => println!(
                            "[janitor] cancelled stale Pending #{id} (max_tokens {mt}, waited >{}s)",
                            stale_secs(mt)
                        ),
                        Err(e) => {
                            eprintln!("[janitor] cancel #{id} failed: {e}");
                            // Retry next tick.
                            record_created(id, mt);
                        }
                    }
                }
                _ => { /* advanced/terminal/gone — stop tracking */ }
            }
        }
    });
}

/// Process-local job-id allocator for the access point's fast path.
///
/// With no-wait `create_job`, the on-chain `NextJobId` counter doesn't advance
/// until the tx lands, so two overlapping requests can read the same counter
/// and both predict the same id. Since the access point is the sole creator of
/// jobs, we hand out ids from a local counter seeded from (and never behind)
/// the chain counter. Fixes store/assignment collisions + provider claim storms.
static NEXT_JOB_ID: Mutex<Option<u64>> = Mutex::new(None);

/// Allocate the next predicted job id. `chain_next` is the current on-chain
/// `NextJobId`; the returned id is `max(local_counter, chain_next)`.
fn alloc_predicted_job_id(chain_next: u64) -> u64 {
    let mut g = NEXT_JOB_ID.lock().unwrap();
    let cur = match *g {
        Some(c) if c >= chain_next => c,
        _ => chain_next,
    };
    *g = Some(cur + 1);
    cur
}

/// Realign the process-local allocator to a known-good on-chain id (never move it
/// backwards). Call after a create is CONFIRMED so the allocator can't stay drifted
/// ahead of the chain (the +1 id-desync bug: a create that didn't land still
/// advanced the local counter, so every later label was one too high).
fn reseed_alloc(next_id: u64) {
    let mut g = NEXT_JOB_ID.lock().unwrap();
    *g = Some(g.unwrap_or(0).max(next_id));
}

/// A known provider endpoint + its last-seen health.
#[derive(Clone)]
struct Provider {
    status_url: String,
    address: String,
    gflops: u64,
    models: Vec<String>,
    can_pull: bool,
    tok_per_s: f64,
    owner_cost_usd_hr: f64,
    mem_bw_gbps: f64,
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
                    can_pull: v.get("can_pull").and_then(|x| x.as_bool()).unwrap_or(false),
                    tok_per_s: v.get("tok_per_s").and_then(|x| x.as_f64()).unwrap_or(40.0),
                    owner_cost_usd_hr: v
                        .get("owner_cost_usd_hr")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.03),
                    mem_bw_gbps: v.get("mem_bw_gbps").and_then(|x| x.as_f64()).unwrap_or(250.0),
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
                    can_pull: false,
                    tok_per_s: 40.0,
                    owner_cost_usd_hr: 0.03,
                    mem_bw_gbps: 250.0,
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

fn has_model(p: &Provider, model: &str) -> bool {
    if model.is_empty() || model == "auto" {
        !p.models.is_empty()
    } else {
        p.models.iter().any(|m| m == model || m.split(':').next() == Some(model))
    }
}

/// Resolve/validate a requested model against the live provider registry.
///
/// Returns the concrete model to enqueue, or an error to **reject** the request.
/// Unknown models are rejected rather than routed to a can-pull provider: a job
/// for a model no provider can actually serve never completes and permanently
/// consumes a global `MaxActiveJobs` slot → eventually `TooManyActiveJobs`
/// blocks ALL job creation (2026-10-05 incident).
fn resolve_requested_model(reg: &[Provider], requested: &str) -> Result<String, String> {
    let m = requested.trim();
    if m.is_empty() || m == "auto" {
        for p in reg {
            if let Some(x) = p.models.first() {
                return Ok(x.clone());
            }
        }
        return Err("no models available from any provider".into());
    }
    if reg.iter().any(|p| has_model(p, m)) {
        Ok(m.to_string())
    } else {
        let mut have: Vec<String> = reg.iter().flat_map(|p| p.models.clone()).collect();
        have.sort();
        have.dedup();
        Err(format!("unknown model '{m}'; available: {have:?}"))
    }
}

/// Job-relative GPN price for an image/video generation job, grounded in the
/// market GPU-hour price from the profitability oracle (mirrors the text
/// per-token pricing). Scales linearly with output pixels × denoise steps, so a
/// bigger image or more steps costs more. Reference: 512×512 @ 8 steps ≈ REF_SECS
/// of GPU time (including one-time model load).
fn image_price_gpn(w: f64, h: f64, steps: f64, token_price_usd: f64, market_hr: f64, util: f64) -> f64 {
    const REF_SECS: f64 = 90.0;
    const MARGIN: f64 = 0.20;
    const REF_UNITS: f64 = 512.0 * 512.0 * 8.0;
    let units = w.max(1.0) * h.max(1.0) * steps.max(1.0);
    let est_secs = (units / REF_UNITS) * REF_SECS;
    let util = util.clamp(0.05, 1.0);
    let usd = (market_hr.max(0.001) / 3600.0) * est_secs * (1.0 + MARGIN) / util;
    let base_units = (usd / token_price_usd.max(1e-9)) * 1e12;
    // Return whole GPN, at least 1 base unit.
    base_units.round().max(1.0) / 1e12
}

/// Model-aware selection. Returns `(provider, needs_pull)`.
///
/// 1. A provider that already HAS the model (or, if none was requested, any provider
///    with at least one model) — fastest + least-loaded.
/// 2. Otherwise a provider that CAN pull it (`can_pull`), which will download the model
///    on demand before running.
///
/// Tie-break (Jeremy's rule): a *puller* can beat a *ready* provider if
/// `est(puller) + pull_penalty < est(ready)` — i.e. downloading the model on a
/// fast/idle machine still finishes sooner than waiting on a slow/busy ready one.
/// Flatten an OpenAI `messages[]` array into a single prompt for the GPN job.
fn messages_to_prompt(v: &Value) -> String {
    let mut s = String::new();
    if let Some(sys) = v.get("system").and_then(|x| x.as_str()) {
        s.push_str(&format!("system: {sys}\n"));
    }
    if let Some(msgs) = v.get("messages").and_then(|x| x.as_array()) {
        for m in msgs {
            let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
            let content = m.get("content").and_then(|x| x.as_str()).unwrap_or("");
            s.push_str(&format!("{role}: {content}\n"));
        }
    }
    s.push_str("assistant:");
    s
}

/// Parse a rough parameter count (in billions) from a model name, e.g.
/// "llama3.2:1b" -> 1.0, "qwen2.5:0.5b" -> 0.5, "llama3:70b" -> 70.0.
fn params_b_for(model: &str) -> f64 {
    let m = model.to_ascii_lowercase();
    if let Some(idx) = m.find('b') {
        let pre = &m[..idx];
        let num: String = pre
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if let Ok(v) = num.parse::<f64>() {
            if v > 0.0 {
                return v;
            }
        }
    }
    1.0
}

/// Roofline estimate of output tokens/sec for `params_b` on a GPU with
/// `gflops` GFLOP/s and `mem_bw_gbps` GB/s of memory bandwidth:
///   compute-bound:  gflops·1e9·eff_c / (2 FLOPs per param per token)
///   memory-bound:   bw·1e9·eff_m    / (2 bytes per fp16 weight per token)
/// Decode is usually memory-bound; prefill/train is compute-bound. We take the min.
fn roofline_tok_per_s(gflops: f64, mem_bw_gbps: f64, params_b: f64) -> f64 {
    const EFF_COMPUTE: f64 = 0.40;
    const EFF_MEM: f64 = 0.70;
    let p = params_b.max(0.05) * 1e9;
    let compute = gflops.max(1.0) * 1e9 * EFF_COMPUTE / (2.0 * p);
    let memory = mem_bw_gbps.max(1.0) * 1e9 * EFF_MEM / (p * 2.0);
    compute.min(memory).max(1.0)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Target USD cost per output token for a model (the *stable* anchor). Uses a
/// ~1B reference at $2e-6/token, scaled by roughly the parameter count parsed
/// from the model name (0.5b/1b/8b/70b…). This is what keeps a job's *dollar*
/// cost stable while the GPN amount floats with the token price.
fn usd_per_token_for(model: &str) -> f64 {
    let base = 2.0e-6;
    let m = model.to_ascii_lowercase();
    let mut params_b = 1.0f64;
    if let Some(idx) = m.find('b') {
        // find the numeric run immediately before the 'b'
        let pre = &m[..idx];
        let num: String = pre
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if let Ok(v) = num.parse::<f64>() {
            if v > 0.0 {
                params_b = v;
            }
        }
    }
    base * params_b.max(0.5)
}

fn pick(registry: &[Provider], min_gflops: u64, model: &str, pull_penalty_ms: u64) -> Option<(Provider, bool)> {
    let live: Vec<&Provider> = registry
        .iter()
        .filter(|p| p.last_seen.is_some() && p.gflops >= min_gflops)
        .collect();

    // Availability: a node currently running an AI job is NOT available (one GPU does one
    // job at a time). Prefer idle nodes, chosen by LOWEST LATENCY (least-ms), not by load.
    let idle: Vec<&Provider> = live.iter().copied().filter(|p| p.active == 0).collect();

    // 1. Idle nodes that already HAVE the model -> lowest latency wins.
    let mut ready: Vec<&Provider> = idle.iter().copied().filter(|p| has_model(p, model)).collect();
    ready.sort_by_key(|p| p.latency_ms);

    // 2. Idle nodes that CAN pull the model -> lowest latency.
    let mut pullers: Vec<&Provider> = idle.iter().copied().filter(|p| p.can_pull).collect();
    pullers.sort_by_key(|p| p.latency_ms);

    match (ready.first().copied(), pullers.first().copied()) {
        // Both exist: a puller wins only if it beats the ready idle node end-to-end
        // (downloading on a fast/idle node still finishes sooner than waiting on a slow one).
        (Some(r), Some(p)) => {
            if p.latency_ms + pull_penalty_ms < r.latency_ms {
                Some((p.clone(), true))
            } else {
                Some((r.clone(), false))
            }
        }
        (Some(r), None) => Some((r.clone(), false)),
        (None, Some(p)) => Some((p.clone(), true)),
        (None, None) => {
            // Every compatible node is busy: no idle node exists. Fall back to queuing on the
            // least-loaded, then lowest-latency node (the job waits; it is not run concurrently).
            let mut busy: Vec<&Provider> = live.iter().copied().filter(|p| has_model(p, model)).collect();
            busy.sort_by_key(|p| p.active * 1000 + p.latency_ms / 10);
            busy.first().map(|p| ((*p).clone(), false))
        }
    }
}

fn write_assignment(store: &str, job_id: u64, provider: &str, model: &str, pull: bool) -> std::io::Result<()> {
    let mut m: serde_json::Map<String, Value> = std::fs::read_to_string(store)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    m.insert(job_id.to_string(), json!({"provider": provider, "model": model, "pull": pull}));
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
    // Optional bearer token. When set, state-changing (POST) + artifact requests
    // from NON-loopback clients must present it (local crons/monitors keep working).
    let ap_token = std::env::var("GPN_AP_TOKEN").unwrap_or_default();
    // Testnet "free AI": the access point's own hot wallet pays the escrow, so a
    // caller can run a job with no wallet at all. On MAINNET this MUST be off —
    // callers sign with their own wallet and the access point only routes.
    let allow_hot_wallet = args.iter().any(|a| a == "--allow-hot-wallet")
        || std::env::var("GPN_ALLOW_HOT_WALLET").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
    // Estimated cost (ms) of downloading a model on demand, used only for the
    // "download + be faster than a ready-but-slow provider" tie-break.
    let pull_penalty_ms: u64 = opt(&args, "--pull-penalty-ms")
        .or_else(|| std::env::var("GPN_PULL_PENALTY_MS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(120_000);
    // Rate card: price the AI in USD (cost of compute + margin), then convert to GPN.
    //   rental-fraction-bps: target = clamp(frac × market_rental, owner_cost, market).
    //                        5000 = 50% of the market rental (Jeremy's rule).
    //   margin-bps:          profit on top of the (target) compute cost.
    let rental_frac: f64 = opt(&args, "--rental-fraction-bps")
        .or_else(|| std::env::var("GPN_RENTAL_FRACTION_BPS").ok())
        .and_then(|s| s.parse::<f64>().ok())
        .map(|b| b / 10_000.0)
        .unwrap_or(0.5);
    let margin: f64 = opt(&args, "--margin-bps")
        .or_else(|| std::env::var("GPN_MARGIN_BPS").ok())
        .and_then(|s| s.parse::<f64>().ok())
        .map(|b| b / 10_000.0)
        .unwrap_or(0.30);
    let urls: Vec<String> = providers_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Build the chain client (synchronous runtime for tiny_http handler).
    let rt = Arc::new(tokio::runtime::Runtime::new()?);
    let client = Arc::new(rt.block_on(ChainClient::new(&endpoint))?.with_signer(&signer_uri));
    spawn_janitor(client.clone(), rt.clone());

    let registry = Arc::new(Mutex::new(Vec::<Provider>::new()));
    refresh(&registry, &Arc::new(urls.clone()));

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    println!("GPN access point (scheduler) listening on http://{listen}");
    println!("  chain: {endpoint}");
    println!("  providers: {} ({})", urls.len(), urls.join(", "));
    println!("  assign store: {assign_store}");
    println!("  endpoints: /health /providers /jobs /job/{{id}} /mode | POST /jobs /route/{{id}}");
    println!(
        "  mode: {} ({})",
        if allow_hot_wallet { "TESTNET / free-AI" } else { "MAINNET / client-signed" },
        if allow_hot_wallet { "access-point hot wallet pays" } else { "callers must sign with their OWN wallet" }
    );

    let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    // Handle each request on its OWN thread. Previously the loop handled requests
    // sequentially, so a single blocking handler (e.g. POST /jobs doing a chain
    // RPC, or the OpenAI facade waiting for a job to finish) would wedge EVERY
    // other endpoint — including GET /health. Threading keeps the AP responsive.
    for mut req in server.incoming_requests() {
        let rt = rt.clone();
        let client = client.clone();
        let registry = registry.clone();
        let assign_store = assign_store.clone();
        let hdr = hdr.clone();
        let ap_token = ap_token.clone();
        std::thread::spawn(move || {
        let url = req.url().to_string();
        let method = req.method().clone();
        let mut body = String::new();
        let _ = req.as_reader().read_to_string(&mut body);
        let path: Vec<&str> = url.trim_start_matches('/').split('/').filter(|s| !s.is_empty()).collect();

        // Auth gate (non-loopback only).
        if !ap_token.is_empty() {
            let is_loopback = req.remote_addr().map(|a| a.ip().is_loopback()).unwrap_or(false);
            let protected = method == Method::Post
                || path.first().map(|s| *s == "artifact").unwrap_or(false);
            if protected && !is_loopback {
                let want = format!("Bearer {ap_token}");
                let ok = req.headers().iter().any(|h| {
                    (h.field.equiv("Authorization") && h.value.as_str().trim() == want)
                        || (h.field.equiv("X-Api-Key") && h.value.as_str().trim() == ap_token)
                });
                if !ok {
                    let _ = req.respond(
                        Response::from_string(json!({"error":"unauthorized"}).to_string())
                            .with_header(hdr.clone())
                            .with_status_code(401),
                    );
                    return;
                }
            }
        }

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
            (Method::Get, ["mode"]) => json!({
                "hot_wallet_enabled": allow_hot_wallet,
                "mode": if allow_hot_wallet { "testnet_free_ai" } else { "mainnet_client_signed" },
                "note": if allow_hot_wallet {
                    "testnet: the access point pays for jobs (free AI); callers may also sign with their own wallet via POST /route/{id}"
                } else {
                    "mainnet: POST /jobs is disabled — sign the job with YOUR wallet and POST /route/{id}"
                }
            }),
            // ── OpenAI-compatible facade ─────────────────────────────────────
            (Method::Get, ["v1", "models"]) => {
                let reg = registry.lock().unwrap();
                let mut ids: Vec<String> = reg.iter().flat_map(|p| p.models.clone()).collect();
                ids.sort();
                ids.dedup();
                if ids.is_empty() {
                    ids.push("llama3.2:1b".into());
                }
                json!({"object":"list", "data": ids.iter().map(|m| json!({
                    "id": m, "object": "model", "owned_by": "gpn"
                })).collect::<Vec<_>>()})
            }
            (Method::Post, ["v1", "chat", "completions"]) => {
                let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("auto").to_string();
                let stream = v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false);
                let prompt = messages_to_prompt(&v);
                if !allow_hot_wallet {
                    let r = Response::from_string(json!({
                        "error": {"message": "hot-wallet inference is disabled on this access point (mainnet)", "type": "gpn_error"}
                    }).to_string()).with_header(hdr.clone()).with_status_code(403);
                    let _ = req.respond(r);
                    return;
                }
                // route to a provider — resolve/validate the model FIRST so we
                // never create a job for a model no provider can serve (those
                // orphaned Pending jobs saturate the global MaxActiveJobs cap).
                let reg = registry.lock().unwrap().clone();
                let model = match resolve_requested_model(&reg, &model) {
                    Ok(m) => m,
                    Err(e) => {
                        let r = Response::from_string(json!({
                            "error": {"message": e, "type": "gpn_error"}
                        }).to_string()).with_header(hdr.clone()).with_status_code(400);
                        let _ = req.respond(r);
                        return;
                    }
                };
                let chosen = pick(&reg, 0, &model, pull_penalty_ms);
                let (provider, needs_pull) = match chosen {
                    Some(x) => x,
                    None => {
                        let r = Response::from_string(json!({
                            "error": {"message": "no compatible provider online", "type": "gpn_error"}
                        }).to_string()).with_header(hdr.clone()).with_status_code(503);
                        let _ = req.respond(r);
                        return;
                    }
                };
                // Passthrough mode (`"mode":"passthrough"`): run inference
                // directly on the provider with NO on-chain job, escrow, commitment,
                // or billing. Answer returned; nothing recorded. Default = settle.
                let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("settle").to_ascii_lowercase();
                let passthrough = mode == "passthrough";
                // create + route (hot wallet pays). Id comes from a process-local
                // allocator so concurrent requests never predict the same job id.
                let next_before = rt.block_on(client.get_next_job_id());
                let workload_hash = {
                    use sha2::{Digest, Sha256};
                    let mut h = Sha256::new();
                    h.update(now_secs().to_le_bytes());
                    h.update(prompt.as_bytes());
                    let mut o = [0u8; 32];
                    o.copy_from_slice(&h.finalize());
                    o[0] |= 1;
                    o
                };
                // Fast AP path (GPN_FAST_AP, default OFF): submit create_job
                // WITHOUT waiting for block inclusion and predict the id from the
                // pre-submit counter. Removes the AP's create_job ≈1-block stall
                // from the e2e path. The chain catches up in the background; the
                // provider self-heals any create/accept ordering race on its next
                // poll.
                //
                // NOTE: the process-local id allocator is only consulted on this
                // fast path, so it stays fully inert on the stop-and-wait path.
                let fast_ap = std::env::var("GPN_FAST_AP")
                    .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                    .unwrap_or(true);
                let (created, id) = if passthrough {
                    (false, 0u64)
                } else if fast_ap {
                    let predicted = alloc_predicted_job_id(next_before);
                    let submit = rt.block_on(client.create_job_fast(
                        8192u32, 75u16, 1u8, 0u32, 150u64, "LLMInference", "Spot",
                        (1_000_000_000u128 * 512) as u128, 1_000_000_000u128, 512u32, 1000,
                        workload_hash,
                    ));
                    // CONFIRM the real on-chain id (the source of truth). The AP is the sole
                    // creator, so the create lands within ~1 block and NextJobId advances; our
                    // id is the confirmed counter value. Then re-seed the allocator so it can't
                    // stay drifted ahead of the chain. Bounded so it never stalls indefinitely.
                    let mut real: Option<u64> = None;
                    let deadline = Instant::now() + Duration::from_millis(6000);
                    while Instant::now() < deadline {
                        let after = rt.block_on(client.get_next_job_id());
                        if after > next_before {
                            real = Some(after - 1);
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    match (submit, real) {
                        // Created + confirmed: label with the REAL on-chain id, re-seed allocator.
                        (Ok(()), Some(id)) | (Err(_), Some(id)) => {
                            reseed_alloc(id + 1);
                            (true, id)
                        }
                        // Submitted but not yet visible (rare): best-effort prediction.
                        (Ok(()), None) => (true, predicted),
                        // Not submitted and not visible: realign to the chain, declare failure.
                        (Err(_), None) => {
                            reseed_alloc(next_before);
                            (false, next_before)
                        }
                    }
                } else {
                    let _ = rt.block_on(client.create_job(
                        8192u32, 75u16, 1u8, 0u32, 150u64, "LLMInference", "Spot",
                        (1_000_000_000u128 * 512) as u128, 1_000_000_000u128, 512u32, 1000,
                        workload_hash,
                    ));
                    let after = rt.block_on(client.get_next_job_id());
                    (after > next_before, after.saturating_sub(1))
                };
                let mut text = String::new();
                if passthrough {
                    // Direct provider call, no chain. Derive the off-chain infer
                    // endpoint from the provider's status_url (:8093 → :8094).
                    let infer_url = format!(
                        "{}/infer",
                        provider.status_url.replacen(":8093", ":8094", 1)
                    );
                    let body = json!({"model": model, "prompt": prompt, "temperature": 0.7, "max_tokens": 512});
                    text = match reqwest::blocking::Client::new()
                        .post(&infer_url)
                        .json(&body)
                        .send()
                        .and_then(|r| r.json::<serde_json::Value>())
                    {
                        Ok(j) if j.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                            j.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string()
                        }
                        Ok(j) => format!(
                            "[gpn error] {}",
                            j.get("error").and_then(|x| x.as_str()).unwrap_or("inference failed")
                        ),
                        Err(e) => format!("[gpn error] {e}"),
                    };
                } else if created {
                    record_created(id, 512);
                    let _ = gpn_agent::llm::LlmStore::new().put(id, gpn_agent::llm::LlmJob::new_llm(&model, &prompt, 0.7, 512));
                    let _ = write_assignment(&assign_store, id, &provider.address, &model, needs_pull);
                    // block until the provider finishes (OpenAI calls are synchronous)
                    for _ in 0..1200 {
                        if let Some(j) = gpn_agent::llm::get_job(id) {
                            if j.status == "done" {
                                text = j.response.clone().unwrap_or_default();
                                break;
                            }
                            if j.status == "error" {
                                text = format!("[gpn error] {}", j.error.clone().unwrap_or_default());
                                break;
                            }
                        }
                        std::thread::sleep(Duration::from_millis(300));
                    }
                } else {
                    text = "[gpn error] could not create job".to_string();
                }
                if stream {
                    let idc = format!("chatcmpl-gpn-{id}");
                    let created = now_secs();
                    let chunk1 = json!({"id":idc,"object":"chat.completion.chunk","created":created,"model":model,
                        "choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":serde_json::Value::Null}]});
                    let chunk2 = json!({"id":idc,"object":"chat.completion.chunk","created":created,"model":model,
                        "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
                    let sse = format!("data: {chunk1}\n\ndata: {chunk2}\n\ndata: [DONE]\n\n");
                    let ct = Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..]).unwrap();
                    let cc = Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..]).unwrap();
                    let r = Response::from_string(sse).with_header(ct).with_header(cc);
                    let _ = req.respond(r);
                    return;
                }
                json!({
                    "id": format!("chatcmpl-gpn-{id}"), "object": "chat.completion", "created": now_secs(),
                    "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
                })
            }
            (Method::Post, ["jobs"]) => {
                if !allow_hot_wallet {
                    let r = Response::from_string(json!({
                        "ok": false,
                        "error": "hot-wallet jobs are disabled on this access point (mainnet). Sign the job with your own wallet and POST /route/{id} instead (see gpn_job)."
                    }).to_string()).with_header(hdr.clone()).with_status_code(403);
                    let _ = req.respond(r);
                    return;
                }
                let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let g = |k: &str, d: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(d);
                let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("auto").to_string();
                let wait = v.get("wait").and_then(|x| x.as_bool()).unwrap_or(false);
                let wait_secs: u64 = v.get("wait_secs").and_then(|x| x.as_u64()).unwrap_or(180);
                // per-token billing (optional): rate × max_tokens = escrow ceiling.
                let ppt_in: u128 = v.get("price_per_token").and_then(|x| x.as_u64()).unwrap_or(0) as u128;
                let mtok: u32 = v.get("max_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                // Oracle + fleet cost economics → GPN per token.
                //   market  = oracle avg_gpu_hour_price (per-class market rental)
                //   target  = clamp(rental_frac × market, owner_cost, market)
                //   tok/s   = fleet-average measured throughput
                //   $/token = target_hr / (tok/s × 3600 × util) × (1 + margin)
                //   gpn/tok = $/token ÷ token_price_usd
                let (token_price_usd, market_hr, util) = rt
                    .block_on(client.get_feed())
                    .map(|f| (f.token_price_usd, f.avg_gpu_hour_price_usd, f.utilization))
                    .unwrap_or((0.15, 3.0, 0.65));
                let params_b = params_b_for(&model);
                let (tok_per_s, owner_hr) = {
                    let reg = registry.lock().unwrap().clone();
                    let live: Vec<&Provider> = reg.iter().filter(|p| p.last_seen.is_some()).collect();
                    if live.is_empty() {
                        (roofline_tok_per_s(165.0, 250.0, params_b), 0.03_f64)
                    } else {
                        let t = live
                            .iter()
                            .map(|p| roofline_tok_per_s(p.gflops as f64, p.mem_bw_gbps, params_b))
                            .sum::<f64>()
                            / live.len() as f64;
                        let o = live.iter().map(|p| p.owner_cost_usd_hr).fold(f64::MAX, f64::min);
                        (t.max(1.0), o.max(0.001))
                    }
                };
                let market_hr = market_hr.max(0.001);
                let util = util.clamp(0.05, 1.0);
                let target_hr = (rental_frac * market_hr).clamp(owner_hr, market_hr);
                let usd_per_token = (target_hr / (tok_per_s * 3600.0 * util)) * (1.0 + margin);
                let oracle_ppt: u128 =
                    ((usd_per_token / token_price_usd.max(1e-9)) * 1e12).max(1.0) as u128;
                let ppt: u128 = if ppt_in > 0 { ppt_in } else { oracle_ppt };
                let mtok_eff: u32 = if mtok > 0 { mtok } else { 256 };
                let min_gflops = g("min_gflops", 150);
                let price_gpn = v.get("price_gpn").and_then(|x| x.as_f64()).unwrap_or(10.0);
                let job_type = v.get("job_type").and_then(|x| x.as_str()).unwrap_or("LLMInference");
                let prompt = v.get("prompt").and_then(|x| x.as_str()).map(|s| s.to_string());

                // choose the best provider (model-aware). Generation jobs
                // (image/video) are served by the diffusion provider, which
                // self-claims from chain — the LLM model registry doesn't list
                // gen models, so DON'T validate them here. Reject unknown models
                // only for LLM/text jobs (prevents orphaned Pending jobs).
                let jt_lc = job_type.to_ascii_lowercase();
                let model_lc = model.to_ascii_lowercase();
                let is_gen = jt_lc.contains("image")
                    || jt_lc.contains("video")
                    || model_lc.contains("sd-")
                    || model_lc.contains("stable-diffusion")
                    || model_lc.contains("sdxl")
                    || model_lc.contains("flux")
                    || model_lc.contains("ltx");
                let reg = registry.lock().unwrap().clone();
                let model = if is_gen {
                    model
                } else {
                    match resolve_requested_model(&reg, &model) {
                        Ok(m) => m,
                        Err(e) => {
                            let r = Response::from_string(json!({"ok":false,"error":e}).to_string())
                                .with_header(hdr.clone()).with_status_code(400);
                            let _ = req.respond(r);
                            return;
                        }
                    }
                };
                let (provider, needs_pull) = match pick(&reg, min_gflops, &model, pull_penalty_ms) {
                    Some(x) => x,
                    None => {
                        let r = Response::from_string(json!({"ok":false,"error":"no compatible provider available"}).to_string())
                            .with_header(hdr.clone()).with_status_code(503);
                        let _ = req.respond(r);
                        return;
                    }
                };

                // create the on-chain job (escrow) as the access-point signer
                // workload hash: the marketplace rejects all-zeros, but we do NOT
                // want the prompt recoverable on-chain. Use an opaque random-ish id
                // (privacy: the question is never derivable from chain state).
                let workload_hash = {
                    use sha2::{Digest, Sha256};
                    use std::sync::atomic::{AtomicU64, Ordering};
                    use std::time::{SystemTime, UNIX_EPOCH};
                    static CTR: AtomicU64 = AtomicU64::new(0);
                    let n = CTR.fetch_add(1, Ordering::Relaxed);
                    let t = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0);
                    let mut h = Sha256::new();
                    h.update(t.to_le_bytes());
                    h.update(n.to_le_bytes());
                    h.update(std::process::id().to_le_bytes());
                    let mut o = [0u8; 32];
                    o.copy_from_slice(&h.finalize());
                    o[0] |= 1;
                    o
                };
                let next_before = rt.block_on(client.get_next_job_id());
                // Job-relative pricing for generation jobs (pixels × steps), grounded
                // in the oracle market GPU price; text jobs price per-token (ppt).
                let price_gpn = if is_gen {
                    match v.get("price_gpn").and_then(|x| x.as_f64()) {
                        Some(p) if p > 0.0 => p, // explicit override
                        _ => image_price_gpn(
                            g("width", 512) as f64,
                            g("height", 512) as f64,
                            g("steps", 8) as f64,
                            token_price_usd,
                            market_hr,
                            util,
                        ),
                    }
                } else {
                    price_gpn
                };
                // escrow/pricing: generation jobs use a flat, job-sized price
                // (price_gpn from pixels×steps); text jobs use per-token billing.
                let (escrow, job_ppt, job_mtok): (u128, u128, u32) = if is_gen {
                    ((price_gpn * 1e12) as u128, 0u128, 0u32)
                } else if ppt > 0 && mtok_eff > 0 {
                    (ppt.saturating_mul(mtok_eff as u128), ppt, mtok_eff)
                } else {
                    ((price_gpn * 1e12) as u128, 0u128, 0u32)
                };
                let res = rt.block_on(client.create_job(
                    g("min_vram_mb", 8192) as u32,
                    g("min_compute_capability", 75) as u16,
                    g("min_gpu_count", 1) as u8,
                    g("min_cuda_cores", 0) as u32,
                    min_gflops, job_type, "Spot",
                    escrow, job_ppt, job_mtok, 1000, workload_hash,
                ));
                // The signer can mis-attribute a *concurrent* extrinsic's failure
                // (a busy chain packs several txs per block). So don't trust the
                // Err blindly: re-read the job counter — if it advanced, OUR job
                // was in fact created, and we must still route it.
                let after = rt.block_on(client.get_next_job_id());
                match (res, after > next_before) {
                    (Ok(_), _) | (Err(_), true) => {
                        let id = after.saturating_sub(1);
                        // store the off-chain payload (prompt/model/params) if present.
                        // Generation jobs (image/video) MUST be stored with a gen
                        // kind, otherwise the LLM provider claims them and 404s on
                        // a diffusion model; the diffusion provider only claims
                        // jobs where `kind` is image/video.
                        if let Some(p) = prompt.as_ref() {
                            let jt = job_type.to_ascii_lowercase();
                            // Guard: a diffusion/video model submitted with
                            // job_type omitted (defaults to LLMInference) must
                            // be routed to the diffusion provider, not the LLM
                            // provider (which 404/500s pulling it from ollama).
                            let model_l = model.to_ascii_lowercase();
                            let model_diff = model_l.contains("sd-")
                                || model_l.contains("stable-diffusion")
                                || model_l.contains("sdxl")
                                || model_l.contains("flux")
                                || model_l.contains("ltx");
                            let is_gen_kind =
                                jt.contains("image") || jt.contains("video") || model_diff;
                            let job = if is_gen_kind {
                                let kind = if jt.contains("video") || model_l.contains("ltx") {
                                    "video"
                                } else {
                                    "image"
                                };
                                let neg = v.get("negative_prompt").and_then(|x| x.as_str());
                                gpn_agent::llm::LlmJob::new_gen(
                                    kind,
                                    &model,
                                    p,
                                    neg,
                                    g("width", 512) as u32,
                                    g("height", 512) as u32,
                                    g("steps", 8) as u32,
                                    v.get("seed").and_then(|x| x.as_i64()),
                                    g("frames", 16) as u32,
                                    g("fps", 8) as u32,
                                )
                            } else {
                                gpn_agent::llm::LlmJob::new_llm(&model, p, 0.7, 256)
                            };
                            let _ = gpn_agent::llm::LlmStore::new().put(id, job);
                        }
                        // dispatch: record which provider (by address) should claim it,
                        // plus the model + whether it must pull the model first.
                        record_created(id, mtok_eff as u64);
                        let _ = write_assignment(&assign_store, id, &provider.address, &model, needs_pull);
                        // optional inline wait: poll the off-chain store and return the
                        // answer in THIS response (no separate /result call).
                        let mut answer: Option<serde_json::Value> = None;
                        if wait {
                            let deadline = std::time::Instant::now() + Duration::from_secs(wait_secs.min(600));
                            while std::time::Instant::now() < deadline {
                                if let Some(j) = gpn_agent::llm::get_job(id) {
                                    if j.status == "done" || j.status == "error" {
                                        answer = Some(json!({
                                            "status": j.status, "response": j.response,
                                            "result_hash": j.result_hash, "salt": j.salt,
                                            "artifact_path": j.artifact_path,
                                            "artifact_media_type": j.artifact_media_type,
                                            "verify": "sha256(salt ++ response) == result_hash",
                                            "error": j.error,
                                        }));
                                        break;
                                    }
                                }
                                std::thread::sleep(Duration::from_millis(300));
                            }
                        }
                        let mut out = json!({"ok":true,"job_id":id,"routed_to":provider.address,
                               "model":model,"pull":needs_pull,
                               "status_url":provider.status_url,
                               "latency_ms":provider.latency_ms,"load":format!("{}/{}",provider.active,provider.capacity)});
                        if let Some(a) = answer {
                            out["answer"] = a;
                        } else if wait {
                            out["answer"] = json!({"status":"timeout","note":"not finished; poll /result/{id}"});
                        }
                        out
                    }
                    (Err(e), false) => json!({"ok":false,"error":format!("{e}")}),
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
            // The answer/artifact lives OFF-chain in the provider's local store.
            // On this testnet the access point shares the provider's box, so it can
            // serve the result. (Production: the provider returns the result to the
            // client directly, or encrypted to the client's key.)
            (Method::Get, ["result", id]) => {
                let idn: u64 = id.parse().unwrap_or(0);
                match gpn_agent::llm::get_job(idn) {
                    Some(j) => json!({
                        "job_id": idn, "kind": j.kind, "model": j.model, "prompt": j.prompt,
                        "status": j.status, "response": j.response,
                        "result_hash": j.result_hash, "salt": j.salt,
                        "artifact_path": j.artifact_path,
                        "artifact_media_type": j.artifact_media_type,
                        "verify": "sha256(salt ++ response) == result_hash",
                        "error": j.error,
                    }),
                    None => json!({"job_id": idn, "error": "no off-chain record yet"}),
                }
            }
            // Download the generated artifact (image/video) for a job id.
            // Serves the provider's on-disk file over HTTP so remote clients can
            // fetch it (the AP has no other way to hand back the bytes).
            (Method::Get, ["artifact", id]) => {
                let idp = id.split('?').next().unwrap_or(id);
                let idn: u64 = idp.parse().unwrap_or(0);
                let rec = gpn_agent::llm::get_job(idn);
                let path = rec.as_ref().and_then(|j| j.artifact_path.clone());
                match path {
                    Some(p) if std::path::Path::new(&p).exists() => match std::fs::read(&p) {
                        Ok(bytes) => {
                            let mt = rec
                                .and_then(|j| j.artifact_media_type)
                                .unwrap_or_else(|| "application/octet-stream".into());
                            let name = std::path::Path::new(&p)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| format!("job_{idn}"));
                            let ct = Header::from_bytes(&b"Content-Type"[..], mt.as_bytes())
                                .unwrap_or_else(|_| hdr.clone());
                            let cd = Header::from_bytes(
                                &b"Content-Disposition"[..],
                                format!("inline; filename=\"{name}\"").as_bytes(),
                            )
                            .unwrap_or_else(|_| hdr.clone());
                            let _ = req.respond(Response::from_data(bytes).with_header(ct).with_header(cd));
                        }
                        Err(e) => {
                            let _ = req.respond(
                                Response::from_string(json!({"error": e.to_string()}).to_string())
                                    .with_header(hdr.clone())
                                    .with_status_code(500),
                            );
                        }
                    },
                    _ => {
                        let _ = req.respond(
                            Response::from_string(
                                json!({"error":"no artifact for that job (not generated yet?)"}).to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(404),
                        );
                    }
                }
                return;
            }
            // Serve the canonical chain-spec so a fresh node (e.g. Windows) can
            // join THIS chain: `--chain <downloaded>.json` pins the exact genesis.
            (Method::Get, ["genesis-spec"]) => {
                let p = std::env::var("GPN_SPEC_PATH").unwrap_or_else(|_| {
                    "/home/jeremyg/shared_workspace/artifacts/project7/gpn-testnet-spec.json".to_string()
                });
                match std::fs::read(&p) {
                    Ok(bytes) => {
                        let ct = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                            .unwrap_or_else(|_| hdr.clone());
                        let cd = Header::from_bytes(
                            &b"Content-Disposition"[..],
                            &b"attachment; filename=\"gpn-testnet-spec.json\""[..],
                        )
                        .unwrap_or_else(|_| hdr.clone());
                        let _ = req.respond(Response::from_data(bytes).with_header(ct).with_header(cd));
                    }
                    Err(e) => {
                        let _ = req.respond(
                            Response::from_string(json!({"error": e.to_string()}).to_string())
                                .with_header(hdr.clone())
                                .with_status_code(500),
                        );
                    }
                }
                return;
            }
            // Client-side signing: the caller already created + escrowed the job
            // with THEIR OWN wallet; they just ask us to ROUTE it to a provider.
            (Method::Post, ["route", id]) => {
                // optional ?model=<m> in the query string
                let (idp, query) = id.split_once('?').unwrap_or((id, ""));
                let idn: u64 = idp.parse().unwrap_or(0);
                let qmodel = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("model="))
                    .map(|m| m.replace("%3A", ":").replace("%3a", ":"))
                    .unwrap_or_else(|| "auto".to_string());
                if rt.block_on(client.get_job_state(idn)).is_none() {
                    json!({"ok":false,"error":"job not found"})
                } else {
                    let reg = registry.lock().unwrap().clone();
                    match resolve_requested_model(&reg, &qmodel) {
                        Err(e) => json!({"ok":false,"error":e}),
                        Ok(qmodel) => match pick(&reg, 0, &qmodel, pull_penalty_ms) {
                            Some((p, pull)) => {
                                let _ = write_assignment(&assign_store, idn, &p.address, &qmodel, pull);
                                json!({"ok":true,"job_id":idn,"routed_to":p.address,
                                       "model":qmodel,"pull":pull,
                                       "latency_ms":p.latency_ms,"load":format!("{}/{}",p.active,p.capacity)})
                            }
                            None => json!({"ok":false,"error":"no compatible provider available"}),
                        },
                    }
                }
            }
            _ => json!({"error":"not found"}),
        };

        let code = if resp.get("error").map(|v| !v.is_null()).unwrap_or(false) { 404 } else { 200 };
        let _ = req.respond(Response::from_string(resp.to_string()).with_header(hdr.clone()).with_status_code(code));
        });
    }
    Ok(())
}
