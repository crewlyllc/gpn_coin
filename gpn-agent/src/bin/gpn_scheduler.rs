//! gpn_scheduler — an GPN **access point**: a public job API that ROUTES work to
//! the best provider instead of making callers know provider IPs.
//!
//! A caller hits one URL (`POST /jobs`); the scheduler:
//!   1. filters providers by COMPATIBILITY (attested GFLOPS >= job min_gflops),
//!   2. scores the survivors by LATENCY + LOAD (from each provider's /status heartbeat),
//!      preferring the provider whose **user→provider** RTT is lowest when the
//!      caller supplies client-measured RTT hints (`{"rtt": {"<addr|url>": <ms>}}`)
//!      — see `routing.rs` / item 3 (client-side RTT). Falls back to the
//!      access-point-measured latency when no hints are given.
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
use gpn_agent::routing::{self, RttOverrides};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Response, Server};

/// Job ids this AP created recently, with creation time. Used by the janitor to
/// reclaim orphaned jobs that would otherwise hold a global `MaxActiveJobs` slot
/// forever. Reclaimed via the marketplace's permissionless `expire_job` (past
/// `created_at + max_duration`) and, as a fast path, requestor-side `cancel_job`
/// — see artifacts/project7/job-lifecycle-gap-analysis-2026-10-05.md.
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

/// Terminal job states — a job here no longer holds an `ActiveJobCount` slot,
/// so the janitor stops tracking it.
fn is_terminal_state(state: &str) -> bool {
    matches!(state, "Completed" | "Cancelled" | "Failed")
}

/// Has a job outlived its on-chain deadline (`now > created_at + max_duration`)?
/// Mirrors `pallet-compute-marketplace::expire_job`'s guard exactly (strict `>`).
fn past_deadline(created_at: u32, max_duration: u32, head: u64) -> bool {
    head > created_at as u64 + max_duration as u64
}

/// How many recent job ids to re-adopt at AP startup so orphaned non-terminal
/// jobs created by a *previous* AP process (lost from the in-memory janitor
/// list) are still reclaimed once past their on-chain deadline.
const JANITOR_SEED_LOOKBACK: u64 = 256;

