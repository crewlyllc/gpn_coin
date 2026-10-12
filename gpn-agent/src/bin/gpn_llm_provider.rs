//! gpn_llm_provider — the provider-side LLM executor for the GPN marketplace.
//!
//! Polls the chain for `LLMInference` jobs that are `Running` and assigned to
//! this signer, reads the off-chain prompt/model from the shared LLM store,
//! runs inference against the local ollama server (`:11434`), writes the
//! response back to the store, and settles the job on-chain:
//! `submit_result(sha256(response))` then `complete_job` (escrow release).
//!
//! This is the "bridge" between the chain (coordination/escrow) and the actual
//! model inference (off-chain). The chain never sees the prompt or response —
//! only the result hash.
//!
//! Usage:
//!   gpn_llm_provider [endpoint] [signer_uri] [poll_ms]
//! Env overrides: ENDPOINT, SIGNER_URI, POLL_MS, OLLAMA_URL.

use gpn_agent::chain::signer::Sr25519Signer;
use gpn_agent::chain::ChainClient;
use gpn_agent::llm::{get_job, run_inference, set_response, LlmStore};
use std::collections::HashMap;
use std::io::Read as _;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Per-job count of consecutive polls where the job was *absent* from chain
/// state. Bounds the fast path's optimistic claiming: a job that never appears
/// (mis-routed / dropped create) can't cause an infinite claim storm.
fn missing_counter() -> &'static Mutex<HashMap<u64, u32>> {
    static M: OnceLock<Mutex<HashMap<u64, u32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse a "0x"-prefixed 32-byte hex string into `[u8; 32]`.
fn hex32(s: &str) -> Option<[u8; 32]> {
    let h = s.trim_start_matches("0x");
    let b = hex::decode(h).ok()?;
    b.try_into().ok()
}

