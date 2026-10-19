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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Warn **once** (not every poll) that ollama is missing, so a box without it
/// logs a single clear, actionable message instead of spamming pull failures.
static OLLAMA_WARNED: AtomicBool = AtomicBool::new(false);
fn warn_ollama_missing_once() {
    if !OLLAMA_WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "[provider] ⚠️  {}",
            gpn_agent::llm::infer::ollama_missing_hint()
        );
    }
}

/// True when the error string is the "ollama not running" sentinel (which was
/// already warned about), so callers stay quiet for it.
fn is_ollama_unavailable(e: &str) -> bool {
    e == gpn_agent::llm::infer::OLLAMA_UNAVAILABLE
}

/// Per-job count of consecutive polls where the job was *absent* from chain
/// state. Bounds the fast path's optimistic claiming: a job that never appears
/// (mis-routed / dropped create) can't cause an infinite claim storm.
fn missing_counter() -> &'static Mutex<HashMap<u64, u32>> {
    static M: OnceLock<Mutex<HashMap<u64, u32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Item #41: ids already warned about being payload-less (warn once each, not
/// once per 500 ms poll).
fn payloadless_warned() -> &'static Mutex<std::collections::HashSet<u64>> {
    static W: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Decision for one candidate job at the claim boundary (item #41).
///
/// A provider may only claim a job whose off-chain payload it holds locally:
/// the AP writes the payload BEFORE dispatching / pushing `/assign`, so an
/// assignment without a payload means that write was lost. Such a job can
/// never be run by us — claiming it burns `accept`+`start` extrinsics on every
/// poll (68 duplicate claims measured on #5629) and wedges it `Running` with
/// the escrow locked until expiry.
#[derive(Debug, PartialEq, Eq)]
enum ClaimGate {
    /// Payload present — claim normally.
    Claim,
    /// No payload, already warned about this id — skip silently.
    SkipSilently,
    /// No payload, first sighting — skip and log once.
    SkipAndWarn,
}

fn payload_gate(has_payload: bool, first_sighting: bool) -> ClaimGate {
    if has_payload {
        ClaimGate::Claim
    } else if first_sighting {
        ClaimGate::SkipAndWarn
    } else {
        ClaimGate::SkipSilently
    }
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
                let _ = req
                    .respond(tiny_http::Response::from_string("not found").with_status_code(404));
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
                Ok((text, _hash)) => {
                    serde_json::json!({"ok": true, "text": text, "model": model_r})
                }
                Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
            };
            let hdr = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                .unwrap();
            let _ =
                req.respond(tiny_http::Response::from_string(resp.to_string()).with_header(hdr));
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

    // #46 Phase 1 — provider-PULL assignments. The AP can only push to us if our
    // heartbeat port is inbound-reachable; behind NAT/CGNAT it never is. The pull
    // loop asks the AP for our own assignments instead and writes them into the
    // same stores the claim loop already reads (push stays on unless
    // GPN_ASSIGN_SOURCE=pull).
    if let Some(cfg) = gpn_agent::assign_pull::PullConfig::from_env(&me) {
        gpn_agent::assign_pull::spawn(cfg);
    }

    // Guaranteed baseline: every node ships a starter model set so the network
    // always has at least one usable LLM, even before any demand-driven pulls.
    let baseline = std::env::var("GPN_BASELINE_MODELS")
        .unwrap_or_else(|_| "llama3.2:1b,qwen2.5:0.5b".to_string());
    for m in baseline.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match ensure_model(m) {
            Ok(note) => println!("[provider] baseline: {note}"),
            Err(e) if is_ollama_unavailable(&e) => {}
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
            // Bounded: this is a non-fatal advert, but `submit_signed_call` waits
            // for INCLUDED-IN-BEST-BLOCK (up to its 300 s tx-inclusion timeout).
            // Restarting during a block-import stall (single-peer ban loop) made
            // startup block here for minutes: no claim loop, no infer server, one
            // request timeout after another. Cap it so serving starts regardless;
            // the advert retries on the next restart.
            match tokio::time::timeout(
                std::time::Duration::from_secs(15),
                client.set_provider_endpoint(&ep),
            )
            .await
            {
                Ok(Ok(())) => println!("[provider] advertised on-chain endpoint: {ep}"),
                Ok(Err(e)) => eprintln!("[provider] set_provider_endpoint({ep}) failed: {e}"),
                Err(_) => eprintln!(
                    "[provider] set_provider_endpoint({ep}) timed out after 15s (chain stalled?) — continuing startup"
                ),
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
        let listen =
            std::env::var("GPN_INFER_LISTEN").unwrap_or_else(|_| "127.0.0.1:8094".to_string());
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
        installed
            .iter()
            .any(|n| n == name || n.split(':').next() == Some(name))
    };
    if !requested.is_empty() && requested != "auto" && has(requested) {
        // Prefer the exact installed tag when a bare name was requested.
        if let Some(exact) = installed.iter().find(|n| n.as_str() == requested) {
            return exact.clone();
        }
        if let Some(tag) = installed
            .iter()
            .find(|n| n.split(':').next() == Some(requested))
        {
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

/// Parse a settled-id set from `path`, dropping any id that has not yet been
/// issued by the chain (`>= chain_tip`, i.e. the live `ComputeMarketplace::
/// NextJobId`).
///
/// Item #29: an id is only ever *settled* once it exists on-chain, so an id at
/// or above the chain tip is necessarily a **predicted off-chain label** that
/// ran ahead of the chain (the AP's label-vs-`NextJobId` drift). Persisting such
/// a write-off blacklists a *future real job id* forever — job #4858 was skipped
/// exactly this way (written off at 16:29 when the tip was ~4773). Filtering at
/// load time heals every poisoned entry without touching the file by hand: as
/// soon as the tip is below an id it stops being trusted as settled.
///
/// Backward compat: a legacy bare JSON array (or a JSON object keyed by id) is
/// still accepted, and is filtered by the same tip rule. A `chain_tip` of 0
/// (chain unreachable) drops everything — a single pass re-queries rather than
/// risk hiding a real job, and the cache is rewritten from what we re-learn.
fn read_settled_from_with_tip(path: &str, chain_tip: u64) -> (std::collections::HashSet<u64>, bool) {
    let mut set = std::collections::HashSet::new();
    let mut dropped = false;
    let Ok(s) = std::fs::read_to_string(path) else {
        return (set, dropped);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return (set, dropped);
    };
    let mut keep = |n: u64| {
        if n < chain_tip {
            set.insert(n);
        } else {
            dropped = true;
        }
    };
    match v {
        serde_json::Value::Array(a) => {
            for x in a {
                if let Some(n) = x.as_u64() {
                    keep(n);
                }
            }
        }
        serde_json::Value::Object(o) => {
            // New form: {"watermark":N,"ids":[...]}; legacy form: {id: ...}.
            if let Some(ids) = o.get("ids").and_then(|x| x.as_array()) {
                for x in ids {
                    if let Some(n) = x.as_u64() {
                        keep(n);
                    }
                }
            } else {
                for k in o.keys() {
                    if let Ok(n) = k.parse::<u64>() {
                        keep(n);
                    }
                }
            }
        }
        _ => {}
    }
    (set, dropped)
}

/// Persist a settled-id set stamped with the chain tip observed at write time
/// (`{"watermark":N,"ids":[…]}`), sorted for stable, diff-friendly output.
/// The watermark is informational — the load-side trust rule uses the *current*
/// tip — but it makes a poisoned write-off auditable after the fact.
fn write_settled_to(
    path: &str,
    set: &std::collections::HashSet<u64>,
    chain_tip: u64,
) -> std::io::Result<()> {
    let mut ids: Vec<u64> = set.iter().copied().filter(|id| *id < chain_tip).collect();
    ids.sort_unstable();
    let doc = serde_json::json!({ "watermark": chain_tip, "ids": ids });
    std::fs::write(path, serde_json::to_string(&doc)?)
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
fn read_assigned(me: &str, addr: &str) -> Vec<(u64, String, bool)> {
    let path = assignment_store_path();
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
                    pv.get("provider")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    pv.get("model")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
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

/// Resolve the AP base URL to POST the finished result back to.
///
/// Precedence: `GPN_RESULT_URL` (full base, e.g. `http://127.0.0.1:8092`) >
/// `GPN_AP_URL` (alias) > the AP-advertised `callback` recorded on the
/// assignment. When the AP is **co-located** with the provider, pinning the
/// loopback address removes the public-DNS dependency entirely (a host whose
/// resolver does not answer AAAA otherwise stalls this callback — the
/// "host-DNS /result defect", see `docs/run-a-node.md`). Remote providers keep
/// using the advertised callback.
fn result_callback_base(advertised: &str) -> String {
    for var in ["GPN_RESULT_URL", "GPN_AP_URL"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                // Allow either a bare base or a full `.../result` path.
                return v
                    .trim_end_matches('/')
                    .trim_end_matches("/result")
                    .trim_end_matches('/')
                    .to_string();
            }
        }
    }
    advertised.trim_end_matches('/').to_string()
}

/// POST the finished job's off-chain result back to the requesting AP so ITS
/// LlmStore learns the outcome (a remote provider cannot write the AP's store
/// directly). Best-effort: a missing callback or an unreachable AP is ignored —
/// the on-chain `submit_result`/`complete_job` still settle the job.
fn return_result(id: u64) {
    let Some(cb) = assignment_callback(id) else {
        return;
    };
    let Some(j) = get_job(id) else { return };
    let base = result_callback_base(&cb);
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
    // IPv4-only resolver so an AAAA-laggy host resolver cannot stall this 5s
    // best-effort callback (see src/net.rs).
    if let Ok(c) = gpn_agent::net::ipv4_only_client(std::time::Duration::from_secs(5)) {
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
    // Preflight: without ollama there is nothing to pull. Warn once, clearly,
    // and bail with a sentinel the callers suppress (no per-poll spam).
    if !gpn_agent::llm::infer::ollama_reachable() {
        warn_ollama_missing_once();
        return Err(gpn_agent::llm::infer::OLLAMA_UNAVAILABLE.to_string());
    }
    // 1. already present?
    if let Ok(r) = cli.get(format!("{base}/api/tags")).send() {
        if let Ok(v) = r.json::<serde_json::Value>() {
            let have = v["models"]
                .as_array()
                .map(|a| {
                    a.iter().any(|m| {
                        let n = m["name"].as_str().unwrap_or("");
                        n == model || n.split(':').next() == Some(model)
                    })
                })
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
        Err(format!(
            "pull {model} http {}: {}",
            r.status(),
            r.text().unwrap_or_default()
        ))
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

/// How long to wait before re-submitting the SAME heal transition for the same
/// job. `submit_fast` is pool-ack only, so a caller cannot tell whether the tx
/// it just broadcast will be included; at ~15–18 s block times a retry inside
/// one block interval would allocate a **fresh nonce** for work that is still
/// queued, run the shared nonce stream ahead of the chain, and churn the pool
/// (this is what turned one leak into 2–5 identical 4-tx bursts per job).
/// 45 s ≈ 2–3 block intervals: long enough for inclusion to show up as a state
/// change, short enough that a genuinely dropped tx is retried promptly.
const HEAL_RETRY_COOLDOWN: Duration = Duration::from_secs(45);

/// Absolute fallback: if the chain head has NOT advanced (an import stall) we
/// still retry after this long, so a genuinely dropped tx cannot wedge a job
/// forever. `HEAL_RETRY_COOLDOWN` alone was **wall-clock only**, and this chain
/// stalls: measured over 3 h, 6.4% of block intervals exceed 45 s (max 159 s),
/// so the gate expired against an unchanged head and re-submitted the same
/// transition with a fresh nonce (`accept`/`start`/`complete` duplicates).
const HEAL_RETRY_FLOOR: Duration = Duration::from_secs(120);

/// Absolute ceiling on the backoff applied when a heal tx is *included* but
/// leaves the on-chain state unchanged. That means the dispatch **reverted**
/// inside the block (e.g. `accept_job` rejected for provider capacity while the
/// single GPU slot was busy): the nonce is consumed, nothing happens, and the
/// plain 45 s cadence simply re-submits the identical call — measured as ~14–20%
/// duplicated heal lines with **no diagnostic line at all**, which made the
/// retries look spurious. Each ineffective attempt doubles the wait up to this
/// cap (45 → 90 → 180 → 360 s), so a persistently reverting step stops burning
/// nonces and pool slots while still retrying for liveness.
const HEAL_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(360);

/// Gate state for one `(job id, transition)`.
#[derive(Debug, Clone, Copy)]
struct HealAttempt {
    /// When the transition was last submitted (or last admitted for submission).
    last: Instant,
    /// Nonce the tx was signed with. The nonce (not the head number) is the
    /// inclusion evidence: a block being produced says nothing about *our* tx
    /// (it may still be queued behind other people's extrinsics), whereas the
    /// account nonce only advances once the txs below it have been applied.
    /// `None` = not yet known (or a slow-path submit that already waited).
    nonce: Option<u64>,
    /// How many consecutive attempts were *included* and left the state
    /// unchanged (dispatch reverted). Drives the backoff + the diagnostic.
    ineffective: u32,
}

/// Last attempt per `(job id, transition)`. Only touched from the single
/// reconcile pass, but a `Mutex` keeps it sound if the loop is ever parallelised.
fn heal_gate() -> &'static Mutex<HashMap<(u64, HealStep), HealAttempt>> {
    static GATE: OnceLock<Mutex<HashMap<(u64, HealStep), HealAttempt>>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn heal_step_name(step: HealStep) -> &'static str {
    match step {
        HealStep::Accept => "accept",
        HealStep::Start => "start",
        HealStep::SubmitResult => "result submitted",
        HealStep::Complete => "completion",
        HealStep::Wait => "wait",
    }
}

/// Wait required before re-submitting the same transition, given how many prior
/// attempts were included without effect: 45 s, then ×2 per revert, capped.
fn heal_backoff(ineffective: u32) -> Duration {
    let d = HEAL_RETRY_COOLDOWN * (1u32 << ineffective.min(3));
    if d > HEAL_RETRY_MAX_BACKOFF {
        HEAL_RETRY_MAX_BACKOFF
    } else {
        d
    }
}

/// Pure retry decision. With inclusion evidence (our tx landed) retry once the
/// (backoff-scaled) cooldown has elapsed; without it, only after the absolute
/// floor — i.e. assume a queued tx is still coming, but never wedge forever.
fn heal_retry_decision(elapsed: Duration, inclusion_evidence: bool, ineffective: u32) -> bool {
    if inclusion_evidence {
        return elapsed >= heal_backoff(ineffective);
    }
    if elapsed < HEAL_RETRY_COOLDOWN {
        return false;
    }
    elapsed >= HEAL_RETRY_FLOOR
}

/// True when this `(job, transition)` may be submitted now; records the attempt
/// so the next tick cannot re-fire it inside the cooldown, and a tx still queued
/// behind a full block cannot re-fire it until the account nonce proves it
/// landed (or `HEAL_RETRY_FLOOR` passes).
///
/// When the previous tx *did* land and the state still has not moved, the
/// dispatch must have reverted: log that once (it used to be completely silent)
/// and escalate the backoff instead of re-submitting at the plain cadence.
fn heal_attempt_allowed(id: u64, step: HealStep, account_nonce: Option<u64>) -> bool {
    let now = Instant::now();
    let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
    // Drop entries for finished steps/nonexistent ids now and then so the map
    // stays tiny (it is keyed by (id, step), not by history).
    if gate.len() > 512 {
        gate.retain(|_, a| now.duration_since(a.last) < HEAL_RETRY_FLOOR);
    }
    match gate.get(&(id, step)).copied() {
        Some(prev) => {
            let elapsed = now.duration_since(prev.last);
            // `System::Account.nonce` is the NEXT nonce to use, so a tx signed
            // with `n` is included only once the account nonce is strictly > n.
            let landed = match (prev.nonce, account_nonce) {
                (Some(n), Some(cur)) => cur > n,
                _ => false,
            };
            if !heal_retry_decision(elapsed, landed, prev.ineffective) {
                return false;
            }
            let ineffective = if landed { prev.ineffective + 1 } else { prev.ineffective };
            gate.insert(
                (id, step),
                HealAttempt { last: now, nonce: None, ineffective },
            );
            if landed {
                eprintln!(
                    "[provider] heal #{id} ({}): previous tx was included but the state did not move — dispatch reverted; next retry backs off {}s (ineffective attempt {})",
                    heal_step_name(step),
                    heal_backoff(ineffective).as_secs(),
                    ineffective
                );
            }
            true
        }
        None => {
            gate.insert(
                (id, step),
                HealAttempt { last: now, nonce: None, ineffective: 0 },
            );
            true
        }
    }
}

/// Record the nonce a heal transition was submitted with, so the next pass can
/// tell whether it was included. Called only right after a successful submit.
fn heal_record_nonce(id: u64, step: HealStep, nonce: u64) {
    let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = gate.get(&(id, step)).copied() {
        gate.insert(
            (id, step),
            HealAttempt { nonce: Some(nonce), ..prev },
        );
    }
}

/// Drop one `(job, step)` throttle entry. Used when a submit was **rejected**
/// (nothing entered the pool), so there is no queued tx to wait for and the
/// next reconcile pass may try again immediately.
fn heal_clear_step(id: u64, step: HealStep) {
    let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
    gate.remove(&(id, step));
}

/// Forget a job's throttle state once it is out of the heal path (terminal, or
/// owned by another provider), so the map cannot grow without bound.
fn heal_forget(id: u64) {
    let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
    gate.retain(|(jid, _), _| *jid != id);
}

/// What the reconcile pass should do next for a job whose off-chain result is
/// already `done` but whose on-chain state has not caught up.
///
/// HISTORY (root cause of the 2–5×-per-id `healed leaked Pending` churn): the
/// heal branch used to fire the WHOLE `accept → start → submit_result →
/// complete` burst on every reconcile tick. With the fast path each call is
/// `submit_fast` = pool-ack only (no inclusion check), so a burst fired while
/// its predecessors were still queued could not land as a unit; the job stayed
/// `Pending`, the next 10 s tick re-fired all four calls, and the code only
/// logged on *complete* success (`if a.is_ok() && s.is_ok() && …`), so any
/// failing leg — and the retry itself — was silent. Result: 2–5 duplicate
/// 4-tx bursts per job (~240 wasted extrinsics/h) with zero diagnostic output.
///
/// FIX: mirror the real state machine — submit exactly ONE transition per
/// tick, chosen from the state we just read. Each tick costs at most one
/// extrinsic, a leg that did not land is simply retried (idempotent: the
/// pallet answers `JobNotPending`/`JobNotAccepted`, which is benign), and every
/// failure is logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HealStep {
    Accept,
    Start,
    SubmitResult,
    Complete,
    Wait,
}

/// Next heal transition for an on-chain state, or `Wait` if the job is not on
/// the heal path (terminal / unknown states).
fn heal_step_for_state(state: &str) -> HealStep {
    match state {
        "Pending" => HealStep::Accept,
        "Accepted" => HealStep::Start,
        "Running" => HealStep::SubmitResult,
        "AwaitingVerification" => HealStep::Complete,
        _ => HealStep::Wait,
    }
}

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
        // Account nonce at the start of this pass: the heal gate keys retries on
        // the nonce each attempted tx was signed with. A block being produced is
        // NOT evidence that our tx landed (it may be queued behind other people's
        // extrinsics), but the account nonce only advances once the txs below it
        // are applied. `None` (unreadable) is conservative — it never counts as
        // inclusion.
        let account_nonce = client.account_nonce_at_head(me).await;
        for id in next.saturating_sub(RECONCILE_WINDOW)..next {
            if let Some((state, _price, _req, provider_now)) = client.get_job_state(id).await {
                // Heal a fast-path leak: we served this job off-chain (store status
                // `done` + a result hash) but the on-chain state never caught up (the
                // AP's create/accept had not been included when we claimed). Walk it
                // one transition per tick toward `Completed` so it doesn't strand as
                // an orphan AND the provider still gets paid per token.
                let heal = get_job(id).filter(|j| {
                    j.kind == "llm"
                        && j.status == "done"
                        && j.result_hash.as_deref().and_then(hex32).is_some()
                });

                let step = heal_step_for_state(&state);

                if step == HealStep::Accept {
                    // `provider == me` cannot be checked here: a job on this path is
                    // still `Pending` (no provider assigned), which is exactly why the
                    // accept has to be re-driven.
                    if heal.is_some() && heal_attempt_allowed(id, step, account_nonce) {
                        let res = if fast_claim {
                            client.accept_job_fast_nonce(id).await.map(Some)
                        } else {
                            client.accept_job(id).await.map(|_| None)
                        };
                        match res {
                            Ok(nonce) => {
                                if let Some(n) = nonce {
                                    heal_record_nonce(id, step, n);
                                }
                                println!("[provider] heal #{id}: accept submitted (Pending)")
                            }
                            Err(e) if is_benign_state_err(&e.to_string()) => {
                                heal_clear_step(id, step)
                            }
                            Err(e) => {
                                heal_clear_step(id, step);
                                eprintln!("[provider] heal #{id} (accept) failed: {e}")
                            }
                        }
                    }
                    continue;
                }
                if provider_now.as_deref() != Some(me) {
                    heal_forget(id);
                    continue;
                }
                if step == HealStep::SubmitResult {
                    // Only meaningful for a job we already produced a result for;
                    // a result-less `Running` job is the job's own claim loop's
                    // business (we cannot invent a result hash for it).
                    if let Some(j) = &heal {
                        if job_kind_is_ours(id) && heal_attempt_allowed(id, step, account_nonce) {
                            let bytes = j.result_hash.as_deref().and_then(hex32).unwrap();
                            let toks = (j.response.as_deref().unwrap_or("").chars().count() as u32
                                / 4)
                            .max(1);
                            let res = if fast_claim {
                                client.submit_result_fast_nonce(id, bytes, toks).await.map(Some)
                            } else {
                                client.submit_result(id, bytes, toks).await.map(|_| None)
                            };
                            match res {
                                Ok(nonce) => {
                                    if let Some(n) = nonce {
                                        heal_record_nonce(id, step, n);
                                    }
                                    println!("[provider] heal #{id}: result submitted (Running)")
                                }
                                Err(e) if is_benign_state_err(&e.to_string()) => {
                                    heal_clear_step(id, step)
                                }
                                Err(e) => {
                                    heal_clear_step(id, step);
                                    eprintln!("[provider] heal #{id} (submit_result) failed: {e}")
                                }
                            }
                        }
                    }
                } else if step == HealStep::Complete {
                    // Kind-gate: skip jobs owned by the other provider (shared
                    // signer account means `provider == me` is true for both kinds).
                    if !job_kind_is_ours(id) {
                        continue;
                    }
                    if !heal_attempt_allowed(id, step, account_nonce) {
                        continue;
                    }
                    let res = if fast_claim {
                        client.complete_job_fast_nonce(id).await.map(Some)
                    } else {
                        client.complete_job(id).await.map(|_| None)
                    };
                    match res {
                        Ok(nonce) => {
                            if let Some(n) = nonce {
                                heal_record_nonce(id, step, n);
                            }
                            // submit_fast is pool-ack only: keep the (id, Complete)
                            // cooldown instead of forgetting it. Otherwise the next
                            // 10 s tick re-reads a chain head that has not yet
                            // included this tx (esp. during block-import stalls)
                            // and fires a duplicate completion, burning nonces and
                            // pool slots. The terminal branch below clears the
                            // entry once the chain actually reports Completed.
                            println!("[provider] reconcile #{id}: completion submitted (AwaitingVerification)");
                        }
                        Err(e) if is_benign_state_err(&e.to_string()) => {
                            // Already finalized elsewhere — quiet, not an error.
                            heal_clear_step(id, step);
                        }
                        Err(e) => {
                            heal_clear_step(id, step);
                            eprintln!("[provider] reconcile #{id} (complete) failed: {e}")
                        }
                    }
                } else if step == HealStep::Start && job_kind_is_ours(id) {
                    // Start ONE transition. Two cases:
                    //  (a) we already produced a result (`heal`) ⇒ start only; the
                    //      next tick sees `Running` and submits the result, so the
                    //      provider is paid per token instead of the job being
                    //      completed result-less for a full refund.
                    //  (b) plain zombie (no result) ⇒ start + complete as before, to
                    //      reclaim the `MaxJobsPerProvider` slot.
                    if !heal_attempt_allowed(id, step, account_nonce) {
                        continue;
                    }
                    let start_res = if fast_claim {
                        client.start_job_fast_nonce(id).await.map(Some)
                    } else {
                        client.start_job(id).await.map(|_| None)
                    };
                    let start_nonce = match start_res {
                        Ok(n) => n,
                        Err(e) => {
                            heal_clear_step(id, step);
                            if !is_benign_state_err(&e.to_string()) {
                                eprintln!("[provider] reconcile #{id} (start) failed: {e}");
                            }
                            continue;
                        }
                    };
                    if let Some(n) = start_nonce {
                        heal_record_nonce(id, step, n);
                    }
                    if heal.is_some() {
                        println!("[provider] heal #{id}: started (Accepted)");
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
                // Terminal / unknown: drop the throttle entry so the map stays small.
                if matches!(state.as_str(), "Completed" | "Failed" | "Cancelled") {
                    heal_forget(id);
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
                if !is_ollama_unavailable(&e) {
                    eprintln!("[provider] job #{id}: {e}");
                }
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
    //
    // Item #29: only trust ids the chain has actually issued (id < NextJobId);
    // a write-off above the tip is a predicted label and must never blacklist a
    // future real job.
    let settled_path = settled_store_path();
    let chain_tip = client.get_next_job_id().await;
    let (mut settled, pruned) = read_settled_from_with_tip(&settled_path, chain_tip);
    let mut settled_dirty = pruned;
    // Write-offs for ids the chain has NOT issued yet (predicted labels from the
    // AP's +drift). They are skipped in memory only, never persisted: one that
    // later turns out to be a real job id must not be blacklisted (#29).
    let mut predicted_writeoff: std::collections::HashSet<u64> = std::collections::HashSet::new();
    // Jobs we claim this pass but (in fast mode) never wait on-chain for; we
    // run them directly below rather than re-reading chain state that may not
    // have advanced yet.
    let mut just_claimed: Vec<u64> = Vec::new();
    for job_id in targets {
        // Already settled (non-Pending or written off) on a previous pass — no
        // need to spend two chain reads rediscovering that every cycle.
        if job_id < chain_tip && settled.contains(&job_id) {
            continue;
        }
        if predicted_writeoff.contains(&job_id) {
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
                    // forever). An id at/above the chain tip is a *predicted*
                    // label: skip it for this process only (#29).
                    if job_id < chain_tip {
                        settled.insert(job_id);
                        settled_dirty = true;
                    } else {
                        predicted_writeoff.insert(job_id);
                    }
                    continue;
                }
            }
        }
        // Item #41: refuse to claim a job we hold no off-chain payload for.
        let gate = if get_job(job_id).is_some() {
            ClaimGate::Claim
        } else {
            let first_sighting = payloadless_warned().lock().unwrap().insert(job_id);
            payload_gate(false, first_sighting)
        };
        match gate {
            ClaimGate::Claim => {}
            ClaimGate::SkipAndWarn => {
                eprintln!(
                    "[provider] skipping LLM job #{job_id}: assigned but no local payload — \
                     cannot run it; leaving it Pending (expires on-chain)"
                );
                continue;
            }
            ClaimGate::SkipSilently => continue,
        }
        println!("[provider] claiming LLM job #{job_id}");
        if fast_claim {
            // accept_job + start_job as one nonce-ordered burst, no block wait.
            match client.claim_job_pipelined(job_id).await {
                Ok(()) => {
                    just_claimed.push(job_id);
                    // One GPU = one job: accept AT MOST ONE per pass; the next job
                    // is only claimed once this one has been run to completion.
                    break;
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
        // One GPU = one job: we have accepted this job — stop claiming others
        // until it has been run to completion.
        break;
    }

    // Persist any newly-settled ids (one write per pass, not per id).
    if settled_dirty {
        if let Err(e) = write_settled_to(&settled_path, &settled, chain_tip) {
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
    if let gpn_agent::provider_policy::Decision::Refuse(r) = policy.check(&job.prompt, &job.model) {
        let cat = r.category.unwrap_or("model_policy");
        eprintln!(
            "[job {id}] \u{26a0} policy refusal (category={cat}) \u{2014} not running: {}",
            r.reason
        );
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
    // Item #22 build-order step 4: report/ban path. Refuse to run a job whose
    // on-chain requestor is on this operator's denylist (shared with the AP).
    if let Some((_state, _price, Some(requestor), _prov)) = client.get_job_state(id).await {
        if let Some(r) = policy.requestor_refused(&requestor) {
            let cat = r.category.unwrap_or("requestor_denylist");
            eprintln!(
                "[job {id}] \u{26a0} requestor denylist refusal \u{2014} not running: {}",
                r.reason
            );
            let _ = LlmStore::new().set_error(id, &format!("gpn_policy_violation: {cat}"));
            let rr = if fast {
                client.fail_job_fast(id, 2).await
            } else {
                client.fail_job(id, 2).await
            };
            match rr {
                Ok(()) => println!("[job {id}] requestor banned \u{2014} escrow refunded"),
                Err(e) => eprintln!("[job {id}] fail_job (denylist) failed: {e}"),
            }
            return Ok(());
        }
    }
    let model = resolve_model(&job.model);
    let preview: String = job.prompt.chars().take(60).collect();
    println!(
        "[job {id}] running inference — model={} prompt=\"{}\"",
        model, preview
    );

    let _ = LlmStore::new().set_status(id, "running");
    // Yield the GPU to this job for its GPU-contending inference (one GPU = one
    // job): the co-located node miner FULLY STOPS while the flag is held and
    // resumes the instant the guard drops. The guard is scoped to `run_inference`
    // ONLY and released BEFORE the on-chain settlement below — settlement needs
    // fresh blocks, and holding the flag across a block-waiting `submit_result`
    // would starve block production on a single-authority chain and deadlock the
    // provider (chain halt). No GPU is used during settlement, so releasing here
    // loses nothing.
    let result = {
        let _yield = gpn_agent::gpu_yield::GpuYieldGuard::acquire();
        run_inference(&model, &job.prompt, job.temperature, job.max_tokens).await
    };
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
mod claim_gate_tests {
    use super::{payload_gate, ClaimGate};

    /// Item #41: a job with no local payload must never be claimed (claiming it
    /// burns accept+start on every poll and wedges it `Running`); the skip is
    /// logged exactly once per id.
    #[test]
    fn payload_gate_claims_only_jobs_we_can_actually_run() {
        assert_eq!(payload_gate(true, false), ClaimGate::Claim);
        assert_eq!(payload_gate(true, true), ClaimGate::Claim);
        assert_eq!(payload_gate(false, true), ClaimGate::SkipAndWarn);
        assert_eq!(payload_gate(false, false), ClaimGate::SkipSilently);
    }
}

#[cfg(test)]
mod heal_step_tests {
    use super::{
        heal_attempt_allowed, heal_backoff, heal_gate, heal_record_nonce, heal_retry_decision,
        heal_step_for_state, HealAttempt, HealStep, HEAL_RETRY_MAX_BACKOFF,
    };
    use std::time::{Duration, Instant};

    /// Backdate a gate entry so the cooldown is already over (tests cannot sleep
    /// 45 s). Mirrors what a real earlier attempt leaves behind.
    fn seed_attempt(
        id: u64,
        step: HealStep,
        age: Duration,
        nonce: Option<u64>,
        ineffective: u32,
    ) {
        let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
        gate.insert(
            (id, step),
            HealAttempt { last: Instant::now() - age, nonce, ineffective },
        );
    }

    #[test]
    fn heal_gate_admits_one_attempt_per_cooldown() {
        // Unique ids: the gate is process-global and tests run in parallel.
        assert!(heal_attempt_allowed(9_100_001, HealStep::Accept, Some(100)));
        // No nonce evidence yet (nothing submitted): still blocked in the cooldown.
        assert!(!heal_attempt_allowed(9_100_001, HealStep::Accept, Some(101)));
        // A different transition is a different key — it may proceed at once.
        assert!(heal_attempt_allowed(9_100_001, HealStep::Start, Some(101)));
    }

    #[test]
    fn heal_gate_records_the_nonce_it_was_submitted_with() {
        assert!(heal_attempt_allowed(9_100_002, HealStep::Complete, Some(42)));
        heal_record_nonce(9_100_002, HealStep::Complete, 77);
        let gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            gate.get(&(9_100_002, HealStep::Complete)).and_then(|a| a.nonce),
            Some(77)
        );
    }

    #[test]
    fn retry_waits_for_our_own_nonce_not_for_the_head() {
        // Cooldown elapsed, but the account nonce still equals the one our tx was
        // signed with ⇒ our tx is queued behind a full block (the dominant case:
        // the head advances every ~11 s, ours does not land). Do NOT burn a new
        // nonce on a duplicate. This is what made the head-based #33 gate a no-op.
        seed_attempt(9_100_003, HealStep::Start, Duration::from_secs(51), Some(100), 0);
        assert!(!heal_attempt_allowed(9_100_003, HealStep::Start, Some(100)));
        // Account nonce passed ours ⇒ the tx was applied (real inclusion evidence).
        seed_attempt(9_100_004, HealStep::Start, Duration::from_secs(51), Some(100), 0);
        assert!(heal_attempt_allowed(9_100_004, HealStep::Start, Some(101)));
        // Unreadable account nonce is conservative: never evidence.
        seed_attempt(9_100_005, HealStep::Start, Duration::from_secs(51), Some(100), 0);
        assert!(!heal_attempt_allowed(9_100_005, HealStep::Start, None));
        // Absolute floor still guarantees liveness for a genuinely dropped tx.
        seed_attempt(9_100_006, HealStep::Start, Duration::from_secs(121), Some(100), 0);
        assert!(heal_attempt_allowed(9_100_006, HealStep::Start, Some(100)));
    }

    #[test]
    fn retry_needs_inclusion_evidence_or_the_absolute_floor() {
        // Inside the cooldown: never, regardless of inclusion.
        assert!(!heal_retry_decision(Duration::from_secs(10), true, 0));
        assert!(!heal_retry_decision(Duration::from_secs(44), false, 0));
        // Cooldown elapsed but our tx provably has not landed: no retry.
        assert!(!heal_retry_decision(Duration::from_secs(51), false, 0));
        // Our nonce advanced: retry.
        assert!(heal_retry_decision(Duration::from_secs(51), true, 0));
        // Absolute floor: assume the tx was dropped and retry for liveness.
        assert!(heal_retry_decision(Duration::from_secs(120), false, 0));
        assert!(heal_retry_decision(Duration::from_secs(600), false, 0));
    }

    /// Item #35: a heal tx that IS included but leaves the state unchanged has
    /// had its dispatch reverted (e.g. `MaxProviderCapacity`). Re-submitting at
    /// the plain 45 s cadence wastes a nonce per attempt, so the gate must back
    /// off progressively instead — while still retrying, never wedging.
    #[test]
    fn reverted_dispatch_backs_off_instead_of_resubmitting() {
        // One inclusion-but-reverted attempt: 51 s is no longer enough.
        seed_attempt(9_100_007, HealStep::Accept, Duration::from_secs(51), Some(100), 1);
        assert!(!heal_attempt_allowed(9_100_007, HealStep::Accept, Some(101)));
        // After the doubled backoff it retries, and the counter escalates.
        seed_attempt(9_100_008, HealStep::Accept, Duration::from_secs(91), Some(100), 1);
        assert!(heal_attempt_allowed(9_100_008, HealStep::Accept, Some(101)));
        {
            let gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(
                gate.get(&(9_100_008, HealStep::Accept)).map(|a| a.ineffective),
                Some(2)
            );
        }
        // Pure schedule: 45 → 90 → 180 → 360 (capped), never unbounded.
        assert_eq!(heal_backoff(0), Duration::from_secs(45));
        assert_eq!(heal_backoff(1), Duration::from_secs(90));
        assert_eq!(heal_backoff(2), Duration::from_secs(180));
        assert_eq!(heal_backoff(3), HEAL_RETRY_MAX_BACKOFF);
        assert_eq!(heal_backoff(9), HEAL_RETRY_MAX_BACKOFF);
        assert!(!heal_retry_decision(Duration::from_secs(89), true, 1));
        assert!(heal_retry_decision(Duration::from_secs(90), true, 1));
    }

    #[test]
    fn heal_walks_one_transition_per_tick() {
        assert_eq!(heal_step_for_state("Pending"), HealStep::Accept);
        assert_eq!(heal_step_for_state("Accepted"), HealStep::Start);
        assert_eq!(heal_step_for_state("Running"), HealStep::SubmitResult);
        assert_eq!(heal_step_for_state("AwaitingVerification"), HealStep::Complete);
    }

    #[test]
    fn heal_ignores_terminal_and_unknown_states() {
        for s in ["Completed", "Failed", "Cancelled", "Bogus", ""] {
            assert_eq!(heal_step_for_state(s), HealStep::Wait, "state {s}");
        }
    }
}

#[cfg(test)]
mod settled_cache_tests {
    //! Item #16 red-team finding #34: the persisted settled-id cache bounds the
    //! claim loop's chain reads when `assignments.json` grows unbounded.
    use super::*;

    fn tmp(name: &str) -> String {
        std::env::temp_dir()
            .join(format!(
                "gpn-settled-test-{}-{}.json",
                std::process::id(),
                name
            ))
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
        write_settled_to(&p, &set, 4096).unwrap();
        let (back, dropped) = read_settled_from_with_tip(&p, 4096);
        assert_eq!(back, set);
        assert!(!dropped);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn settled_reads_tolerate_object_and_missing_file() {
        // Backward compat: a legacy JSON object keyed by id also parses.
        let obj = tmp("obj");
        std::fs::write(&obj, r#"{"7":{"provider":"x"},"9":{}}"#).unwrap();
        let (s, _) = read_settled_from_with_tip(&obj, 100);
        assert!(s.contains(&7) && s.contains(&9) && s.len() == 2);
        // ...and so does a legacy bare array (the pre-#29 on-disk format).
        let arr = tmp("arr");
        std::fs::write(&arr, r#"[7,9,11]"#).unwrap();
        let (s2, _) = read_settled_from_with_tip(&arr, 100);
        assert!(s2.contains(&7) && s2.contains(&11) && s2.len() == 3);
        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&arr);

        // A missing file is simply an empty set (first run), never a panic.
        let (empty, _) = read_settled_from_with_tip("/nonexistent/gpn-settled.json", 100);
        assert!(empty.is_empty());
    }

    #[test]
    fn settled_drops_ids_at_or_above_the_chain_tip() {
        // Item #29 regression: a write-off recorded for a *predicted* label must
        // not blacklist the real job that later takes that id. Ids beyond the
        // chain's NextJobId are not issued yet ⇒ never trusted as settled.
        let p = tmp("poison");
        let mut set = std::collections::HashSet::new();
        set.insert(4772);
        set.insert(4858); // written off for a drift label; real job #4858 later
        set.insert(9999);
        write_settled_to(&p, &set, 10000).unwrap();

        // At load time the chain tip is 4773 ⇒ only the genuinely issued id is kept.
        let (back, dropped) = read_settled_from_with_tip(&p, 4773);
        assert!(back.contains(&4772), "issued + settled id stays skipped");
        assert!(!back.contains(&4858), "future id must not be blacklisted");
        assert!(dropped, "pruning must mark the cache dirty so the file shrinks");
        let _ = std::fs::remove_file(&p);

        // Persisting also refuses to record anything at/above the tip.
        let p2 = tmp("poison-write");
        let mut s2 = std::collections::HashSet::new();
        s2.insert(10);
        s2.insert(500);
        write_settled_to(&p2, &s2, 100).unwrap();
        let raw = std::fs::read_to_string(&p2).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(doc["watermark"], 100);
        assert_eq!(doc["ids"], serde_json::json!([10]));
        let _ = std::fs::remove_file(&p2);
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