/// Every 30s, reclaim this AP's orphaned jobs:
///   1. **Primary — pallet `expire_job` (permissionless):** any non-terminal job
///      past its on-chain `created_at + max_duration` deadline is reclaimed via the
///      marketplace's own timeout. Being permissionless and state-agnostic, this
///      also clears provider-abandoned `Accepted`/`Running` orphans, which a
///      requestor-side `cancel_job` can never touch.
///   2. **Fast path — `cancel_job` (requestor-only, Pending-only):** our own
///      still-`Pending` jobs past the token-scaled [`stale_secs`] window get a
///      quick requestor cancel, well before the on-chain deadline.
/// Both use the shared nonce stream (`*_fast`) so they can't desync request-path
/// txs. Tracking is seeded from recent chain job ids so restarts don't leak slots.
fn spawn_janitor(client: Arc<ChainClient>, rt: Arc<tokio::runtime::Runtime>) {
    // Re-adopt non-terminal jobs from the last AP epoch so the on-chain expiry can
    // still reclaim them after a restart (the in-memory list is empty then).
    let next = rt.block_on(client.get_next_job_id());
    let start = next.saturating_sub(JANITOR_SEED_LOOKBACK);
    let mut seeded = 0u32;
    for id in start..next {
        if let Some((state, _, _, _)) = rt.block_on(client.get_job_state(id)) {
            if !is_terminal_state(&state) {
                record_created(id, 0);
                seeded += 1;
            }
        }
    }
    if seeded > 0 {
        println!("[janitor] re-adopted {seeded} non-terminal job(s) from #{start}..#{} after restart", next.saturating_sub(1));
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let now = Instant::now();
        let head = rt.block_on(client.head_block_number()).unwrap_or(0);
        let snapshot: Vec<(u64, Instant, u64)> = recent_jobs().lock().unwrap().clone();
        let mut keep: Vec<(u64, Instant, u64)> = Vec::with_capacity(snapshot.len());
        for (id, t, mt) in snapshot {
            let Some((state, _, _, _)) = rt.block_on(client.get_job_state(id)) else {
                continue; // gone / unreadable — stop tracking
            };
            if is_terminal_state(&state) {
                continue; // slot already freed
            }
            // Primary: permissionless on-chain expiry for ANY non-terminal job
            // past its deadline (also reclaims Accepted/Running orphans).
            if let Some((created_at, max_duration)) = rt.block_on(client.get_job_timing(id)) {
                if past_deadline(created_at, max_duration, head) {
                    let deadline = created_at as u64 + max_duration as u64;
                    match rt.block_on(client.expire_job_fast(id)) {
                        Ok(()) => {
                            println!(
                                "[janitor] expired #{id} via pallet expire_job (state {state}, block {head} > deadline {deadline})"
                            );
                            continue;
                        }
                        Err(e) => eprintln!("[janitor] expire #{id} failed: {e}"),
                    }
                }
            }
            // Fast path: requestor cancel of our own stale Pending jobs.
            if state == "Pending" && now.duration_since(t).as_secs() > stale_secs(mt) {
                match rt.block_on(client.cancel_job_fast(id)) {
                    Ok(()) => {
                        println!(
                            "[janitor] cancelled stale Pending #{id} (max_tokens {mt}, waited >{}s)",
                            stale_secs(mt)
                        );
                        continue;
                    }
                    Err(e) => eprintln!("[janitor] cancel #{id} failed: {e}"),
                }
            }
            keep.push((id, t, mt));
        }
        *recent_jobs().lock().unwrap() = keep;
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

/// Serializes on-chain job CREATION so exactly ONE create is in flight at a time.
/// The AP is the sole creator and the chain assigns ids in submit order, so with
/// serialization the process-local prediction is exactly the real on-chain id — and
/// concurrent requests can't bind to one shared `NextJobId` (the misattribution bug).
static CREATE_LOCK: Mutex<()> = Mutex::new(());

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

/// Force the process-local allocator to an exact on-chain id. Used to HEAL a drift after
/// a create that consumed a local id but did NOT land. Only safe with no create in flight
/// (enforced by `CREATE_LOCK`).
fn align_alloc(next_id: u64) {
    let mut g = NEXT_JOB_ID.lock().unwrap();
    *g = Some(next_id);
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
    vram_mb: u64,
    ram_mb: u64,
    /// GPU class advertised by the heartbeat (item 19): "discrete" |
    /// "integrated" | "unknown". A *supporting* flag for the TFLOPS floor.
    gpu_class: String,
    /// Estimated FP32 TFLOP/s advertised by the heartbeat; 0.0 = unknown.
    tflops: f64,
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
///
/// The candidate set is the **union** of (a) any static `--providers` URLs and
/// (b) providers DISCOVERED ON-CHAIN (`ComputeMarketplace::ProviderEndpoints`,
/// item 1). On-chain discovery means a new provider becomes routable without
/// editing the access point's config — it just advertises its endpoint with
/// `set_provider_endpoint` and this loop picks it up. Entries that resolve to
/// the same provider address are de-duplicated (keep the lowest-latency URL), so
/// a node that is listed both statically and on-chain isn't counted twice.
fn refresh(
    registry: &Arc<Mutex<Vec<Provider>>>,
    urls: &Arc<Vec<String>>,
    client: Arc<ChainClient>,
    rt: Arc<tokio::runtime::Runtime>,
) {
    let reg = registry.clone();
    let urls = urls.clone();
    std::thread::spawn(move || loop {
        // 1. Merge the static list with the on-chain discovered endpoints.
        let mut all: Vec<String> = urls.iter().cloned().collect();
        // A provider discovered on-chain identifies itself by its storage KEY
        // (the AccountId), so keep url -> account so we can attribute an
        // endpoint whose /status heartbeat omits its own `address`.
        let mut chain_addr: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for (who, url) in rt.block_on(client.list_provider_endpoints()) {
            if let Some(addr) = hex_to_ss58(&who) {
                chain_addr.entry(url.clone()).or_insert(addr);
            }
            if !all.iter().any(|u| u == &url) {
                all.push(url);
            }
        }

        // 2. Poll every endpoint's /status into a Provider.
        let mut out = Vec::new();
        for u in all.iter() {
            let v = jpost(u);
            let p = match v {
                Some(v) => Provider {
                    status_url: u.clone(),
                    address: attribute_address(
                        v.get("address").and_then(|x| x.as_str()).unwrap_or(""),
                        u,
                        &chain_addr,
                    ),
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
                    vram_mb: v.get("vram_mb").and_then(|x| x.as_u64()).unwrap_or(0),
                    ram_mb: v.get("ram_mb").and_then(|x| x.as_u64()).unwrap_or(0),
                    gpu_class: v
                        .get("gpu_class")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    tflops: v.get("tflops").and_then(|x| x.as_f64()).unwrap_or(0.0),
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
                    vram_mb: 0,
                    ram_mb: 0,
                    gpu_class: "unknown".to_string(),
                    tflops: 0.0,
                    active: 0,
                    capacity: 0,
                    latency_ms: 9999,
                    last_seen: None,
                },
            };
            out.push(p);
        }

        // 3. De-duplicate by provider address (a node listed both statically and
        //    on-chain must not be counted twice). Keep the lowest-latency entry.
        out.sort_by_key(|p| p.latency_ms);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        out.retain(|p| p.address.is_empty() || seen.insert(p.address.clone()));

        *reg.lock().unwrap() = out;
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// Resolve a registry entry's `address`: prefer the provider's own heartbeat
/// `address`; when it is empty (an on-chain-discovered endpoint whose `/status`
/// was started without `--address`), fall back to the provider's on-chain key so
/// the entry stays attributable (assignments / `routed_to` / client RTT hints
/// key on `address`).
fn attribute_address(hb_addr: &str, url: &str, chain_addr: &std::collections::HashMap<String, String>) -> String {
    if hb_addr.is_empty() {
        chain_addr.get(url).cloned().unwrap_or_default()
    } else {
        hb_addr.to_string()
    }
}

/// Convert a `0x`-hex AccountId32 (the form chain storage reads return, see
/// `ChainClient::list_provider_endpoints`) into the canonical ss58 string used
/// everywhere else in the agent, so an on-chain-discovered provider can be keyed
/// by address (client RTT hints, assignment store, `routed_to`).
fn hex_to_ss58(hex_addr: &str) -> Option<String> {
    use sp_core::crypto::Ss58Codec;
    let raw = hex_addr.strip_prefix("0x").unwrap_or(hex_addr);
    let bytes: [u8; 32] = hex::decode(raw).ok()?.try_into().ok()?;
    Some(sp_core::sr25519::Public::from_raw(bytes).to_ss58check())
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

/// Resolve the effective inference prompt for a native `POST /jobs` body.
/// Accepts `prompt` (native), `input` (ML inference), or an OpenAI-style
/// `messages[]`/`system` body. Returns `None` when the body carries no usable
/// prompt, so the caller can reject the job BEFORE creating it on-chain.
///
/// Without this gate a prompt-less body is still created on-chain (escrow
/// locked + a global `ActiveJobCount` and per-requestor slot consumed) but
/// never written to the off-chain store, so no provider can claim it — a
/// guaranteed unservable orphan until the expiry sweep, while the caller gets
/// a misleading `ok:true` (red-team finding #24).
fn resolve_job_prompt(v: &Value) -> Option<String> {
    if let Some(p) = v.get("prompt").and_then(|x| x.as_str()) {
        if !p.trim().is_empty() {
            return Some(p.to_string());
        }
    }
    if let Some(i) = v.get("input").and_then(|x| x.as_str()) {
        if !i.trim().is_empty() {
            return Some(i.to_string());
        }
    }
    let has_messages = v
        .get("messages")
        .and_then(|x| x.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let has_system = v.get("system").and_then(|x| x.as_str()).is_some();
    if has_messages || has_system {
        return Some(messages_to_prompt(v));
    }
    None
}

/// Abuse & safety guardrail (item #22): moderate a job prompt at the access
/// point before any job is created/routed. Returns `Some(category)` when the
/// AP policy is to **refuse** the prompt (fail-closed hard category, enforced
/// mode); returns `None` when allowed — including in `warn` (log-only) mode,
/// where the match is logged but the request proceeds. Design:
/// `design-abuse-safety-guardrails.md`. Mode via `GPN_MODERATION`
/// (`on` default / `warn` / `off`); the classifier is a dependency-free,
/// high-precision rules baseline (a hosted classifier can replace it later).
fn moderation_block(prompt: &str) -> Option<gpn_agent::moderation::Category> {
    use gpn_agent::moderation::{mode_from_env, moderate, Mode, Verdict};
    if prompt.trim().is_empty() {
        return None;
    }
    match mode_from_env() {
        Mode::Off => None,
        Mode::Warn => {
            if let Verdict::Block { category } = moderate(prompt) {
                // Abuse signal (no content logged, privacy preserved): emit on
                // stderr like the AP's other ops logs so it is visible by default.
                eprintln!(
                    "[moderation] flagged category {} (warn-only, allowed)",
                    category.id()
                );
            }
            None
        }
        Mode::On => match moderate(prompt) {
            Verdict::Block { category } => {
                // Abuse signal only — never the prompt content (design §5).
                eprintln!("[moderation] refused a prompt in category {}", category.id());
                Some(category)
            }
            Verdict::Allow => None,
        },
    }
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

fn pick(registry: &[Provider], min_gflops: u64, min_vram_mb: u64, min_ram_mb: u64, min_tflops: f64, model: &str, pull_penalty_ms: u64, rtt: &RttOverrides) -> Option<(Provider, bool)> {
    // Hardware-capability gate (item 14): a job declaring a VRAM/RAM floor (e.g.
    // video needs a big-VRAM box) routes only to a provider that advertises AT
    // LEAST that much. This is a HARD requirement, so it FAILS CLOSED: a provider
    // that advertises less, or nothing (0 = unknown, e.g. an old heartbeat), is
    // EXCLUDED when a floor is set — the safe choice, since the alternative is
    // routing an OOM-prone job to a box that cannot run it. When no floor is set
    // (min_* = 0, the default) the gate is inert and routing is unchanged.
    let hw_ok = |p: &Provider| {
        (min_vram_mb == 0 || p.vram_mb >= min_vram_mb)
            && (min_ram_mb == 0 || p.ram_mb >= min_ram_mb)
    };
    // Minimum-GPU-capability gate (item 19): a real (discrete) GPU OR an
    // advertised FP32 TFLOPS figure that clears `min_tflops`. A CPU/APU
    // **integrated** GPU must not earn unless it clears the objective floor.
    // The TFLOPS floor is authoritative; "discrete" is only a *supporting*
    // flag for a box that hasn't reported a number yet. Fail-closed only when a
    // floor is set (min_tflops <= 0 ⇒ inert, routing unchanged):
    //   • known tflops      → must be ≥ the floor (discrete or not);
    //   • unknown tflops(0) → only a self-declared *discrete* card is trusted.
    let gpu_ok = |p: &Provider| {
        if min_tflops <= 0.0 {
            return true;
        }
        if p.tflops > 0.0 {
            return p.tflops >= min_tflops;
        }
        p.gpu_class == "discrete"
    };
    let live: Vec<&Provider> = registry
        .iter()
        .filter(|p| {
            p.last_seen.is_some() && p.gflops >= min_gflops && hw_ok(p) && gpu_ok(p)
        })
        .collect();

    // Effective latency for the *user's* path: a client-measured (user→provider)
    // RTT wins over the access-point-measured value when the caller supplied one
    // (item 3 — client-side RTT). Falls back to `latency_ms` otherwise.
    let eff = |p: &Provider| {
        routing::effective_latency_ms(p.latency_ms, routing::lookup_override(rtt, &p.address, &p.status_url))
    };

    // Availability: a node currently running an AI job is NOT available (one GPU does one
    // job at a time). Prefer idle nodes, chosen by LOWEST LATENCY (least-ms), not by load.
    let idle: Vec<&Provider> = live.iter().copied().filter(|p| p.active == 0).collect();

    // 1. Idle nodes that already HAVE the model -> lowest latency wins.
    let mut ready: Vec<&Provider> = idle.iter().copied().filter(|p| has_model(p, model)).collect();
    ready.sort_by_key(|p| eff(p));

    // 2. Idle nodes that CAN pull the model -> lowest latency.
    let mut pullers: Vec<&Provider> = idle.iter().copied().filter(|p| p.can_pull).collect();
    pullers.sort_by_key(|p| eff(p));

    match (ready.first().copied(), pullers.first().copied()) {
        // Both exist: a puller wins only if it beats the ready idle node end-to-end
        // (downloading on a fast/idle node still finishes sooner than waiting on a slow one).
        (Some(r), Some(p)) => {
            if eff(p) + pull_penalty_ms < eff(r) {
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
            busy.sort_by_key(|p| p.active * 1000 + eff(p) / 10);
            busy.first().map(|p| ((*p).clone(), false))
        }
    }
}

/// Client-side RTT hints from a request body: `{"rtt": {"<addr-or-url>": <ms>}}`.
///
/// The client measures its OWN round-trip to the provider endpoints (from
/// `GET /providers`) and/or the access point (via `GET /ping`) and passes them
/// here so routing reflects the **user→provider** path, not just AP→provider.
/// Absent/malformed hints are ignored → routing falls back to the AP-measured
/// latency (identical to the old behaviour).
fn rtt_overrides(v: &Value) -> RttOverrides {
    v.get("rtt").map(routing::parse_rtt_overrides).unwrap_or_default()
}

/// PUSH an assignment to the chosen provider over HTTP, so a REMOTE provider (which
/// can't read this AP's host-local assignments file) still learns the job is its. The
/// provider's heartbeat (`POST /assign`) writes it into the provider's OWN local file AND
/// (item #14) persists the full off-chain job payload into the provider's own LlmStore,
/// so a remote provider can actually RUN the job (the prompt lives only in the AP's
/// store otherwise). The push also carries a `callback` URL so the provider can return
/// the result to this AP. Best-effort — silently no-ops if the provider is unreachable.
fn dispatch_assignment(
    status_url: &str,
    job_id: u64,
    provider: &str,
    model: &str,
    pull: bool,
    job_payload: Option<&gpn_agent::llm::LlmJob>,
    callback_url: &str,
    callback_token: &str,
) {
    let base = status_url
        .trim_end_matches('/')
        .trim_end_matches("/status")
        .trim_end_matches('/');
    let url = format!("{base}/assign");
    // Shared AP↔provider secret so a remote provider can reject a forged
    // assignment push to its publicly-exposed heartbeat (item #16, finding #7).
    // Sent as a header + body field; unset env ⇒ the provider stays legacy/permissive.
    let assign_token = std::env::var("GPN_ASSIGN_TOKEN").unwrap_or_default();
    let body = assignment_body(job_id, provider, model, pull, job_payload, callback_url, callback_token, &assign_token);
    if let Ok(c) = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        let mut req = c.post(&url).json(&body);
        if !assign_token.is_empty() {
            req = req.header("X-Gpn-Assign-Token", &assign_token);
        }
        let _ = req.send();
    }
}

/// Build the `POST /assign` JSON body. Split out so the assignment-push auth
/// binding (item #16 finding #7) is unit-testable without an HTTP round trip.
fn assignment_body(
    job_id: u64,
    provider: &str,
    model: &str,
    pull: bool,
    job_payload: Option<&gpn_agent::llm::LlmJob>,
    callback_url: &str,
    callback_token: &str,
    assign_token: &str,
) -> Value {
    let mut body = json!({
        "job_id": job_id, "provider": provider, "model": model,
        "pull": pull, "callback": callback_url, "callback_token": callback_token,
        "assign_token": assign_token
    });
    if let Some(j) = job_payload {
        if let Ok(v) = serde_json::to_value(j) {
            body["job"] = v;
        }
    }
    body
}

fn write_assignment(
    store: &str,
    job_id: u64,
    provider: &str,
    model: &str,
    pull: bool,
    callback_url: &str,
    callback_token: &str,
) -> std::io::Result<()> {
    let mut m: serde_json::Map<String, Value> = std::fs::read_to_string(store)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    m.insert(
        job_id.to_string(),
        json!({"provider": provider, "model": model, "pull": pull, "callback": callback_url, "callback_token": callback_token}),
    );
    std::fs::write(store, serde_json::to_string_pretty(&Value::Object(m))?)
}

/// Generate an unguessable per-job callback token (item #16, red-team finding #2).
/// The `POST /result/{id}` result-return channel is exempt from the caller-token
/// auth gate (remote providers hold no user token), so it must be bound to the
/// assignment some other way: the AP issues this token at dispatch, the provider
/// echoes it back, and the AP rejects a result whose token doesn't match. 32 bytes
/// of OS entropy, hex-encoded; falls back to a hash of process/time state if
/// `/dev/urandom` is unavailable (still unguessable to a remote caller).
fn new_callback_token(job_id: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut buf = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if std::io::Read::read_exact(&mut f, &mut buf).is_ok() {
            return hex::encode(buf);
        }
    }
    let mut h = Sha256::new();
    h.update(job_id.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .to_le_bytes(),
    );
    hex::encode(h.finalize())
}

/// The per-job callback token the AP issued for this assignment, if any. Read
/// back from the AP's own assignment store so the `POST /result/{id}` handler can
/// authenticate a remote provider's result even across an AP restart.
fn assignment_token(store: &str, job_id: u64) -> Option<String> {
    let s = std::fs::read_to_string(store).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    v.get(job_id.to_string())
        .and_then(|r| r.get("callback_token"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .filter(|t| !t.is_empty())
}

/// Constant-time string equality (avoid leaking the token via timing).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
    // Public base URL this AP is reachable at, used as the `callback` a remote
    // provider POSTs the finished result to (`{callback}/result/{id}`). Item #14:
    // a remote provider can be anywhere, so it needs an absolute URL back to us.
    // Precedence: --callback-url > GPN_AP_CALLBACK_URL > GPN_PUBLIC_URL > http://<listen>.
    let callback_url = opt(&args, "--callback-url")
        .or_else(|| std::env::var("GPN_AP_CALLBACK_URL").ok())
        .or_else(|| std::env::var("GPN_PUBLIC_URL").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("http://{listen}"));
    // Optional bearer token. When set, state-changing (POST) + artifact requests
    // from NON-loopback clients must present it (local crons/monitors keep working).
    let ap_token = std::env::var("GPN_AP_TOKEN").unwrap_or_default();
    // Item #22 build-order step 4: the abuse **report path + operator denylist**.
    // The denylist (accounts / providers / IPs) is enforced at this AP and read
    // by providers from the SAME file (`GPN_DENY_FILE` / `~/.gpn/denylist.json`);
    // reports are appended to a JSONL log (`GPN_REPORTS_FILE`).
    let denylist_path = gpn_agent::abuse::Denylist::default_path();
    let denylist = {
        let mut d = gpn_agent::abuse::Denylist::load(&denylist_path);
        d.merge_env();
        Arc::new(Mutex::new(d))
    };
    let reports_path = gpn_agent::abuse::reports_path();
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
    refresh(&registry, &Arc::new(urls.clone()), client.clone(), rt.clone());
    println!("  on-chain discovery: ENABLED (ComputeMarketplace::ProviderEndpoints)");

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    println!("GPN access point (scheduler) listening on http://{listen}");
    println!("  chain: {endpoint}");
    println!("  providers: {} ({})", urls.len(), urls.join(", "));
    println!("  assign store: {assign_store}");
    println!("  endpoints: /health /ping /providers /jobs /job/{{id}} /mode | POST /jobs /route/{{id}}");
    println!("  abuse: denylist={} ({} bans) reports={}",
        denylist_path.display(), denylist.lock().unwrap().accounts.len(), reports_path.display());
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
        let callback_url = callback_url.clone();
        let hdr = hdr.clone();
        let ap_token = ap_token.clone();
        let listen = listen.clone();
        let denylist = denylist.clone();
        let denylist_path = denylist_path.clone();
        let reports_path = reports_path.clone();
        std::thread::spawn(move || {
        let url = req.url().to_string();
        let method = req.method().clone();
        let mut body = String::new();
        let _ = req.as_reader().read_to_string(&mut body);
        let path: Vec<&str> = url.trim_start_matches('/').split('/').filter(|s| !s.is_empty()).collect();

        // Auth gate (non-loopback only).
        if !ap_token.is_empty() {
            let is_loopback = req.remote_addr().map(|a| a.ip().is_loopback()).unwrap_or(false);
            // `/result/{id}` is the provider→AP result-return channel (item #14);
            // a remote provider holds no user token, so the POST side is exempt
            // from the caller-token gate — instead it is authenticated by the
            // per-job callback token (item #16, finding #2) checked in its handler.
            // The GET side (read the prompt/response) still requires the token.
            let is_result_path = path.first().map(|s| *s == "result").unwrap_or(false);
            let is_result_post = is_result_path && method == Method::Post;
            // `/report` is the abuse-report path — anyone (user/provider/third
            // party) must be able to file one, so it is exempt from the caller
            // token too (design §3). It is content-free.
            let is_report_post =
                path.first().map(|s| *s == "report").unwrap_or(false) && method == Method::Post;
            let protected = !is_result_post
                && !is_report_post
                && (method == Method::Post
                    || is_result_path
                    || path
                        .first()
                        .map(|s| *s == "artifact" || *s == "reports")
                        .unwrap_or(false));
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
            // Cheap, unauthenticated RTT probe. A client times this GET to measure
            // its OWN round-trip to this access point — used to pick the nearest AP
            // when several are available (item 3, client-side RTT). No chain RPC, no
            // state: safe to hammer.
            (Method::Get, ["ping"]) => {
                json!({"ok":true, "ap": listen, "t": now_secs()})
            }
            (Method::Get, ["providers"]) => {
                let reg = registry.lock().unwrap();
                json!({ "providers": reg.iter().map(|p| json!({
                    "status_url": p.status_url, "address": p.address, "gflops": p.gflops, "models": p.models,
                    "active": p.active, "capacity": p.capacity, "latency_ms": p.latency_ms,
                    "vram_mb": p.vram_mb, "ram_mb": p.ram_mb,
                    "gpu_class": p.gpu_class, "tflops": p.tflops,
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
            // ── Abuse & safety: report path + ban lists (item #22, step 4) ────
            // Anyone may file a report; it is recorded content-free (target +
            // category only — NEVER the prompt/response text, design §5).
            (Method::Post, ["report"]) => {
                let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("account").to_string();
                let target = v.get("target").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let category = v.get("category").and_then(|x| x.as_str()).map(|s| s.to_string());
                let reason = v.get("reason").and_then(|x| x.as_str()).map(|s| s.to_string());
                let source = req
                    .remote_addr()
                    .map(|a| a.ip().to_string())
                    .unwrap_or_else(|| "unknown".into());
                let rep = gpn_agent::abuse::Report {
                    report_id: gpn_agent::abuse::new_report_id(),
                    ts: gpn_agent::abuse::now_secs(),
                    kind,
                    target,
                    category,
                    reason,
                    source: Some(source),
                };
                match rep.validate() {
                    Err(e) => json!({"ok": false, "error": e}),
                    Ok(()) => match gpn_agent::abuse::append_report(&reports_path, &rep) {
                        Ok(()) => {
                            eprintln!(
                                "[abuse] report {} kind={} category={} (recorded)",
                                rep.report_id,
                                rep.kind,
                                rep.category.as_deref().unwrap_or("-")
                            );
                            json!({"ok": true, "report_id": rep.report_id})
                        }
                        Err(e) => json!({"ok": false, "error": format!("could not record report: {e}")}),
                    },
                }
            }
            // Operator view of recent reports (token-gated on non-loopback via
            // the `reports` entry in the auth gate). Content-free.
            (Method::Get, ["reports"]) => {
                let reports = gpn_agent::abuse::read_reports(&reports_path, 50);
                json!({"count": reports.len(), "reports": reports})
            }
            // Operator ban/unban (item #22, step 4). POST => token-gated.
            (Method::Post, ["admin", "ban"]) => {
                let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("ban").to_string();
                let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("account").to_string();
                let value = v.get("value").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                let valid_kind = ["account", "provider", "ip"].contains(&kind.as_str());
                if value.is_empty() || !valid_kind {
                    json!({"ok": false, "error": "need {action: ban|unban, kind: account|provider|ip, value}"})
                } else {
                    let mut d = denylist.lock().unwrap();
                    let changed = match action.as_str() {
                        "unban" | "remove" => d.remove(&kind, &value),
                        _ => match kind.as_str() {
                            "account" => d.add_account(&value),
                            "provider" => d.add_provider(&value),
                            "ip" => d.add_ip(&value),
                            _ => false,
                        },
                    };
                    let total = d.accounts.len() + d.providers.len() + d.ips.len();
                    match d.save(&denylist_path) {
                        Ok(()) => {
                            eprintln!(
                                "[abuse] {action} {kind}={value} (changed={changed}, {total} total bans)"
                            );
                            json!({"ok": true, "action": action, "kind": kind, "value": value,
                                   "changed": changed, "total_bans": total})
                        }
                        Err(e) => json!({"ok": false, "error": format!("could not persist denylist: {e}")}),
                    }
                }
            }
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
                // Abuse guardrail (item #22): moderate the prompt at the AP, the
                // user-facing chokepoint, BEFORE any on-chain create or routing.
                if let Some(cat) = moderation_block(&prompt) {
                    let r = Response::from_string(json!({
                        "error": {"message": cat.reason(), "type": "gpn_policy_violation", "category": cat.id()}
                    }).to_string()).with_header(hdr.clone()).with_status_code(451);
                    let _ = req.respond(r);
                    return;
                }
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
                let chosen = pick(&reg, 0, 0, 0, 0.0, &model, pull_penalty_ms, &rtt_overrides(&v));
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
                // `_create_guard` serializes the create so exactly one is in flight.
                let _create_guard = CREATE_LOCK.lock().unwrap();
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
                    // Serialized by `_create_guard`: exactly ONE create is in flight at a time, so
                    // the process-local prediction IS the real on-chain id (the AP is the sole
                    // creator; the chain assigns ids in submit order). Labeling with the prediction
                    // avoids the concurrent-confirm misattribution (two in-flight creates binding to
                    // one shared NextJobId → a stranded Pending job).
                    let predicted = alloc_predicted_job_id(next_before);
                    match rt.block_on(client.create_job_fast(
                        8192u32, 75u16, 1u8, 0u32, 150u64, "LLMInference", "Spot",
                        (1_000_000_000u128 * 512) as u128, 1_000_000_000u128, 512u32, 1000,
                        workload_hash,
                    )) {
                        Ok(()) => (true, predicted),
                        Err(_) => {
                            // The create may have landed despite a slow/!acked pool submit.
                            // Confirm briefly; only trust it if it matches our prediction.
                            let mut real: Option<u64> = None;
                            let deadline = Instant::now() + Duration::from_millis(2000);
                            while Instant::now() < deadline {
                                let after = rt.block_on(client.get_next_job_id());
                                if after > next_before {
                                    real = Some(after - 1);
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(200));
                            }
                            match real {
                                Some(id) if id == predicted => (true, id),
                                // Did not land: realign the allocator and report failure.
                                _ => {
                                    align_alloc(next_before);
                                    (false, predicted)
                                }
                            }
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
                drop(_create_guard);
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
                    let job_payload = gpn_agent::llm::get_job(id);
                    let callback_token = new_callback_token(id);
                    let _ = write_assignment(&assign_store, id, &provider.address, &model, needs_pull, &callback_url, &callback_token);
                    dispatch_assignment(&provider.status_url, id, &provider.address, &model, needs_pull, job_payload.as_ref(), &callback_url, &callback_token);
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
                // Item #22 build-order step 4: refuse a banned client IP (per-AP
                // denylist). Checked BEFORE any chain RPC or job creation.
                if let Some(ip) = req.remote_addr().map(|a| a.ip().to_string()) {
                    if denylist.lock().unwrap().is_ip_banned(&ip) {
                        let _ = req.respond(
                            Response::from_string(json!({
                                "ok": false,
                                "error": {"message": "service refused (operator denylist)", "type": "gpn_policy_violation"}
                            }).to_string()).with_header(hdr.clone()).with_status_code(403),
                        );
                        return;
                    }
                }
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
                // Hardware floor for this job (item 14): default 0 = no VRAM/RAM
                // requirement; a video/gen job sends e.g. min_vram_mb=16000 so the
                // scheduler skips small-VRAM boxes (the 8GB 4060 OOMs on video).
                let min_vram_mb = g("min_vram_mb", 0);
                let min_ram_mb = g("min_ram_mb", 0);
                // Minimum GPU capability (item 19): a job may declare an FP32
                // TFLOPS floor (`min_tflops`, or env `GPN_MIN_TFLOPS`) so it only
                // routes to a real/discrete GPU that clears it. 0.0 = no floor
                // (gate inert). Integrated GPUs advertise a low estimate and are
                // excluded; a self-declared discrete card with no number yet is
                // trusted (the attestation floor still gates earning on-chain).
                let min_tflops: f64 = v
                    .get("min_tflops")
                    .and_then(|x| x.as_f64())
                    .or_else(|| {
                        std::env::var("GPN_MIN_TFLOPS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                    })
                    .unwrap_or(0.0);
                let price_gpn = v.get("price_gpn").and_then(|x| x.as_f64()).unwrap_or(10.0);
                let job_type = v.get("job_type").and_then(|x| x.as_str()).unwrap_or("LLMInference");
                // Effective prompt: `prompt` (native), `input` (ML), or an
                // OpenAI-style `messages[]`/`system` body. Red-team finding #24
                // (agent/AP input validation / orphan-job DoS): a native
                // `POST /jobs` with no prompt was still created on-chain —
                // escrow locked + a global `ActiveJobCount` and per-requestor
                // slot consumed — but never written to the off-chain store, so
                // no provider could claim it (a guaranteed unservable orphan
                // until the expiry sweep) while the caller got `ok:true`.
                // Reject it up front, BEFORE any on-chain create.
                let prompt = resolve_job_prompt(&v);
                if prompt.is_none() {
                    let r = Response::from_string(
                        json!({
                            "ok": false,
                            "error": "missing prompt: provide `prompt`, `input`, or an OpenAI-style `messages` array"
                        })
                        .to_string(),
                    )
                    .with_header(hdr.clone())
                    .with_status_code(400);
                    let _ = req.respond(r);
                    return;
                }
                // Abuse guardrail (item #22): refuse a prohibited-use prompt at
                // the AP before the on-chain escrow create (fail-closed).
                if let Some(cat) = moderation_block(prompt.as_deref().unwrap_or("")) {
                    let r = Response::from_string(json!({
                        "ok": false,
                        "error": {"message": cat.reason(), "type": "gpn_policy_violation", "category": cat.id()}
                    }).to_string()).with_header(hdr.clone()).with_status_code(451);
                    let _ = req.respond(r);
                    return;
                }

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
                let (provider, needs_pull) = match pick(&reg, min_gflops, min_vram_mb, min_ram_mb, min_tflops, &model, pull_penalty_ms, &rtt_overrides(&v)) {
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
                    min_vram_mb as u32,
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
                            let is_ml_kind = jt == "mlinference";
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
                            } else if is_ml_kind {
                                let task = v
                                    .get("task")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("generate");
                                gpn_agent::llm::LlmJob::new_ml(
                                    &model,
                                    p,
                                    gpn_agent::llm::normalize_ml_task(Some(task)),
                                    0.7,
                                    256,
                                )
                            } else {
                                gpn_agent::llm::LlmJob::new_llm(&model, p, 0.7, 256)
                            };
                            let _ = gpn_agent::llm::LlmStore::new().put(id, job);
                        }
                        // dispatch: record which provider (by address) should claim it,
                        // plus the model + whether it must pull the model first.
                        record_created(id, mtok_eff as u64);
                        let job_payload = gpn_agent::llm::get_job(id);
                        let callback_token = new_callback_token(id);
                        let _ = write_assignment(&assign_store, id, &provider.address, &model, needs_pull, &callback_url, &callback_token);
                        dispatch_assignment(&provider.status_url, id, &provider.address, &model, needs_pull, job_payload.as_ref(), &callback_url, &callback_token);
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
            // POST /result/{id} — the provider→AP result-return channel (item #14).
            // A REMOTE provider runs the job against the payload we pushed in
            // `/assign`, then POSTs the finished result here so THIS AP's own
            // LlmStore learns the outcome and its inline waiter unblocks. Body:
            // {status, response, result_hash, salt, artifact_path, artifact_media_type, error}.
            (Method::Post, ["result", id]) => {
                let idn: u64 = id.parse().unwrap_or(0);
                let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                // Bind the callback to the assignment (item #16, finding #2): the
                // AP issued a per-job token at dispatch and the provider must echo
                // it. Without a match, ANY remote caller could inject/overwrite a
                // job's result (answer poisoning). Loopback stays exempt so local
                // tests/monitors keep working. A job with no recorded token (legacy
                // pre-fix assignment) is accepted for backward-compat.
                let is_loopback = req.remote_addr().map(|a| a.ip().is_loopback()).unwrap_or(false);
                if !is_loopback {
                    if let Some(want) = assignment_token(&assign_store, idn) {
                        let got = v
                            .get("callback_token")
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                            .or_else(|| {
                                req.headers().iter().find_map(|h| {
                                    if h.field.equiv("X-Gpn-Callback-Token") {
                                        Some(h.value.as_str().trim().to_string())
                                    } else {
                                        None
                                    }
                                })
                            })
                            .unwrap_or_default();
                        if !ct_eq(&want, &got) {
                            let _ = req.respond(
                                Response::from_string(json!({"error":"invalid callback token"}).to_string())
                                    .with_header(hdr.clone())
                                    .with_status_code(401),
                            );
                            return;
                        }
                    }
                }
                let status = v.get("status").and_then(|x| x.as_str()).unwrap_or("done");
                let store = gpn_agent::llm::LlmStore::new();
                let res = if status == "error" {
                    store.set_error(idn, v.get("error").and_then(|x| x.as_str()).unwrap_or("remote inference error"))
                } else if let Some(ap) = v.get("artifact_path").and_then(|x| x.as_str()) {
                    store.complete_artifact(
                        idn,
                        ap,
                        v.get("artifact_media_type").and_then(|x| x.as_str()).unwrap_or("application/octet-stream"),
                        v.get("result_hash").and_then(|x| x.as_str()).unwrap_or(""),
                    )
                } else {
                    store.complete(
                        idn,
                        v.get("response").and_then(|x| x.as_str()).unwrap_or(""),
                        v.get("result_hash").and_then(|x| x.as_str()).unwrap_or(""),
                        v.get("salt").and_then(|x| x.as_str()),
                    )
                };
                match res {
                    Ok(_) => {
                        println!("[result] remote provider returned #{idn} ({status})");
                        json!({"ok":true,"job_id":idn})
                    }
                    Err(e) => json!({"error": format!("store: {e}")}),
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
                let route_body: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let qmodel = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("model="))
                    .map(|m| m.replace("%3A", ":").replace("%3a", ":"))
                    .unwrap_or_else(|| "auto".to_string());
                let job_state = rt.block_on(client.get_job_state(idn));
                // Item #22 build-order step 4: enforce the operator denylist over
                // the client IP AND the job's on-chain requestor/provider.
                if let Some(ip) = req.remote_addr().map(|a| a.ip().to_string()) {
                    if denylist.lock().unwrap().is_ip_banned(&ip) {
                        let _ = req.respond(
                            Response::from_string(json!({
                                "ok": false,
                                "error": {"message": "service refused (operator denylist)", "type": "gpn_policy_violation"}
                            }).to_string()).with_header(hdr.clone()).with_status_code(403),
                        );
                        return;
                    }
                }
                if let Some((_st, _pr, req_acct, prov_acct)) = &job_state {
                    let banned = {
                        let d = denylist.lock().unwrap();
                        req_acct.as_deref().map(|a| d.is_account_banned(a)).unwrap_or(false)
                            || prov_acct.as_deref().map(|a| d.is_provider_banned(a)).unwrap_or(false)
                    };
                    if banned {
                        let _ = req.respond(
                            Response::from_string(json!({
                                "ok": false,
                                "error": {"message": "service refused (operator denylist)", "type": "gpn_policy_violation"}
                            }).to_string()).with_header(hdr.clone()).with_status_code(403),
                        );
                        return;
                    }
                }
                if job_state.is_none() {
                    json!({"ok":false,"error":"job not found"})
                } else {
                    let reg = registry.lock().unwrap().clone();
                    match resolve_requested_model(&reg, &qmodel) {
                        Err(e) => json!({"ok":false,"error":e}),
                        Ok(qmodel) => match pick(&reg, 0, 0, 0, 0.0, &qmodel, pull_penalty_ms, &rtt_overrides(&route_body)) {
                            Some((p, pull)) => {
                                // Client-signed path: the prompt lives only in the
                                // caller's body. If they included the off-chain job
                                // (item #14) persist + push it, so a REMOTE provider
                                // can run it; otherwise a remote provider will claim
                                // but find no local prompt.
                                let job_payload = route_body
                                    .get("job")
                                    .and_then(|jv| serde_json::from_value::<gpn_agent::llm::LlmJob>(jv.clone()).ok());
                                // Abuse guardrail (item #22): the routing AP is the
                                // chokepoint; refuse a prohibited-use prompt here
                                // too (client-signed jobs still route through us).
                                if let Some(cat) = job_payload
                                    .as_ref()
                                    .and_then(|j| moderation_block(&j.prompt))
                                {
                                    let r = Response::from_string(json!({
                                        "ok": false,
                                        "error": {"message": cat.reason(), "type": "gpn_policy_violation", "category": cat.id()}
                                    }).to_string()).with_header(hdr.clone()).with_status_code(451);
                                    let _ = req.respond(r);
                                    return;
                                }
                                if let Some(j) = &job_payload {
                                    let _ = gpn_agent::llm::LlmStore::new().put(idn, j.clone());
                                }
                                let callback_token = new_callback_token(idn);
                                let _ = write_assignment(&assign_store, idn, &p.address, &qmodel, pull, &callback_url, &callback_token);
                                dispatch_assignment(&p.status_url, idn, &p.address, &qmodel, pull, job_payload.as_ref(), &callback_url, &callback_token);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(addr: &str, url: &str, latency_ms: u64, model: &str) -> Provider {
        Provider {
            status_url: url.to_string(),
            address: addr.to_string(),
            gflops: 30000,
            models: vec![model.to_string()],
            can_pull: false,
            tok_per_s: 40.0,
            owner_cost_usd_hr: 0.03,
            mem_bw_gbps: 250.0,
            vram_mb: 0,
            ram_mb: 0,
            gpu_class: "unknown".to_string(),
            tflops: 0.0,
            active: 0,
            capacity: 4,
            latency_ms,
            last_seen: Some(Instant::now()),
        }
    }

    #[test]
    fn hex_to_ss58_converts_a_chain_key_to_the_canonical_address() {
        // Bob's AccountId32 as it comes back from a `ProviderEndpoints` storage
        // read (hex) must render as the ss58 address used by the rest of the
        // agent, so a discovered provider is attributable.
        let hex = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";
        assert_eq!(
            hex_to_ss58(hex).as_deref(),
            Some("5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty")
        );
        assert_eq!(hex_to_ss58("not-hex"), None);
        assert_eq!(hex_to_ss58("0xdeadbeef"), None);
    }

    #[test]
    fn attribute_address_falls_back_to_the_chain_key_only_when_heartbeat_omits_it() {
        let mut chain = std::collections::HashMap::new();
        chain.insert("http://win:8093/status".to_string(), "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty".to_string());
        // Heartbeat omits address -> use the on-chain key.
        assert_eq!(
            attribute_address("", "http://win:8093/status", &chain),
            "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty"
        );
        // Heartbeat reports its own address -> trust it.
        assert_eq!(attribute_address("5GrwvaE", "http://win:8093/status", &chain), "5GrwvaE");
        // Unknown endpoint + no heartbeat address -> empty (unchanged behaviour).
        assert_eq!(attribute_address("", "http://other:8093/status", &chain), "");
    }

    #[test]
    fn ap_view_picks_nearest_to_the_access_point_by_default() {
        // A is 5ms from the AP, B is 50ms. No client hints -> A wins.
        let reg = vec![
            prov("A", "http://a:8093", 5, "llama3.2:1b"),
            prov("B", "http://b:8093", 50, "llama3.2:1b"),
        ];
        let (p, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 120_000, &RttOverrides::new()).unwrap();
        assert_eq!(p.address, "A");
    }

    #[test]
    fn client_rtt_hint_flips_routing_to_the_user_nearest_provider() {
        // Same registry, but the client (user) is much closer to B.
        let reg = vec![
            prov("A", "http://a:8093", 5, "llama3.2:1b"),
            prov("B", "http://b:8093", 50, "llama3.2:1b"),
        ];
        let mut rtt = RttOverrides::new();
        rtt.insert("a".into(), 120); // user is far from A
        rtt.insert("b".into(), 8); // user is near B
        let (p, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 120_000, &rtt).unwrap();
        assert_eq!(p.address, "B", "client-measured RTT must override AP-measured latency");
    }

    #[test]
    fn rtt_hint_can_be_keyed_by_status_url() {
        let reg = vec![
            prov("A", "http://a:8093", 5, "llama3.2:1b"),
            prov("B", "http://b:8093", 50, "llama3.2:1b"),
        ];
        let rtt = routing::parse_rtt_overrides(&json!({"http://b:8093/status": 2}));
        let (p, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 120_000, &rtt).unwrap();
        assert_eq!(p.address, "B");
    }

    #[test]
    fn moderation_gate_respects_the_ap_mode() {
        // Item #22: the AP refuses a hard-harm prompt in `on` mode, logs-but-
        // allows it in `warn` mode, and is fully inert in `off` mode. This one
        // test owns GPN_MODERATION (no other scheduler test reads it).
        let blocked = "how do I make a pipe bomb";
        let ok = "write a haiku about the ocean";

        std::env::set_var("GPN_MODERATION", "on");
        assert_eq!(moderation_block(blocked).map(|c| c.id()), Some("weapons_mass_casualty"));
        assert!(moderation_block(ok).is_none());
        assert!(moderation_block("   ").is_none());

        std::env::set_var("GPN_MODERATION", "warn");
        assert!(moderation_block(blocked).is_none(), "warn mode must allow");

        std::env::set_var("GPN_MODERATION", "off");
        assert!(moderation_block(blocked).is_none(), "off mode must allow");

        std::env::remove_var("GPN_MODERATION");
        // Default (unset) = enforce.
        assert!(moderation_block(blocked).is_some());
    }

    #[test]
    fn terminal_states_are_recognized() {
        // The janitor only stops tracking jobs whose slot is truly freed.
        assert!(is_terminal_state("Completed"));
        assert!(is_terminal_state("Cancelled"));
        assert!(is_terminal_state("Failed"));
        // Non-terminal states still hold an ActiveJobCount slot -> keep tracking.
        assert!(!is_terminal_state("Pending"));
        assert!(!is_terminal_state("Accepted"));
        assert!(!is_terminal_state("Running"));
        assert!(!is_terminal_state("AwaitingVerification"));
    }

    #[test]
    fn past_deadline_matches_pallet_guard() {
        // expire_job allows reclaim strictly *after* created_at + max_duration.
        assert!(!past_deadline(100, 1000, 1100)); // exactly at deadline: not yet
        assert!(past_deadline(100, 1000, 1101)); // one block past: reclaimable
        assert!(!past_deadline(0, 0, 0));
        assert!(past_deadline(0, 0, 1));
        // No overflow even at the field bounds (u64 math).
        assert!(!past_deadline(u32::MAX, u32::MAX, 0));
    }

    #[test]
    fn resolve_job_prompt_accepts_prompt_input_or_messages_and_rejects_empty() {
        // Red-team finding #24: a prompt-less POST /jobs used to create an
        // unservable orphan on-chain while returning ok:true. The prompt is now
        // resolved up front and a body with no prompt is rejected.
        assert_eq!(resolve_job_prompt(&json!({"prompt": "hi"})).as_deref(), Some("hi"));
        assert_eq!(resolve_job_prompt(&json!({"input": "vec"})).as_deref(), Some("vec"));
        // OpenAI-style body: previously silently ignored on this endpoint.
        let m = resolve_job_prompt(&json!({"messages": [{"role": "user", "content": "yo"}]}))
            .expect("messages body must yield a prompt");
        assert!(m.contains("user: yo"));
        // A system-only body also yields a prompt.
        assert!(resolve_job_prompt(&json!({"system": "be nice"})).is_some());
        // Blank / absent prompt is rejected before any on-chain create.
        assert_eq!(resolve_job_prompt(&json!({"prompt": "   "})), None);
        assert_eq!(resolve_job_prompt(&json!({"messages": []})), None);
        assert_eq!(resolve_job_prompt(&json!({})), None);
        // `prompt` wins over `input` when both are present.
        assert_eq!(
            resolve_job_prompt(&json!({"prompt": "P", "input": "I"})).as_deref(),
            Some("P")
        );
    }

    #[test]
    fn zero_override_does_not_disturb_ap_ordering() {
        let reg = vec![
            prov("A", "http://a:8093", 5, "llama3.2:1b"),
            prov("B", "http://b:8093", 50, "llama3.2:1b"),
        ];
        let mut rtt = RttOverrides::new();
        rtt.insert("b".into(), 0); // "not measured"
        let (p, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 120_000, &rtt).unwrap();
        assert_eq!(p.address, "A");
    }

    #[test]
    fn pick_gates_on_advertised_vram() {
        // item 14: a video-class job (min_vram 16 GB) must skip the 8 GB node.
        let mk = |addr: &str, url: &str, lat: u64, vram: u64| {
            let mut p = prov(addr, url, lat, "ltx-video");
            p.vram_mb = vram;
            p
        };
        let reg = vec![mk("small", "http://s", 5, 8188), mk("big", "http://b", 50, 24576)];
        // With a 16 GB floor, only the big node qualifies even though `small` is faster.
        let (p, _) = pick(&reg, 0, 16000, 0, 0.0, "ltx-video", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p.address, "big", "16GB job must skip the 8GB node");
        // No floor -> the faster small node wins (gate inactive).
        let (p2, _) = pick(&reg, 0, 0, 0, 0.0, "ltx-video", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p2.address, "small");
    }

    #[test]
    fn pick_excludes_providers_that_cannot_meet_the_floor() {
        // A VRAM/RAM floor is a HARD requirement, so the gate fails closed: an
        // 8GB node AND an unadvertised (0 = unknown) node are both excluded from a
        // 16GB job -> no candidate.
        let mut small = prov("small", "http://s", 5, "ltx-video");
        small.vram_mb = 8188;
        let unknown = prov("unknown", "http://u", 1, "ltx-video"); // vram/ram = 0
        let reg = vec![small, unknown];
        assert!(
            pick(&reg, 0, 16000, 0, 0.0, "ltx-video", 0, &RttOverrides::new()).is_none(),
            "16GB job must find no candidate among an 8GB and an unadvertised node"
        );

        // A RAM floor is enforced the same way.
        let mut low = prov("low", "http://l", 5, "llama3.2:1b");
        low.ram_mb = 8000;
        let mut hi = prov("hi", "http://h", 50, "llama3.2:1b");
        hi.ram_mb = 64000;
        let reg2 = vec![low, hi];
        let (p, _) = pick(&reg2, 0, 0, 32000, 0.0, "llama3.2:1b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p.address, "hi", "32GB RAM floor must skip the 8GB-RAM node");

        // No floor -> the gate is inert (the faster unadvertised node still wins).
        let reg3 = vec![
            prov("s2", "http://s2", 50, "ltx-video"),
            prov("u2", "http://u2", 5, "ltx-video"),
        ];
        let (p2, _) = pick(&reg3, 0, 0, 0, 0.0, "ltx-video", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p2.address, "u2");
    }

    #[test]
    fn pick_gates_on_advertised_tflops() {
        // item 19: a job declaring a 3 TFLOPS floor must skip an APU/integrated
        // GPU even though it is the faster/nearest node.
        let mk = |addr: &str, url: &str, lat: u64, class: &str, tflops: f64| {
            let mut p = prov(addr, url, lat, "llama3.2:1b");
            p.gpu_class = class.to_string();
            p.tflops = tflops;
            p
        };
        let reg = vec![
            mk("apu", "http://a", 5, "integrated", 1.8), // 780M-class iGPU
            mk("dgpu", "http://d", 50, "discrete", 15.1), // RTX 4060
        ];
        // With a 3 TFLOPS floor the integrated node is excluded despite being faster.
        let (p, _) = pick(&reg, 0, 0, 0, 3.0, "llama3.2:1b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p.address, "dgpu", "3 TFLOPS job must skip the integrated GPU");
        // No floor -> the faster integrated node wins (gate inert).
        let (p2, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p2.address, "apu");

        // A known-but-too-low tflops is excluded even if it claims "discrete"...
        let weak = mk("weak", "http://w", 1, "discrete", 2.0);
        assert!(pick(&vec![weak.clone()], 0, 0, 0, 3.0, "llama3.2:1b", 0, &RttOverrides::new()).is_none());
        // ...and an *unknown* tflops (0) is trusted ONLY when self-declared discrete.
        let unknown_disc = mk("ud", "http://ud", 1, "discrete", 0.0);
        assert!(pick(&vec![unknown_disc], 0, 0, 0, 3.0, "llama3.2:1b", 0, &RttOverrides::new()).is_some());
        let unknown_integ = mk("ui", "http://ui", 1, "integrated", 0.0);
        assert!(pick(&vec![unknown_integ], 0, 0, 0, 3.0, "llama3.2:1b", 0, &RttOverrides::new()).is_none());
    }

    #[test]
    fn write_assignment_records_the_callback_for_result_return() {
        // Item #14: the assignment record must carry the AP callback URL so a
        // remote provider knows where to POST the finished result.
        let p = std::env::temp_dir().join(format!("gpn-assign-test-{}.json", std::process::id()));
        let ps = p.to_string_lossy().to_string();
        write_assignment(&ps, 42, "5Bob", "qwen2.5:7b", true, "http://gpn-testnet.duckdns.org:8092", "tok-abc").unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&ps).unwrap()).unwrap();
        assert_eq!(v["42"]["provider"], "5Bob");
        assert_eq!(v["42"]["model"], "qwen2.5:7b");
        assert_eq!(v["42"]["pull"], true);
        assert_eq!(v["42"]["callback"], "http://gpn-testnet.duckdns.org:8092");
        assert_eq!(v["42"]["callback_token"], "tok-abc");
        // The per-job callback token is read back for the POST /result auth check.
        assert_eq!(assignment_token(&ps, 42).as_deref(), Some("tok-abc"));
        assert_eq!(assignment_token(&ps, 43), None);
        let _ = std::fs::remove_file(&ps);
    }

    #[test]
    fn callback_token_is_unguessable_and_constant_time_compared() {
        let a = new_callback_token(7);
        let b = new_callback_token(7);
        assert_eq!(a.len(), 64, "32 bytes hex-encoded");
        assert_ne!(a, b, "two tokens for the same job must differ (OS entropy)");
        assert!(ct_eq(&a, &a));
        assert!(!ct_eq(&a, &b));
        assert!(!ct_eq(&a, ""));
        assert_eq!(assignment_token("/nonexistent/gpn-assign.json", 1), None);
    }

    #[test]
    fn assignment_push_carries_the_shared_token_that_binds_it_to_the_provider() {
        let body = assignment_body(7, "prov", "llama3.2:1b", false, None, "http://ap:8092", "ctok", "s3cret");
        assert_eq!(body["assign_token"], "s3cret");
        assert_eq!(body["callback_token"], "ctok");
        assert_eq!(body["job_id"], 7);
        // Empty token still serializes (provider treats it as "no secret configured").
        let body2 = assignment_body(7, "prov", "m", false, None, "http://ap", "ctok", "");
        assert_eq!(body2["assign_token"], "");
    }

    #[test]
    fn assignment_job_payload_roundtrips_as_llmjob() {
        // The AP serializes the full off-chain LlmJob into the `/assign` push and
        // the provider heartbeat deserializes it back into its own store. Guard
        // that wire shape (item #14 remote-provider run path).
        let job = gpn_agent::llm::LlmJob::new_llm("qwen2.5:7b", "hello world", 0.7, 256);
        let v = serde_json::to_value(&job).unwrap();
        let back: gpn_agent::llm::LlmJob = serde_json::from_value(v).unwrap();
        assert_eq!(back.kind, "llm");
        assert_eq!(back.model, "qwen2.5:7b");
        assert_eq!(back.prompt, "hello world");
        assert_eq!(back.status, "queued");
    }
}