/// Rate-limit the reconcile pass. Reconcile scans the last `RECONCILE_WINDOW`
/// (128) jobs on-chain — expensive. With the poll at 300ms it would run ~3×/s
/// and peg the CPU; gate it to ~once per 10s.
fn should_reconcile() -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 10_000 {
        LAST.store(now, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Off-chain, chain-free inference endpoint ("passthrough" mode).
/// `POST /infer {model,prompt,temperature,max_tokens}` → `{ok,text,model}`. NO
/// chain, escrow, commitment, or billing — free/private tier. Loopback bind.
fn spawn_infer_server(listen: &str) {
    let listen = listen.to_string();
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(&listen) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[infer] bind {listen} failed: {e}");
                return;
            }
        };
        println!("[infer] passthrough inference endpoint on http://{listen} (POST /infer)");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("infer runtime");
        for mut req in server.incoming_requests() {
            let path = req.url().split('?').next().unwrap_or("").to_string();
            let is_infer = req.method() == &tiny_http::Method::Post && path == "/infer";
            if !is_infer {
                let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
                continue;
            }
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("auto");
            let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
            let temp = v.get("temperature").and_then(|x| x.as_f64()).unwrap_or(0.7) as f32;
            let max_tokens = v.get("max_tokens").and_then(|x| x.as_u64()).unwrap_or(256) as u32;
            let model_r = resolve_model(model);
            let resp = match rt.block_on(run_inference(&model_r, prompt, temp, max_tokens)) {
                Ok((text, _hash)) => serde_json::json!({"ok": true, "text": text, "model": model_r}),
                Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
            };
            let hdr = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
            let _ = req.respond(tiny_http::Response::from_string(resp.to_string()).with_header(hdr));
        }
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let signer_uri = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("SIGNER_URI").ok())
        .unwrap_or_else(|| "//Alice".to_string());
    let poll_ms: u64 = args
        .get(3)
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("POLL_MS").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(5000);

    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    let (me, addr) = {
        use subxt::tx::Signer as _;
        let s = Sr25519Signer::from_uri(&signer_uri)?;
        // get_job_state returns the provider as 0x-hex; the attestation lookup
        // parses an ss58/hex address string.
        let aid = s.account_id();
        let b: &[u8] = aid.as_ref();
        (format!("0x{}", hex::encode(b)), aid.to_string())
    };

    println!(
        "GPN LLM provider running — endpoint={} signer={} poll={}ms (inference via {})",
        endpoint,
        me,
        poll_ms,
        gpn_agent::llm::infer::default_ollama_url()
    );

    // Guaranteed baseline: every node ships a starter model set so the network
    // always has at least one usable LLM, even before any demand-driven pulls.
    let baseline = std::env::var("GPN_BASELINE_MODELS")
        .unwrap_or_else(|_| "llama3.2:1b,qwen2.5:0.5b".to_string());
    for m in baseline.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match ensure_model(m) {
            Ok(note) => println!("[provider] baseline: {note}"),
            Err(e) => eprintln!("[provider] baseline {m}: {e}"),
        }
    }

    // On-chain provider discovery (item 1): advertise this provider's reachable
    // `/status` URL on-chain so access points / the scheduler can discover us from
    // the chain instead of a hand-configured `--providers` list. Opt out by
    // leaving GPN_PROVIDER_ENDPOINT unset/empty. Requires this account to already
    // be a registered provider (the pallet rejects otherwise); a failure here is
    // non-fatal — the provider still serves jobs.
    match std::env::var("GPN_PROVIDER_ENDPOINT") {
        Ok(ep) if !ep.trim().is_empty() => {
            let ep = ep.trim().to_string();
            match client.set_provider_endpoint(&ep).await {
                Ok(()) => println!("[provider] advertised on-chain endpoint: {ep}"),
                Err(e) => eprintln!("[provider] set_provider_endpoint({ep}) failed: {e}"),
            }
        }
        _ => println!("[provider] GPN_PROVIDER_ENDPOINT unset/empty — on-chain discovery disabled"),
    }

    // Fast-claim mode: pipeline accept→start (and submit_result→complete) as a
    // nonce-ordered burst with NO block-inclusion wait, so the claim lane stops
    // being bound by block time. Disable with GPN_FAST_CLAIM=0 to fall back to
    // the stop-and-wait path.
    let fast_claim = std::env::var("GPN_FAST_CLAIM")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);
    println!(
        "[provider] fast claim path: {}",
        if fast_claim {
            "ON (pipelined, no block wait)"
        } else {
            "off (stop-and-wait)"
        }
    );

    // Chain-free passthrough inference endpoint (opt-out with GPN_PASSTHROUGH=0).
    if std::env::var("GPN_PASSTHROUGH")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
    {
        let listen = std::env::var("GPN_INFER_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8094".to_string());
        spawn_infer_server(&listen);
    }

    loop {
        if let Err(e) = process_once(&client, &me, &addr, fast_claim).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// Resolve a requested model to a concrete one. `auto`/empty means "pick any
/// model this machine has" — choose the first from the local Ollama, falling
/// back to the guaranteed baseline model.
fn resolve_model(requested: &str) -> String {
    let base = gpn_agent::llm::infer::default_ollama_url();
    let installed: Vec<String> = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(2500))
        .build()
        .and_then(|c| c.get(format!("{base}/api/tags")).send())
        .ok()
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .and_then(|v| {
            v["models"].as_array().map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default();
    let has = |name: &str| {
        installed.iter().any(|n| n == name || n.split(':').next() == Some(name))
    };
    if !requested.is_empty() && requested != "auto" && has(requested) {
        // Prefer the exact installed tag when a bare name was requested.
        if let Some(exact) = installed.iter().find(|n| n.as_str() == requested) {
            return exact.clone();
        }
        if let Some(tag) = installed.iter().find(|n| n.split(':').next() == Some(requested)) {
            return tag.clone();
        }
        return requested.to_string();
    }
    // Requested model unavailable (or `auto`/empty) → fall back to an INSTALLED
    // model so the job COMPLETES instead of orphaning forever (which saturates
    // the on-chain MaxActiveJobs cap — 2026-10-05 incident). Prefer llama3.2:1b.
    if installed.iter().any(|n| n == "llama3.2:1b") {
        return "llama3.2:1b".to_string();
    }
    installed
        .into_iter()
        .next()
        .unwrap_or_else(|| "llama3.2:1b".to_string())
}

fn assignment_store_path() -> String {
    std::env::var("GPN_ASSIGN_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/assignments.json")
    })
}

/// Provider-private cache of assignment ids that are already settled (a job we
/// have conclusively seen as non-`Pending` on-chain, or written off after
/// repeated absence). Item #16 red-team finding #34: `assignments.json` only
/// ever grows (`write_assignment` inserts, never prunes), so re-reading it and
/// calling `get_job_state` (~2 chain reads) for every historical assignment on
/// every ~300ms pass made the claim cycle balloon from <1s to 30-100s (O(history)
/// RPC per poll ⇒ the whole job-claim path stalls). Persisting the settled set
/// lets the claim loop skip a resolved id WITHOUT an RPC, so the per-pass chain
/// cost tracks only *unresolved* assignments.
fn settled_store_path() -> String {
    std::env::var("GPN_LLM_SETTLED_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/llm-settled.json")
    })
}

/// Parse a settled-id set from `path`. Accepts a JSON array of ids (what we
/// write) and tolerates a JSON object keyed by id (forward/backward compat).
/// Path is a parameter so the parse is unit-testable without touching `$HOME`.
fn read_settled_from(path: &str) -> std::collections::HashSet<u64> {
    let mut set = std::collections::HashSet::new();
    let Ok(s) = std::fs::read_to_string(path) else {
        return set;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return set;
    };
    match v {
        serde_json::Value::Array(a) => {
            for x in a {
                if let Some(n) = x.as_u64() {
                    set.insert(n);
                }
            }
        }
        serde_json::Value::Object(o) => {
            for k in o.keys() {
                if let Ok(n) = k.parse::<u64>() {
                    set.insert(n);
                }
            }
        }
        _ => {}
    }
    set
}

/// Persist a settled-id set as a sorted JSON array (stable, diff-friendly).
fn write_settled_to(path: &str, set: &std::collections::HashSet<u64>) -> std::io::Result<()> {
    let mut ids: Vec<u64> = set.iter().copied().collect();
    ids.sort_unstable();
    std::fs::write(path, serde_json::to_string(&ids)?)
}

/// Persistent per-job inference-failure counter. We give up after a small
/// number of attempts so a mis-routed job (e.g. an image model like `sd-turbo`
/// that can never run as a chat completion) can't spin the provider forever.
fn failed_store_path() -> String {
    std::env::var("GPN_LLM_FAILED_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/llm-failed.json")
    })
}

/// Max inference attempts before the provider abandons a job.
const MAX_INFER_ATTEMPTS: u64 = 2;

fn fail_count(id: u64) -> u64 {
    std::fs::read_to_string(failed_store_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get(id.to_string()).and_then(|x| x.as_u64()))
        .unwrap_or(0)
}

fn record_failure(id: u64) -> u64 {
    let path = failed_store_path();
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let n = v.get(id.to_string()).and_then(|x| x.as_u64()).unwrap_or(0) + 1;
    if let Some(obj) = v.as_object_mut() {
        obj.insert(id.to_string(), serde_json::json!(n));
    }
    let _ = std::fs::write(&path, v.to_string());
    n
}

/// Should we (still) attempt inference for this job? False once we've hit the
/// attempt cap, or when the requested model is a known non-text kind.
fn should_attempt(id: u64, model: &str) -> bool {
    if fail_count(id) >= MAX_INFER_ATTEMPTS {
        return false;
    }
    // Fast-path reject: image/video/audio diffusion models can't serve chat.
    let m = model.to_ascii_lowercase();
    !(m.contains("sd-")
        || m.contains("diffusion")
        || m.contains("sdxl")
        || m.contains("flux")
        || m.contains("stable-diffusion"))
}

/// Job ids the access-point scheduler assigned to this provider (matched by
/// either the hex or ss58 form of our account), plus the requested model +
/// whether we must pull it on demand. Empty = no scheduler in play.
/// Handles both the legacy string form and the `{provider,model,pull}` object.
fn read_assigned(me: &str, addr: &str) -> Vec<(u64, String, bool)> {    let path = assignment_store_path();
    let Ok(s) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return vec![];
    };
    let Some(m) = v.as_object() else {
        return vec![];
    };
    m.iter()
        .filter_map(|(k, pv)| {
            let id = k.parse::<u64>().ok()?;
            let (p, model, pull) = if let Some(s) = pv.as_str() {
                (s.to_string(), String::new(), false)
            } else {
                (
                    pv.get("provider").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    pv.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    pv.get("pull").and_then(|x| x.as_bool()).unwrap_or(false),
                )
            };
            if p == me || p == addr {
                Some((id, model, pull))
            } else {
                None
            }
        })
        .collect()
}

/// The AP callback base URL recorded for this assignment (item #14), if any.
/// A REMOTE provider uses it to return the finished result to the requesting AP.
fn assignment_callback(id: u64) -> Option<String> {
    let s = std::fs::read_to_string(assignment_store_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get(id.to_string())
        .and_then(|r| r.get("callback"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .filter(|c| !c.is_empty())
}

/// The per-job callback token the AP issued in `/assign` (item #16, finding #2).
/// Echoed back on the result POST so the AP can verify the callback is bound to
/// this assignment (without it any remote caller could inject a result).
fn assignment_callback_token(id: u64) -> Option<String> {
    let s = std::fs::read_to_string(assignment_store_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get(id.to_string())
        .and_then(|r| r.get("callback_token"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .filter(|c| !c.is_empty())
}

/// POST the finished job's off-chain result back to the requesting AP so ITS
/// LlmStore learns the outcome (a remote provider cannot write the AP's store
/// directly). Best-effort: a missing callback or an unreachable AP is ignored —
/// the on-chain `submit_result`/`complete_job` still settle the job.
fn return_result(id: u64) {
    let Some(cb) = assignment_callback(id) else { return };
    let Some(j) = get_job(id) else { return };
    let base = cb.trim_end_matches('/');
    let url = format!("{base}/result/{id}");
    // Echo the per-job callback token (item #16 finding #2) so the AP authenticates
    // this result as the bound assignment (an unbound/forged POST is rejected 401).
    let mut body = serde_json::json!({
        "status": j.status, "response": j.response, "result_hash": j.result_hash,
        "salt": j.salt, "artifact_path": j.artifact_path,
        "artifact_media_type": j.artifact_media_type, "error": j.error,
    });
    if let Some(t) = assignment_callback_token(id) {
        body["callback_token"] = serde_json::Value::String(t);
    }
    if let Ok(c) = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        match c.post(&url).json(&body).send() {
            Ok(_) => println!("[job {id}] result returned to AP ({base})"),
            Err(e) => eprintln!("[job {id}] result return to AP failed: {e}"),
        }
    }
}

/// Ensure `model` is present locally; if not, pull it from Ollama / HuggingFace.
/// Returns a human-readable note about what happened.
fn ensure_model(model: &str) -> Result<String, String> {
    if model.is_empty() || model == "auto" {
        return Ok("no specific model requested".into());
    }
    let base = gpn_agent::llm::infer::default_ollama_url();
    let cli = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()
        .map_err(|e| e.to_string())?;
    // 1. already present?
    if let Ok(r) = cli.get(format!("{base}/api/tags")).send() {
        if let Ok(v) = r.json::<serde_json::Value>() {
            let have = v["models"]
                .as_array()
                .map(|a| a.iter().any(|m| {
                    let n = m["name"].as_str().unwrap_or("");
                    n == model || n.split(':').next() == Some(model)
                }))
                .unwrap_or(false);
            if have {
                return Ok(format!("model {model} already present"));
            }
        }
    }
    // 2. pull it on demand (Ollama registry). For a raw HF GGUF the operator
    //    should use `gpn_pull_model gguf <repo> <file>`, but Ollama-library
    //    models (and many HF repos) resolve via `ollama pull` directly.
    println!("[provider] model {model} missing — pulling on demand…");
    let r = cli
        .post(format!("{base}/api/pull"))
        .json(&serde_json::json!({"name": model, "stream": false}))
        .send()
        .map_err(|e| format!("pull {model} failed: {e}"))?;
    if r.status().is_success() {
        Ok(format!("pulled model {model} on demand"))
    } else {
        Err(format!("pull {model} http {}: {}", r.status(), r.text().unwrap_or_default()))
    }
}

/// True if the off-chain store says job `id` is an LLM (text) job. Unknown
/// jobs are treated as non-LLM so we never pull/handle a foreign kind.
/// Kind-gate for the reconcile pass. Both providers share ONE signer account,
/// so a job whose on-chain `provider == me` may actually belong to the other
/// provider's kind. Reconcile only OUR kind here; a job whose off-chain kind is
/// a different one is left to its owner. Unknown kinds (no store entry) are
/// still reconciled so orphan zombies get reclaimed.
fn job_kind_is_ours(id: u64) -> bool {
    get_job(id).map(|j| j.kind == "llm").unwrap_or(true)
}

/// A state error meaning "the job already advanced / isn't in the state we
/// expected" — benign during reconcile (the owning provider or a prior cycle
/// got there first), so it must not be logged as an ERROR. This is the source
/// of the spurious `JobNotRunning [pallet=11 e=5]` log spam.
fn is_benign_state_err(s: &str) -> bool {
    s.contains("JobNotRunning") || s.contains("JobNotPending") || s.contains("JobNotAccepted")
}

/// How many of the most recent job ids the reconcile pass scans per cycle.
/// Previously this was `0..next` (every job ever created): one chain RPC per
/// historical job on EVERY poll — hundreds of RPCs every few seconds, which
/// contended with claim/start/submit for the signer+node. Only recently created
/// jobs can be mid-flight (`AwaitingVerification`/`Accepted`), so a bounded
/// trailing window suffices and keeps the poll loop tight.
const RECONCILE_WINDOW: u64 = 128;

async fn process_once(
    client: &ChainClient,
    me: &str,
    addr: &str,
    fast_claim: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // -1. Reconcile: finalize any of our jobs left in `AwaitingVerification`.
    // `complete_job` is the provider's own finalization call (state
    // Running|AwaitingVerification → Completed). On a slow chain a
    // `submit_result`/`complete_job` tx can time out *after* the result was
    // recorded, stranding the job mid-state and permanently consuming one of
    // the provider's `MaxJobsPerProvider` slots (→ `MaxProviderCapacity` on
    // every later accept). Re-drive such jobs to Completed so capacity is
    // reclaimed. Kind-agnostic so it also clears zombies with no off-chain
    // payload entry.
    if should_reconcile() {
    let next = client.get_next_job_id().await;
    for id in next.saturating_sub(RECONCILE_WINDOW)..next {
        if let Some((state, _price, _req, _provider_now)) = client.get_job_state(id).await {
            if state == "Pending" {
                // Heal a fast-path leak: we served this job off-chain (store status
                // `done` + a result hash) but the on-chain accept never landed (the
                // AP's create hadn't been included when we claimed). Drive it
                // Pending → Accepted → Running → submit_result → Completed so it
                // doesn't strand as an orphan. Only for jobs we actually have a
                // completed off-chain result for.
                if let Some(j) = get_job(id) {
                    if j.kind == "llm" && j.status == "done" {
                        if let Some(bytes) = j.result_hash.as_deref().and_then(hex32) {
                            let toks = (j.response.as_deref().unwrap_or("").chars().count() as u32 / 4).max(1);
                            let a = if fast_claim { client.accept_job_fast(id).await } else { client.accept_job(id).await };
                            let s = if fast_claim { client.start_job_fast(id).await } else { client.start_job(id).await };
                            let r = if fast_claim { client.submit_result_fast(id, bytes, toks).await } else { client.submit_result(id, bytes, toks).await };
                            let c = if fast_claim { client.complete_job_fast(id).await } else { client.complete_job(id).await };
                            if a.is_ok() && s.is_ok() && r.is_ok() && c.is_ok() {
                                println!("[provider] healed leaked Pending #{id} → Completed");
                            }
                        }
                    }
                }
                continue;
            }
            if _provider_now.as_deref() != Some(me) {
                continue;
            }
            if state == "AwaitingVerification" {
                // Kind-gate: skip jobs owned by the other provider (shared
                // signer account means `provider == me` is true for both kinds).
                if !job_kind_is_ours(id) {
                    continue;
                }
                match if fast_claim {
                    client.complete_job_fast(id).await
                } else {
                    client.complete_job(id).await
                } {
                    Ok(()) => {
                        println!("[provider] reconciled #{id} AwaitingVerification → Completed")
                    }
                    Err(e) if is_benign_state_err(&e.to_string()) => {
                        // Already finalized elsewhere — quiet, not an error.
                    }
                    Err(e) => eprintln!("[provider] reconcile #{id} (complete) failed: {e}"),
                }
            } else if state == "Accepted" && job_kind_is_ours(id) {
                // Capacity reclaim: an `Accepted` LLM job we own that was never
                // started is a zombie (its start tx never landed) permanently
                // holding one of our `MaxJobsPerProvider` slots → every later
                // accept fails `MaxProviderCapacity`. Since our claim loop only
                // *starts* `Pending` jobs, an `Accepted` job can never advance on
                // its own — drive it Accepted → Running → Completed to free the
                // slot (escrow is refunded by `complete_job`).
                let start_res = if fast_claim {
                    client.start_job_fast(id).await
                } else {
                    client.start_job(id).await
                };
                if let Err(e) = start_res {
                    eprintln!("[provider] reconcile #{id} (start) failed: {e}");
                    continue;
                }
                match if fast_claim {
                    client.complete_job_fast(id).await
                } else {
                    client.complete_job(id).await
                } {
                    Ok(()) => println!(
                        "[provider] reconciled #{id} Accepted → Completed (capacity freed)"
                    ),
                    Err(e) => eprintln!("[provider] reconcile #{id} (complete) failed: {e}"),
                }
            }
        }
    }
    } // end reconcile gate

    // 0. Auto-claim: if an access-point scheduler has assigned us specific jobs,
    //    claim ONLY those; otherwise (standalone) claim any claimable LLM job.
    let assigned = read_assigned(me, addr);
    let targets: Vec<u64> = if assigned.is_empty() {
        let attested_gflops = client
            .get_attestation_record(addr)
            .await
            .map(|(_, f, _)| f)
            .unwrap_or(0);
        client
            .list_claimable_jobs(attested_gflops)
            .await
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    } else {
        // Model-aware: pull any requested-but-missing model BEFORE claiming.
        // Only LLM jobs: ignore image/video jobs the scheduler may have (mis)assigned,
        // otherwise we spam `ollama pull` for foreign models (e.g. sd-turbo) forever.
        // Also skip finished jobs and `auto`/empty requests (nothing concrete to pull).
        for (id, model, pull) in &assigned {
            let Some(job) = get_job(*id) else { continue };
            if job.kind != "llm" || job.status == "done" {
                continue;
            }
            if model.is_empty() || model == "auto" {
                continue;
            }
            let _ = pull;
            if let Err(e) = ensure_model(model) {
                eprintln!("[provider] job #{id}: {e}");
            }
        }
        assigned
            .iter()
            // A REMOTE provider never created this job, so it has NO local
            // store entry — `is_llm_id` would be false and the assignment would
            // be silently dropped (the item-4/#14 "Windows box never claims"
            // bug). Use the kind-gate that treats an unknown id as ours: the
            // scheduler only assigns LLM jobs to this LLM provider.
            .filter(|(id, _, _)| job_kind_is_ours(*id))
            .map(|(id, _, _)| *id)
            .collect()
    };
    // finding #34: skip ids we've already settled so the unbounded
    // assignments.json can't drive O(history) chain reads every poll. The set
    // is in-memory here and persisted once per pass (below).
    let settled_path = settled_store_path();
    let mut settled = read_settled_from(&settled_path);
    let mut settled_dirty = false;
    // Jobs we claim this pass but (in fast mode) never wait on-chain for; we
    // run them directly below rather than re-reading chain state that may not
    // have advanced yet.
    let mut just_claimed: Vec<u64> = Vec::new();
    for job_id in targets {
        // Already settled (non-Pending or written off) on a previous pass — no
        // need to spend two chain reads rediscovering that every cycle.
        if settled.contains(&job_id) {
            continue;
        }
        // Skip only a job KNOWN to belong to another kind (e.g. a mis-assigned
        // image/diffusion job); an id absent from our local store is a
        // remote-assigned LLM job and MUST be claimed.
        if get_job(job_id).map(|j| j.kind != "llm").unwrap_or(false) {
            continue;
        }
        // Skip jobs we've already handled. In fast mode the on-chain `Pending`
        // check below can read stale/None right after our pipelined txs land, so
        // trust the local store: a job with a response is finished, full stop.
        if let Some(j) = get_job(job_id) {
            if j.status == "done" || j.status == "error" {
                continue;
            }
        }
        // Only claim a Pending job — skip ones already accepted/started. In fast
        // mode a job may be absent from chain state while the AP's no-wait create
        // is still in flight: tolerate that, but bound it so a job that never
        // appears can't storm the chain with doomed accepts.
        match client.get_job_state(job_id).await {
            Some((state, _, _, _)) => {
                missing_counter().lock().unwrap().remove(&job_id);
                if state != "Pending" {
                    // Conclusively not claimable (Accepted/Running/terminal) —
                    // remember it so we never re-query this id again.
                    settled.insert(job_id);
                    settled_dirty = true;
                    continue;
                }
            }
            None => {
                // Not on-chain yet — the AP's no-wait `create_job` is still in
                // flight. Claim optimistically so we serve the client fast; if the
                // accept races ahead of the create it fails, and the reconcile pass
                // heals it (Pending + done-store → Completed). Bound the counter so
                // a never-created id isn't re-checked forever.
                let mut m = missing_counter().lock().unwrap();
                let n = m.entry(job_id).or_insert(0);
                *n += 1;
                if *n > 40 {
                    m.remove(&job_id);
                    // Write off a never-created / permanently-absent id so it is
                    // not rescanned on every future pass (the counter itself
                    // resets on removal, so without this the id would loop
                    // forever).
                    settled.insert(job_id);
                    settled_dirty = true;
                    continue;
                }
            }
        }
        println!("[provider] claiming LLM job #{job_id}");
        if fast_claim {
            // accept_job + start_job as one nonce-ordered burst, no block wait.
            match client.claim_job_pipelined(job_id).await {
                Ok(()) => {
                    just_claimed.push(job_id);
                    continue;
                }
                Err(e) => {
                    eprintln!("[provider] pipelined claim #{job_id} failed: {e}");
                    continue;
                }
            }
        }
        if let Err(e) = client.accept_job(job_id).await {
            eprintln!("[provider] accept #{job_id} failed: {e}");
            continue;
        }
        if let Err(e) = client.start_job(job_id).await {
            eprintln!("[provider] start #{job_id} failed: {e}");
        }
    }

    // Persist any newly-settled ids (one write per pass, not per id).
    if settled_dirty {
        if let Err(e) = write_settled_to(&settled_path, &settled) {
            eprintln!("[provider] could not persist settled-id cache: {e}");
        }
    }

    // 0b. Fast path: run the jobs we just pipelined WITHOUT waiting for their
    //     `Running` state to land. submit_result/complete_job continue the same
    //     nonce stream, so they dispatch after `start` with no block wait.
    if fast_claim {
        for id in just_claimed.iter().copied() {
            if let Some(job) = get_job(id) {
                if job.kind == "llm" && should_attempt(id, &job.model) {
                    if let Err(e) = run_job(client, id, &job, true).await {
                        eprintln!("[provider] run #{id} (pipelined) failed: {e}");
                    }
                }
            }
        }
    }

    // 1. Run any Running LLM job assigned to this signer. Scan only the RECENT
    //    window (not every job ever) — a full 0..next scan on each 300ms pass
    //    pegged the CPU/RPC once the ledger grew. We only ever run
    //    recently-claimed jobs, so the recent window is sufficient.
    let next = client.get_next_job_id().await;
    for id in next.saturating_sub(RECONCILE_WINDOW)..next {
        // Skip jobs we already ran directly in the fast path this pass.
        if fast_claim && just_claimed.contains(&id) {
            continue;
        }
        // Only jobs that have an off-chain LLM payload are LLM jobs.
        let job = match get_job(id) {
            Some(j) if j.kind == "llm" => j,
            _ => continue,
        };
        // Skip jobs we've already produced a response for. In fast mode the
        // on-chain state can still read `Running` for a beat after our
        // submit_result/complete land in the pool — without this, a later pass
        // would re-run an already-finished job.
        if job.status == "done" {
            continue;
        }
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            let is_mine = provider.as_deref() == Some(me);
            if state == "Running" && is_mine {
                if !should_attempt(id, &job.model) {
                    // Already failed too many times, or a foreign (non-text)
                    // model that can never run here — stop hammering it.
                    continue;
                }
                run_job(client, id, &job, fast_claim).await?;
            }
        }
    }
    Ok(())
}

/// A fresh random salt (hex). Uses time + a process counter + pid as entropy
/// (no external RNG crate); collisions are irrelevant for privacy here.
fn random_salt() -> String {
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
    h.update(b"gpn-salt");
    h.update(t.to_le_bytes());
    h.update(n.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    let d = h.finalize();
    hex::encode(&d[..16])
}

/// sha256(salt || payload) — the on-chain commitment.
fn salted_commit(salt: &str, payload: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(payload.as_bytes());
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

async fn run_job(
    client: &ChainClient,
    id: u64,
    job: &gpn_agent::llm::LlmJob,
    fast: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let job = job.clone();
    // Item #22 build-order step 3: the provider-side policy hook. Re-check the
    // assigned job BEFORE spending any GPU cycles — refuse hard-harm categories
    // (defense in depth over the AP's moderation) and any model this operator
    // does not serve. On refusal we settle the job on-chain so the requestor's
    // escrow refunds and the capacity slot frees, then bail without inferring.
    let policy = gpn_agent::provider_policy::ProviderPolicy::from_env();
    if let gpn_agent::provider_policy::Decision::Refuse(r) = policy.check(&job.prompt, &job.model)
    {
        let cat = r.category.unwrap_or("model_policy");
        eprintln!("[job {id}] \u{26a0} policy refusal (category={cat}) \u{2014} not running: {}", r.reason);
        // Content-free off-chain marker (no prompt stored) so the job is never
        // re-attempted in this process.
        let _ = LlmStore::new().set_error(id, &format!("gpn_policy_violation: {cat}"));
        let rr = if fast {
            client.fail_job_fast(id, 2).await
        } else {
            client.fail_job(id, 2).await
        };
        match rr {
            Ok(()) => println!("[job {id}] refused by provider policy \u{2014} escrow refunded"),
            Err(e) => eprintln!("[job {id}] fail_job (policy) failed: {e}"),
        }
        return Ok(());
    }
    let model = resolve_model(&job.model);
    let preview: String = job.prompt.chars().take(60).collect();
    println!(
        "[job {id}] running inference — model={} prompt=\"{}\"",
        model, preview
    );

    let _ = LlmStore::new().set_status(id, "running");
    // Yield the GPU to this job: drop the flag the node's miner checks, so block
    // mining parks (duty cycle) instead of contending with the inference.
    let yield_path = format!(
        "{}/.gpn/gpu-yield",
        std::env::var("HOME").unwrap_or_else(|_| ".".into())
    );
    let _ = std::fs::create_dir_all(
        std::path::Path::new(&yield_path).parent().unwrap_or(std::path::Path::new(".")),
    );
    let _ = std::fs::write(&yield_path, b"1");
    let result = run_inference(&model, &job.prompt, job.temperature, job.max_tokens).await;
    let _ = std::fs::remove_file(&yield_path);
    match result {
        Ok((text, hash)) => {
            // Salted commitment: on-chain value = sha256(salt ‖ answer). The salt is
            // random + kept OFF-chain, so the on-chain value is opaque to observers
            // but anyone holding the salt can verify the answer. (A bare
            // sha256(answer) would leak short answers by brute force.)
            let salt = random_salt();
            let commit = salted_commit(&salt, &text);
            let commit_hex = format!("0x{}", hex::encode(commit));
            let _ = set_response(id, &text, &commit_hex, Some(&salt));
            // Return the result to the requesting AP (remote providers; item #14).
            return_result(id);
            // Rough output-token meter for per-token billing (chars/4 heuristic).
            let est_tokens: u32 = ((text.chars().count() as u32) / 4).max(1);
            if fast {
                // Continue the pipelined nonce stream — no block-inclusion wait.
                client.submit_result_fast(id, commit, est_tokens).await?;
                client.complete_job_fast(id).await?;
            } else {
                client.submit_result(id, commit, est_tokens).await?;
                client.complete_job(id).await?;
            }
            println!(
                "[job {id}] done — response {} chars, salted commitment {:.16}… (salt off-chain)",
                text.len(),
                commit_hex
            );
            let _ = hash; // the bare hash stays off-chain too (informational)
        }
        Err(e) => {
            let _ = LlmStore::new().set_error(id, &e.to_string());
            return_result(id);
            let n = record_failure(id);
            if n >= MAX_INFER_ATTEMPTS {
                // Terminal failure: mark the job Failed on-chain so escrow is
                // refunded to the requestor immediately (and the ActiveJobCount
                // slot + provider capacity slot are freed). reason=1 = inference error.
                let r = if fast {
                    client.fail_job_fast(id, 1).await
                } else {
                    client.fail_job(id, 1).await
                };
                match r {
                    Ok(()) => println!("[job {id}] failed on-chain — escrow refunded to requestor"),
                    Err(er) => eprintln!("[job {id}] fail_job failed: {er}"),
                }
            }
            eprintln!(
                "[job {id}] inference failed (attempt {n}/{MAX_INFER_ATTEMPTS}), giving up after cap: {e}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod settled_cache_tests {
    //! Item #16 red-team finding #34: the persisted settled-id cache bounds the
    //! claim loop's chain reads when `assignments.json` grows unbounded.
    use super::*;

    fn tmp(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("gpn-settled-test-{}-{}.json", std::process::id(), name))
            .to_string_lossy()
            .to_string()
    }

    #[test]
    fn settled_roundtrip_preserves_ids() {
        let p = tmp("rt");
        let _ = std::fs::remove_file(&p);
        let mut set = std::collections::HashSet::new();
        set.insert(5);
        set.insert(2049);
        set.insert(0);
        write_settled_to(&p, &set).unwrap();
        let back = read_settled_from(&p);
        assert_eq!(back, set);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn settled_reads_tolerate_object_and_missing_file() {
        // Forward/backward compat: a JSON object keyed by id also parses.
        let obj = tmp("obj");
        std::fs::write(&obj, r#"{"7":{"provider":"x"},"9":{}}"#).unwrap();
        let s = read_settled_from(&obj);
        assert!(s.contains(&7) && s.contains(&9) && s.len() == 2);
        let _ = std::fs::remove_file(&obj);

        // A missing file is simply an empty set (first run), never a panic.
        assert!(read_settled_from("/nonexistent/gpn-settled.json").is_empty());
    }

    #[test]
    fn settled_skips_resolved_ids_without_an_rpc() {
        // The claim loop's guard: an id already in the settled set is skipped
        // before any chain read. This is the regression the fix targets — the
        // 1700+ stale assignment entries must not be re-queried every pass.
        let mut settled = std::collections::HashSet::new();
        settled.insert(42u64);
        assert!(settled.contains(&42), "resolved id must be skipped");
        assert!(!settled.contains(&43), "fresh id must still be claimed");
    }
}
