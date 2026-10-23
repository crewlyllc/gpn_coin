//! gpn_diffusion_provider — provider-side image/video generation executor.
//!
//! Polls the chain for `ImageGeneration` / `VideoGeneration` jobs that are
//! `Running` and assigned to this signer, reads the prompt/params from the
//! off-chain store, shells out to the local diffusers helper (`gpn_diffuse.py`,
//! runs on the GPU via HuggingFace diffusers/CUDA), stores the produced
//! artifact + its sha256, and settles the job on-chain (`submit_result` then
//! `complete_job`). The chain only ever sees the artifact hash.
//!
//! Usage:
//!   gpn_diffusion_provider [endpoint] [signer_uri] [poll_ms]
//! Env: ENDPOINT, SIGNER_URI, POLL_MS, GPN_DIFFUSE_SCRIPT (path to gpn_diffuse.py)

use gpn_agent::chain::ChainClient;
use gpn_agent::hub_channel::{Assignment, GenParams, JobResultMsg, ProviderChannel, PubKey};
use gpn_agent::llm::{get_job, LlmStore};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Job ids this process has already declared a **wedge** (helper hung past its
/// budget). Re-running one would just wedge again in a tight loop, burning the
/// GPU with no progress; the chain-side expiry sweep reclaims them instead.
/// In-memory only — a provider restart clears it (a restart is the known cure).
fn wedged_set() -> &'static Mutex<HashSet<u64>> {
    static WEDGED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    WEDGED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_wedged(id: u64) {
    wedged_set().lock().unwrap().insert(id);
}

fn is_wedged(id: u64) -> bool {
    wedged_set().lock().unwrap().contains(&id)
}

/// Item #78: HF repos this process has proved it can never load (401/404/unknown
/// alias). Re-attempting the download every poll is what produced "HF pull
/// sdxl-turbo failed … Repository Not Found (401)" four times in eight seconds
/// while the job sat `Accepted`. In-memory only (a restart re-probes once).
fn unloadable_set() -> &'static Mutex<HashSet<String>> {
    static UNLOADABLE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    UNLOADABLE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_unloadable(repo: &str) {
    unloadable_set().lock().unwrap().insert(repo.to_string());
}

fn is_unloadable(repo: &str) -> bool {
    unloadable_set().lock().unwrap().contains(repo)
}

/// Job ids this process has already failed on-chain (permanent model error), so a
/// still-visible assignment cannot produce a second `fail_job` every poll.
fn failed_set() -> &'static Mutex<HashSet<u64>> {
    static FAILED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    FAILED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_failed_locally(id: u64) {
    failed_set().lock().unwrap().insert(id);
}

fn is_failed_locally(id: u64) -> bool {
    failed_set().lock().unwrap().contains(&id)
}

/// `(attempts, last_attempt)` per job, for TRANSIENT pull failures. The old code
/// retried forever at the poll cadence; a bounded count turns "the network is
/// down" into one loud give-up instead of an infinite log storm.
fn pull_attempts() -> &'static Mutex<std::collections::HashMap<u64, u32>> {
    static PULLS: OnceLock<Mutex<std::collections::HashMap<u64, u32>>> = OnceLock::new();
    PULLS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn record_pull_failure(id: u64) -> u32 {
    let mut m = pull_attempts().lock().unwrap();
    let n = m.entry(id).or_insert(0);
    *n += 1;
    *n
}

fn clear_pull_failures(id: u64) {
    pull_attempts().lock().unwrap().remove(&id);
}

/// Bounded retries for the on-chain `fail_job` itself (a nonce/tx hiccup must not
/// leave a job wedged and must not be re-hammered every poll either).
fn fail_attempts() -> &'static Mutex<std::collections::HashMap<u64, u32>> {
    static FAILS: OnceLock<Mutex<std::collections::HashMap<u64, u32>>> = OnceLock::new();
    FAILS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn record_fail_attempt(id: u64) -> u32 {
    let mut m = fail_attempts().lock().unwrap();
    let n = m.entry(id).or_insert(0);
    *n += 1;
    *n
}

const MAX_FAIL_ATTEMPTS: u32 = 3;

fn fail_give_up(attempts: u32) -> bool {
    attempts >= MAX_FAIL_ATTEMPTS
}

/// How many transient pull failures a job may accumulate before we fail it.
const MAX_PULL_ATTEMPTS: u32 = 3;

fn pull_give_up(attempts: u32) -> bool {
    attempts >= MAX_PULL_ATTEMPTS
}

/// Short model names the network hands out → real HuggingFace repo ids. Kept in
/// sync with `gpn_diffuse.py::model_id` (item #78(a)): the AP's default gen model
/// is `sd-turbo`, clients also send `sdxl-turbo`, and the diffusion helper now
/// resolves both — but `ensure_model` must pull the SAME repo the helper will
/// load, or it downloads one id and the helper asks for another.
const HF_ALIASES: &[(&str, &str)] = &[
    ("sd-turbo", "stabilityai/sd-turbo"),
    ("sd15", "runwayml/stable-diffusion-v1-5"),
    ("sd1.5", "runwayml/stable-diffusion-v1-5"),
    ("sdxl-turbo", "stabilityai/sdxl-turbo"),
    ("sdxl", "stabilityai/stable-diffusion-xl-base-1.0"),
    ("sd21", "stabilityai/stable-diffusion-2-1"),
    ("sd2.1", "stabilityai/stable-diffusion-2-1"),
    ("ltx-video", "Lightricks/LTX-Video"),
    ("ltx", "Lightricks/LTX-Video"),
];

fn resolve_model_alias(model: &str) -> &str {
    HF_ALIASES
        .iter()
        .find(|(alias, _)| *alias == model)
        .map(|(_, repo)| *repo)
        .unwrap_or(model)
}

/// Is this failure PERMANENT (retrying can never help) or TRANSIENT (network,
/// disk, rate limit)? Only permanent failures may fail the job: a transient one
/// must not throw away a servable job.
fn is_permanent_load_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    [
        "repository not found",
        "repository not found for url",
        "401 client error",
        "403 client error",
        "404 client error",
        "gatedrepoerror",
        "invalid username or password",
        "not a valid model identifier",
        "is not a valid model",
        "cannot load model",
        "no model card",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// The classified failure of `ensure_model`.
struct PullError {
    message: String,
    permanent: bool,
}

/// `--flag value` lookup (the binary does not use a CLI parser).
fn opt_str(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .filter(|s| !s.is_empty())
}

fn diffuse_script() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_DIFFUSE_SCRIPT") {
        return PathBuf::from(p);
    }
    // Default: alongside this crate (repo gpn-agent dir).
    PathBuf::from("gpn_diffuse.py")
}

// ── channel-settled generation (off-chain escrow) ───────────────────────────
//
// The LLM provider serves `POST /channel/job` on `status_port + 1` (8094). The
// diffusion provider is a **separate process** on the same box and the same
// signer account, so it serves the same protocol on its own port (8095 by
// default) and the AP addresses it explicitly for render jobs. Payment is the
// same mutually-signed channel update: accept → run → `sign_result` /
// `sign_refund`. No chain call, no block wait, no escrow per render.

/// One artifact this provider produced for a channel job.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredArtifact {
    channel_id: u64,
    path: String,
    media_type: String,
    sha256: String,
    width: u32,
    height: u32,
    bytes: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct ChannelStore {
    #[serde(default)]
    channels: BTreeMap<u64, ProviderChannel>,
    #[serde(default)]
    artifacts: BTreeMap<u64, StoredArtifact>,
}

#[derive(Clone)]
struct ChannelCtx {
    me: PubKey,
    me_hex: String,
    signer: Arc<gpn_agent::chain::signer::Sr25519Signer>,
    state_path: PathBuf,
    vram_mb: u64,
}

fn load_channels(path: &PathBuf) -> ChannelStore {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_channels(path: &PathBuf, store: &ChannelStore) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_vec_pretty(store) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Render one image with `gpn_diffuse.py` under the GPU yield guard.
fn render_image(
    job_id: u64,
    model: &str,
    prompt: &str,
    gen: &GenParams,
    out_path: &str,
) -> Result<serde_json::Value, String> {
    let req_json = json!({
        "kind": "image",
        "model": model,
        "prompt": prompt,
        "width": gen.width,
        "height": gen.height,
        "steps": gen.steps,
        "seed": gen.seed,
        "frames": 1,
        "fps": 1,
        "output": out_path,
    });
    let req_file = gpn_agent::utils::temp_path(&format!("gpn_diffuse_chan_{job_id}.json"));
    std::fs::write(
        &req_file,
        serde_json::to_vec(&req_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("cannot write request: {e}"))?;
    let script = diffuse_script();
    let timeout = diffuse_timeout("image");
    let req_file2 = req_file.clone();
    let res = match gpn_agent::gpu_yield::GpuYieldGuard::acquire() {
        Err(e) => Err(e.to_string()),
        Ok(_yield) => {
            // One GPU = one job: the co-located miner fully stops while the
            // flag is held, and resumes the instant it drops. `acquire` is a
            // real cross-process lock, so a chat job in
            // gpn_llm_provider makes this render WAIT instead of fighting it
            // for VRAM.
            let mut cmd = std::process::Command::new("python3");
            cmd.arg(&script).arg(&req_file2);
            run_with_watchdog(cmd, timeout, &format!("channel job {job_id}")).and_then(
                |(stdout, _)| {
                    serde_json::from_str(&stdout)
                        .map_err(|e| format!("bad helper output: {e} ({stdout})"))
                },
            )
        }
    };
    let _ = std::fs::remove_file(&req_file);
    res
}

fn serve_channel_job(ctx: &ChannelCtx, body: &str) -> (u16, serde_json::Value) {
    let a: Assignment = match serde_json::from_str(body) {
        Ok(a) => a,
        Err(e) => {
            return (
                400,
                json!({"ok": false, "error": format!("bad assignment: {e}")}),
            )
        }
    };
    if a.provider != ctx.me {
        return (
            409,
            json!({"ok": false, "error": "assignment names a different provider"}),
        );
    }
    let Some(gen) = a.gen.clone() else {
        return (
            409,
            json!({"ok": false, "error": "this provider only serves generation assignments"}),
        );
    };
    let mut store = load_channels(&ctx.state_path);
    if let std::collections::btree_map::Entry::Vacant(e) = store.channels.entry(a.channel_id) {
        // First sight of this channel: adopt the state the AP signed for this
        // job. Two providers (LLM on 8094, diffusion on 8095) share ONE channel
        // per requestor, so a fresh mirror must NOT assume the channel is in its
        // deposit-split starting state — after any LLM job it is not, and
        // `accept` would reject with `StaleStateBefore` (observed: HTTP 409 on
        // the first channel-settled image). `state_before` is inside the AP's
        // signature, so it is authoritative and tamper-evident.
        let mut ch = ProviderChannel::new(a.channel_id, a.deposit, PubKey(a.requestor.0));
        ch.sync_to_assignment(&a);
        println!(
            "[channel] adopting channel {} at nonce {} (requestor balance {})",
            a.channel_id, ch.state.nonce, ch.state.balance_requestor
        );
        e.insert(ch);
    }
    {
        let ch = store.channels.get_mut(&a.channel_id).unwrap();
        // Two provider processes share this account's channel: follow the AP's
        // signed pre-state whenever our mirror is behind (see `sync_to_assignment`).
        ch.sync_to_assignment(&a);
        if let Err(e) = ch.accept(&a) {
            eprintln!("[channel] job {} rejected: {e}", a.job_id);
            return (
                409,
                json!({"ok": false, "error": format!("assignment rejected: {e}")}),
            );
        }
    }
    // Persist the pre-paid state BEFORE the render: a crash mid-run must not
    // lose the AP's signed payment.
    save_channels(&ctx.state_path, &store);

    let decision = gpn_agent::image_channel::serve_decision(
        !gpn_agent::idle::claims_allowed(),
        &a.model,
        ctx.vram_mb,
        gen.width,
        gen.height,
    );
    let outcome = match decision {
        gpn_agent::image_channel::ServeDecision::Refund(reason) => Err(reason),
        gpn_agent::image_channel::ServeDecision::Run => {
            let out_dir = std::env::var("GPN_ARTIFACTS").unwrap_or_else(|_| {
                gpn_agent::utils::temp_dir()
                    .join("gpn_artifacts")
                    .to_string_lossy()
                    .to_string()
            });
            let _ = std::fs::create_dir_all(&out_dir);
            let out_path = format!("{out_dir}/chan_{}_.png", a.job_id);
            let started = Instant::now();
            match render_image(a.job_id, &a.model, &a.prompt, &gen, &out_path) {
                Ok(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                    let path = v["artifact"].as_str().unwrap_or(&out_path).to_string();
                    let media_type = v["media_type"].as_str().unwrap_or("image/png").to_string();
                    let sha256 = v["sha256"].as_str().unwrap_or("").to_string();
                    let bytes = std::fs::read(&path).map(|b| b.len() as u64).unwrap_or(0);
                    if bytes == 0 {
                        return refund(ctx, a.channel_id, a.job_id, "render produced no bytes");
                    }
                    store.artifacts.insert(
                        a.job_id,
                        StoredArtifact {
                            channel_id: a.channel_id,
                            path: path.clone(),
                            media_type: media_type.clone(),
                            sha256: sha256.clone(),
                            width: gen.width,
                            height: gen.height,
                            bytes,
                        },
                    );
                    Ok(json!({
                        "artifacts": [{
                            "index": 0,
                            "sha256": sha256,
                            "media_type": media_type,
                            "width": gen.width,
                            "height": gen.height,
                            "bytes": bytes,
                        }],
                        "render_seconds": started.elapsed().as_secs_f64().round(),
                    }))
                }
                Ok(v) => Err(v["error"]
                    .as_str()
                    .unwrap_or("generation failed")
                    .to_string()),
                Err(e) => {
                    if e.contains("WEDGE") {
                        mark_wedged(a.job_id);
                    }
                    Err(e)
                }
            }
        }
    };

    match outcome {
        Ok(output) => {
            let text = serde_json::to_string(&output).unwrap_or_default();
            let msg = store
                .channels
                .get_mut(&a.channel_id)
                .unwrap()
                .sign_result(&ctx.signer, a.job_id, &text, 0, 0)
                .expect("sign_result after accept must succeed");
            save_channels(&ctx.state_path, &store);
            println!(
                "[channel] job {} on channel {} served ({} bytes)",
                a.job_id, a.channel_id, output["artifacts"][0]["bytes"]
            );
            (200, serde_json::to_value(&msg).unwrap_or_default())
        }
        Err(reason) => {
            eprintln!("[channel] job {} refunded: {reason}", a.job_id);
            refund(ctx, a.channel_id, a.job_id, &reason)
        }
    }
}

/// Sign a refund (reverts the pre-authorised payment; the requestor pays
/// nothing) and return the signed message to the AP.
fn refund(
    ctx: &ChannelCtx,
    channel_id: u64,
    job_id: u64,
    reason: &str,
) -> (u16, serde_json::Value) {
    let mut store = load_channels(&ctx.state_path);
    let Some(ch) = store.channels.get_mut(&channel_id) else {
        return (
            409,
            json!({"ok": false, "error": "unknown channel for refund"}),
        );
    };
    let msg: JobResultMsg = match ch.sign_refund(&ctx.signer, job_id, reason) {
        Ok(m) => m,
        Err(e) => {
            return (
                409,
                json!({"ok": false, "error": format!("cannot refund job {job_id}: {e}")}),
            )
        }
    };
    save_channels(&ctx.state_path, &store);
    (200, serde_json::to_value(&msg).unwrap_or_default())
}

fn serve_artifact(ctx: &ChannelCtx, job_id: u64, token: &str) -> Option<(Vec<u8>, String)> {
    let store = load_channels(&ctx.state_path);
    let a = store.artifacts.get(&job_id)?;
    let want = gpn_agent::image_channel::artifact_token(a.channel_id, job_id);
    if !gpn_agent::image_channel::token_matches(&want, token) {
        return None;
    }
    let bytes = std::fs::read(&a.path).ok()?;
    Some((bytes, a.media_type.clone()))
}

/// Tiny HTTP server for the provider half of the channel protocol.
///
/// One thread per request: a render (and, since the cross-process GPU lock, a
/// *wait* for another provider using the GPU) must never make
/// `GET /channel/status` — the AP's liveness probe — or an artifact download
/// time out. Before this, a 30-60 s render made the box look offline, so the AP
/// refused new image jobs with "no diffusion provider serves the channel render
/// endpoint" instead of letting them queue.
fn spawn_channel_server(listen: &str, ctx: ChannelCtx) {
    let listen = listen.to_string();
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(&listen) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[channel] bind {listen} failed: {e}");
                return;
            }
        };
        println!(
            "[channel] diffusion channel endpoint on http://{listen} (POST /channel/job, GET /channel/artifact/{{id}})"
        );
        for req in server.incoming_requests() {
            let ctx = ctx.clone();
            std::thread::spawn(move || handle_channel_request(req, &ctx));
        }
    });
}

fn handle_channel_request(mut req: tiny_http::Request, ctx: &ChannelCtx) {
    let json_hdr =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    // Loopback only: the AP submits signed assignments from this box.
    // A public bind would need the on-chain channel check the LLM
    // provider does before accepting a first-sight channel id.
    let loopback = req
        .remote_addr()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(false);
    if !loopback {
        let _ = req.respond(
            tiny_http::Response::from_string(
                json!({"ok": false, "error": "loopback only"}).to_string(),
            )
            .with_header(json_hdr)
            .with_status_code(403),
        );
        return;
    }
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    if req.method() == &tiny_http::Method::Get && path == "/channel/status" {
        let store = load_channels(&ctx.state_path);
        let ids: Vec<u64> = store.channels.keys().copied().collect();
        let body = json!({
            "ok": true,
            "channel": true,
            "kinds": ["image"],
            "provider": ctx.me_hex,
            "channels": ids,
        });
        let _ =
            req.respond(tiny_http::Response::from_string(body.to_string()).with_header(json_hdr));
        return;
    }
    if req.method() == &tiny_http::Method::Post && path == "/channel/job" {
        let mut body = String::new();
        let _ = req.as_reader().read_to_string(&mut body);
        let (code, resp) = serve_channel_job(ctx, &body);
        let _ = req.respond(
            tiny_http::Response::from_string(resp.to_string())
                .with_header(json_hdr)
                .with_status_code(code),
        );
        return;
    }
    if req.method() == &tiny_http::Method::Get && path.starts_with("/channel/artifact/") {
        let id = path.trim_start_matches("/channel/artifact/");
        let job_id: u64 = id.parse().unwrap_or(0);
        let token = url
            .split_once('?')
            .and_then(|(_, q)| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix("t=").map(|v| v.to_string()))
            })
            .unwrap_or_default();
        match serve_artifact(ctx, job_id, &token) {
            Some((bytes, mt)) => {
                let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], mt.as_bytes())
                    .unwrap_or(json_hdr);
                let _ = req.respond(tiny_http::Response::from_data(bytes).with_header(ct));
            }
            None => {
                let _ = req.respond(
                    tiny_http::Response::from_string(
                        json!({"ok": false, "error": "no such artifact"}).to_string(),
                    )
                    .with_header(json_hdr)
                    .with_status_code(404),
                );
            }
        }
        return;
    }
    let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
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
        .unwrap_or(4000);

    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    let (me, addr) = {
        use subxt::tx::Signer as _;
        let s = gpn_agent::chain::signer::Sr25519Signer::from_uri(&signer_uri)?;
        let aid = s.account_id();
        let b: &[u8] = aid.as_ref();
        (format!("0x{}", hex::encode(b)), aid.to_string())
    };

    println!(
        "GPN diffusion provider running — endpoint={} signer={} poll={}ms script={}",
        endpoint,
        me,
        poll_ms,
        diffuse_script().display()
    );

    // Channel-settled renders (the public `/v1/images/generations` fast path).
    // Off unless the operator turns it on: the AP must know the same port.
    let channel_listen = opt_str(&args, "--channel-listen")
        .or_else(|| std::env::var("GPN_CHANNEL_LISTEN").ok())
        .unwrap_or_else(|| "127.0.0.1:8095".to_string());
    if channel_listen != "off" {
        let signer = Arc::new(gpn_agent::chain::signer::Sr25519Signer::from_uri(
            &signer_uri,
        )?);
        let state_path = std::env::var("GPN_DIFFUSION_CHANNEL_STORE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home)
                    .join(".gpn")
                    .join("diffusion_channels.json")
            });
        let vram_mb = std::env::var("GPN_VRAM_MB")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or_else(gpn_agent::gpu::detect_vram_mb);
        spawn_channel_server(
            &channel_listen,
            ChannelCtx {
                me: PubKey(signer.public_key()),
                me_hex: me.clone(),
                signer,
                state_path,
                vram_mb,
            },
        );
    }

    // #46 Phase 1 — pull assignments from the AP (works behind NAT; see
    // gpn_agent::assign_pull). Push remains active unless GPN_ASSIGN_SOURCE=pull.
    if let Some(cfg) = gpn_agent::assign_pull::PullConfig::from_env(&me) {
        gpn_agent::assign_pull::spawn(cfg);
    }

    // Item #59: the claimable-job scan and the reconcile sweep are chain-heavy
    // (one storage RPC per job id) and used to run on EVERY poll. They are now
    // throttled independently of the cheap local passes.
    let mut last_claim_scan: Option<Instant> = None;
    let mut last_reconcile: Option<Instant> = None;
    let mut job_kinds_cache: JobKinds = None;

    loop {
        // IDLE gate: while the operator idles the node we take no new job.
        // The pass below claims and runs in one go, so a job already started
        // finishes normally; what stops is the *next* claim.
        if !gpn_agent::idle::claims_allowed() {
            tokio::time::sleep(Duration::from_millis(poll_ms.max(1000))).await;
            continue;
        }
        if let Err(e) = process_once(
            &client,
            &me,
            &addr,
            &mut last_claim_scan,
            &mut last_reconcile,
            &mut job_kinds_cache,
        )
        .await
        {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

fn is_gen(job: &gpn_agent::llm::LlmJob) -> bool {
    job.kind == "image" || job.kind == "video"
}

/// A state error meaning "the job already advanced / isn't in the state we
/// expected" — benign during reconcile (the owning provider or a prior cycle
/// got there first), so it must not be logged as an ERROR. This is the source
/// of the spurious `JobNotRunning [pallet=11 e=5]` log spam.
fn is_benign_state_err(s: &str) -> bool {
    s.contains("JobNotRunning") || s.contains("JobNotPending") || s.contains("JobNotAccepted")
}

/// How many of the most recent job ids the reconcile pass scans per cycle
/// (was `0..next` = every historical job, one RPC each, on EVERY poll).
const RECONCILE_WINDOW: u64 = 128;

// ---- item #59: bound the chain-heavy passes ----------------------------
//
// `list_claimable_jobs` costs ONE `state_getStorage` RPC per job id, and this
// provider called it unconditionally on every 2 s poll. With `NextJobId` at
// 6 371 that is ~6 371 sequential RPCs per cycle — measured at ~95 % of a core
// for 10 h straight — and the result was thrown away every time, because every
// job on this network is an `llm` job and `is_gen()` rejects all of them. The
// same process also re-read 128 ids for the reconcile sweep each cycle.
//
// Both passes are now (a) rate-limited and (b) bounded to recent ids. Neither
// changes what this provider can claim: gen jobs only ever live in the newest
// ids, and anything older than the window would have expired on chain anyway.
const DEFAULT_CLAIM_SCAN_MS: u64 = 30_000;
const DEFAULT_RECONCILE_MS: u64 = 30_000;
const DEFAULT_CLAIM_SCAN_WINDOW: u64 = 512;
/// Bound for the "is any of my generation jobs Running?" sweep (local store
/// reads, one per id — see the comment at its call site).
const RUNNING_SCAN_WINDOW: u64 = 512;
/// How long the set of image/video job ids is trusted before the ledger is
/// re-read. A generation job is created once and runs for minutes, so a few
/// seconds of staleness costs nothing; without a TTL every pass re-parsed the
/// whole shared ledger (see `gen_id_set`).
const DEFAULT_GEN_ID_CACHE_MS: u64 = 5_000;

/// Which ids in the shared ledger are image/video jobs (`gen`), which are known
/// jobs of another kind (`other`), and when that split was read.
type JobKinds = Option<(Instant, HashSet<u64>, HashSet<u64>)>;

/// Cached "is this id a generation job?" view of the ledger (item #59).
///
/// One ledger pass per refresh answers both questions the pass needs: which ids
/// we may claim/run, and which ids are known *not* to be ours (so the reconcile
/// kind-gate does not have to read the ledger per id).
fn job_kinds(cache: &mut JobKinds, ttl_ms: u64) -> (HashSet<u64>, HashSet<u64>) {
    if !due(cache.as_ref().map(|(t, _, _)| *t), Instant::now(), ttl_ms) {
        if let Some((_, gen, other)) = cache.as_ref() {
            return (gen.clone(), other.clone());
        }
    }
    let (gen, other) = LlmStore::new().ids_partitioned(is_gen);
    let gen: HashSet<u64> = gen.into_iter().collect();
    let other: HashSet<u64> = other.into_iter().collect();
    *cache = Some((Instant::now(), gen.clone(), other.clone()));
    (gen, other)
}

/// Read a millisecond interval from `name`, ignoring garbage/zero.
fn env_millis(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Half-open id range for a bounded claim scan: the newest `window` ids below
/// `next` (a tiny chain still scans `0..next`).
fn claim_scan_range(next: u64, window: u64) -> (u64, u64) {
    (next.saturating_sub(window.max(1)), next)
}

/// True when `interval_ms` has elapsed since `last`; `None` (never run) is due.
/// Saturating, so a backwards clock jump cannot produce a huge elapsed value and
/// spin the pass; a stamp in the future simply waits (it never panics).
fn due(last: Option<Instant>, now: Instant, interval_ms: u64) -> bool {
    match last {
        None => true,
        Some(t) => now.saturating_duration_since(t) >= Duration::from_millis(interval_ms),
    }
}

/// Hard time budget for one generation. Beyond this the helper is considered
/// **wedged** (GPU contention, a stalled HF download, a CUDA hang) and is
/// killed, so a single bad job can never silence the provider forever — the
/// exact "silent wedge" this instrumentation detects. Override with
/// `GPN_DIFFUSE_TIMEOUT_SECS`; default 600 s for images, 1800 s for video.
fn diffuse_timeout(kind: &str) -> Duration {
    if let Ok(s) = std::env::var("GPN_DIFFUSE_TIMEOUT_SECS") {
        if let Ok(n) = s.parse::<u64>() {
            if n > 0 {
                return Duration::from_secs(n);
            }
        }
    }
    if kind == "video" {
        Duration::from_secs(1800)
    } else {
        Duration::from_secs(600)
    }
}

/// Progress-heartbeat interval while the generation helper runs. Emitting this
/// keeps the provider log from going *silent* during a long render, so a wedge
/// is visible as "still running at Ns" rather than dead air.
const HELPER_HEARTBEAT: Duration = Duration::from_secs(30);

/// Run a command under a hard watchdog, capturing stdout/stderr to temp files.
///
/// Returns `(stdout, stderr)` on success. On non-zero exit returns the helper's
/// stderr in `Err`. On timeout the child is **killed** and `Err("WEDGE: …")` is
/// returned so the caller logs the wedge and keeps serving instead of stalling.
/// The helper's structured failure text, if it printed one. `gpn_diffuse.py`
/// reports failures as `{"ok": false, "error": "..."}` on **stdout** and then
/// exits 1, so a stderr-only error string throws away the real cause (and with it
/// the ability to tell a permanent 401 from a transient network error).
fn json_reason(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        return None;
    }
    v.get("error")
        .and_then(|e| e.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn run_with_watchdog(
    mut cmd: std::process::Command,
    timeout: Duration,
    label: &str,
) -> Result<(String, String), String> {
    let tag = format!("{}_{}", label.replace(' ', "_"), std::process::id());
    let out_path = gpn_agent::utils::temp_path(&format!("gpn_wd_{tag}.out"));
    let err_path = gpn_agent::utils::temp_path(&format!("gpn_wd_{tag}.err"));
    let out_f = std::fs::File::create(&out_path).map_err(|e| format!("tmp stdout: {e}"))?;
    let err_f = std::fs::File::create(&err_path).map_err(|e| format!("tmp stderr: {e}"))?;
    cmd.stdout(out_f).stderr(err_f);
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();
    let start = std::time::Instant::now();
    let mut last_hb = start;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let so = std::fs::read_to_string(&out_path).unwrap_or_default();
                let se = std::fs::read_to_string(&err_path).unwrap_or_default();
                break if status.success() {
                    Ok((so, se))
                } else {
                    let detail = json_reason(&so).unwrap_or_else(|| se.trim().to_string());
                    Err(format!("helper exited {:?}: {}", status.code(), detail))
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!(
                        "WEDGE: {label} helper (pid {pid}) exceeded {}s — killed (GPU/download hang?)",
                        timeout.as_secs()
                    ));
                }
                if last_hb.elapsed() >= HELPER_HEARTBEAT {
                    eprintln!(
                        "[watchdog] {label} helper pid {pid} still running — elapsed={}s",
                        start.elapsed().as_secs()
                    );
                    last_hb = std::time::Instant::now();
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                let _ = child.kill();
                break Err(format!("wait failed: {e}"));
            }
        }
    };
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    result
}

fn assignment_store_path() -> String {
    std::env::var("GPN_ASSIGN_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/assignments.json")
    })
}

/// Job ids the scheduler assigned to us (hex or ss58), + the requested model +
/// whether we must pull it on demand. Handles string + `{provider,model,pull}`.
fn read_assigned(me: &str, addr: &str) -> Vec<(u64, String, bool)> {
    let Ok(s) = std::fs::read_to_string(assignment_store_path()) else {
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

/// Ensure a diffusion model is present; download from HuggingFace if missing.
/// `model` is an HF repo id (e.g. `stabilityai/stable-diffusion-2-1`) or a local
/// path. Empty/`auto` = use whatever the helper defaults to.
fn ensure_model(model: &str) -> Result<String, PullError> {
    if model.is_empty() || model == "auto" {
        return Ok("no specific model requested".into());
    }
    if model.starts_with('/') || model.starts_with('.') {
        return Ok("local model path".into());
    }
    // Map short aliases (as advertised by gpn_diffuse.py) to real HF repo ids,
    // otherwise ensure_model would try to pull a non-existent repo (e.g. "sd-turbo")
    // and 401/404 every poll cycle.
    let model = resolve_model_alias(model);
    // HF cache marker: ~/.cache/huggingface/hub/models--<org>--<name>
    let repo_dir = format!("models--{}", model.replace('/', "--"));
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let hub = std::env::var("HF_HOME")
        .map(|h| format!("{h}/hub"))
        .unwrap_or_else(|_| format!("{home}/.cache/huggingface/hub"));
    if std::path::Path::new(&format!("{hub}/{repo_dir}")).exists() {
        return Ok(format!("model {model} already cached"));
    }
    println!("[provider] diffusion model {model} missing — pulling from HuggingFace…");
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "from huggingface_hub import snapshot_download; snapshot_download('{model}')"
        ))
        .output()
        .map_err(|e| PullError {
            message: format!("spawn python3: {e}"),
            permanent: false,
        })?;
    if out.status.success() {
        Ok(format!("pulled diffusion model {model} from HuggingFace"))
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(PullError {
            permanent: is_permanent_load_error(&stderr),
            message: format!("HF pull {model} failed: {stderr}"),
        })
    }
}

/// Fail a job whose model this provider can NEVER load: one loud line, off-chain
/// error recorded, `fail_job(5)` so the requestor's escrow refunds and the
/// provider's capacity slot frees immediately instead of at the on-chain
/// deadline (item #78 — the #76 class in the provider #76 did not touch).
async fn fail_unloadable_job(client: &ChainClient, me: &str, id: u64, model: &str, why: &str) {
    if is_failed_locally(id) {
        return;
    }
    let msg = if why.starts_with("cannot load diffusion model") {
        why.to_string()
    } else {
        format!("cannot load diffusion model {model}: {why}")
    };
    let _ = LlmStore::new().set_error(id, &msg);
    // Read the CHAIN before acting: a stale store entry can outlive its job, and
    // announcing "failing the job" for one that is already `Failed` is noise.
    // A `Pending` job also has no assigned provider yet, so `fail_job` answers
    // `NotJobProvider` (observed live on #6519) — we were leased this job, so
    // accept+start it first; those are one-shot and make the refund + slot
    // release happen now instead of at the deadline.
    let state = match client.get_job_state(id).await {
        Some((state, _price, _req, provider)) => {
            let mine = provider.as_deref() == Some(me);
            if matches!(state.as_str(), "Completed" | "Failed" | "Cancelled") {
                // Nothing left to fail — just stop looking at it.
                mark_failed_locally(id);
                return;
            }
            if state != "Pending" && !mine {
                // Accepted/Running but owned by someone else — never steal it.
                return;
            }
            state
        }
        // Chain unreadable: try again next pass (bounded by the fail budget).
        None => return,
    };
    eprintln!("[job {id}] \u{26a0} {msg} — failing the job (escrow refunds, slot frees)");
    if state == "Pending" {
        if let Err(e) = client.accept_job(id).await {
            if !e.to_string().contains("JobNotPending") {
                eprintln!("[job {id}] accept (unloadable model) failed: {e}");
            }
        }
        if let Err(e) = client.start_job(id).await {
            if !e.to_string().contains("JobNotPending") && !e.to_string().contains("JobNotStarted")
            {
                eprintln!("[job {id}] start (unloadable model) failed: {e}");
            }
        }
    }
    match client.fail_job(id, 5).await {
        Ok(()) => {
            mark_failed_locally(id);
            println!("[job {id}] failed on-chain (unloadable model) — escrow refunded");
        }
        Err(e) => {
            let n = record_fail_attempt(id);
            if fail_give_up(n) {
                mark_failed_locally(id);
            }
            eprintln!(
                "[job {id}] fail_job (unloadable model) failed ({n}/{MAX_FAIL_ATTEMPTS}): {e}"
            );
        }
    }
}

async fn process_once(
    client: &ChainClient,
    me: &str,
    addr: &str,
    last_claim_scan: &mut Option<Instant>,
    last_reconcile: &mut Option<Instant>,
    job_kinds_cache: &mut JobKinds,
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
    let next = client.get_next_job_id().await;
    // Item #59: ONE ledger pass per refresh answers both "which ids may we
    // claim/run" and "which ids are known not to be ours", so neither the
    // reconcile kind-gate nor the claim filters read the ledger per id.
    let (gen_ids, other_ids) = job_kinds(
        job_kinds_cache,
        env_millis("GPN_GEN_ID_CACHE_MS", DEFAULT_GEN_ID_CACHE_MS),
    );
    let reconcile_ms = env_millis("GPN_RECONCILE_MS", DEFAULT_RECONCILE_MS);
    if due(*last_reconcile, Instant::now(), reconcile_ms) {
        *last_reconcile = Some(Instant::now());
        for id in next.saturating_sub(RECONCILE_WINDOW)..next {
            if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
                if provider.as_deref() != Some(me) {
                    continue;
                }
                if state == "AwaitingVerification" {
                    // Kind-gate: skip jobs owned by the other provider (shared
                    // signer account means `provider == me` is true for both kinds).
                    // Unknown kinds are still reconciled so orphan zombies get freed.
                    if other_ids.contains(&id) {
                        continue;
                    }
                    match client.complete_job(id).await {
                        Ok(()) => {
                            println!("[provider] reconciled #{id} AwaitingVerification → Completed")
                        }
                        Err(e) if is_benign_state_err(&e.to_string()) => {
                            // Already finalized elsewhere — quiet, not an error.
                        }
                        Err(e) => eprintln!("[provider] reconcile #{id} (complete) failed: {e}"),
                    }
                } else if state == "Accepted" && gen_ids.contains(&id) {
                    // Capacity reclaim: an `Accepted` gen job we own that was never
                    // started is a zombie holding a `MaxJobsPerProvider` slot. Our
                    // claim loop only starts `Pending` jobs, so it can never advance
                    // on its own — drive it Accepted → Running → Completed to free
                    // the slot (escrow is refunded by `complete_job`).
                    if let Err(e) = client.start_job(id).await {
                        eprintln!("[provider] reconcile #{id} (start) failed: {e}");
                        continue;
                    }
                    match client.complete_job(id).await {
                        Ok(()) => println!(
                            "[provider] reconciled #{id} Accepted → Completed (capacity freed)"
                        ),
                        Err(e) => eprintln!("[provider] reconcile #{id} (complete) failed: {e}"),
                    }
                }
            }
        }
    }

    // 0. Auto-claim: if the scheduler assigned us specific jobs, claim ONLY those
    //    (pulling any missing model first); otherwise claim any Pending gen job.
    let assigned = read_assigned(me, addr);
    // Item #59: ONE ledger pass per refresh instead of a `get_job` per id.
    // `get_job` is cheap only while the mtime cache holds; the LLM provider
    // rewrites this shared ~5 MB ledger several times a second, so a per-id
    // sweep re-parsed the whole file repeatedly (~19 MB/s of reads, ~55 % of a
    // core) just to answer "is this id an image/video job?".
    let assigned_ids: Vec<u64> = if assigned.is_empty() {
        vec![]
    } else {
        for (id, model, pull) in &assigned {
            if !gen_ids.contains(id) {
                continue;
            }
            // Already failed on-chain for an unloadable model — never re-fail.
            if is_failed_locally(*id) {
                continue;
            }
            let Some(job) = get_job(*id) else { continue };
            // `done` = settled; `error` = we already concluded about this job
            // (a permanent model failure, a wedge, …). Preparing a model for a job
            // we have finished with is what kept the #6518 storm alive for hours
            // after that job was on-chain `Failed`. An errored job that the chain
            // still holds non-terminal is re-driven to `Failed` once (bounded), so
            // its escrow is refunded now instead of at the deadline.
            if job.status == "done" {
                continue;
            }
            if job.status == "error" {
                let repo = resolve_model_alias(model).to_string();
                let why = job
                    .error
                    .clone()
                    .unwrap_or_else(|| "earlier failure".into());
                fail_unloadable_job(client, me, *id, &repo, &why).await;
                continue;
            }
            if model.is_empty() || model == "auto" {
                continue;
            }
            let _ = pull;
            let repo = resolve_model_alias(model).to_string();
            // A repo this process already proved permanent-unloadable must not be
            // re-pulled on every poll (item #78): fail the job once instead.
            if is_unloadable(&repo) {
                fail_unloadable_job(client, me, *id, &repo, "known unloadable on this host").await;
                continue;
            }
            match ensure_model(model) {
                // Only report a pull that actually happened (avoid per-cycle noise).
                Ok(n) if n.starts_with("pulled") => {
                    clear_pull_failures(*id);
                    println!("[provider] job #{id}: {n}");
                }
                Ok(_) => clear_pull_failures(*id),
                Err(e) if e.permanent => {
                    // 401/404/unknown alias: retrying can never help. Remember it
                    // for the repo (so other jobs skip the pull) and fail THIS job
                    // once, so escrow refunds and the capacity slot frees now.
                    mark_unloadable(&repo);
                    fail_unloadable_job(client, me, *id, &repo, &e.message).await;
                }
                Err(e) => {
                    let attempts = record_pull_failure(*id);
                    eprintln!(
                        "[provider] job #{id}: {} (transient, attempt {attempts}/{MAX_PULL_ATTEMPTS})",
                        e.message
                    );
                    if pull_give_up(attempts) {
                        fail_unloadable_job(
                            client,
                            me,
                            *id,
                            &repo,
                            &format!("pull failed {attempts} times: {}", e.message),
                        )
                        .await;
                    }
                }
            }
        }
        assigned
            .iter()
            .filter(|(id, _, _)| gen_ids.contains(id))
            .map(|(id, _, _)| *id)
            .collect()
    };

    let attested_gflops = client
        .get_attestation_record(addr)
        .await
        .map(|(_, f, _)| f)
        .unwrap_or(0);
    // Item #59: bounded + throttled scan (was: full history, every poll).
    let claim_scan_ms = env_millis("GPN_CLAIM_SCAN_MS", DEFAULT_CLAIM_SCAN_MS);
    let claim_window = env_millis("GPN_CLAIM_SCAN_WINDOW", DEFAULT_CLAIM_SCAN_WINDOW);
    let claimable =
        if !assigned_ids.is_empty() || due(*last_claim_scan, Instant::now(), claim_scan_ms) {
            *last_claim_scan = Some(Instant::now());
            let (from, to) = claim_scan_range(next, claim_window);
            client
                .list_claimable_jobs_window(from, to, attested_gflops)
                .await
        } else {
            Vec::new()
        };
    for (job_id, min_gflops) in claimable {
        if !assigned_ids.is_empty() && !assigned_ids.contains(&job_id) {
            continue;
        }
        if !gen_ids.contains(&job_id) {
            continue;
        }
        println!("[provider] claiming generation job #{job_id} (min_gflops={min_gflops})");
        if let Err(e) = client.accept_job(job_id).await {
            // JobNotPending means we (or a prior cycle) already accepted it; don't give
            // up — fall through and still drive it to Running, otherwise it wedges in
            // Accepted forever (accept→continue skipped start_job).
            let already = e.to_string().contains("JobNotPending");
            if !already {
                eprintln!("[provider] accept #{job_id} failed: {e}");
                continue;
            }
        }
        if let Err(e) = client.start_job(job_id).await {
            eprintln!("[provider] start #{job_id} failed: {e}");
        }
        // One GPU = one job: we have accepted this job — stop claiming others
        // until it has been run to completion.
        break;
    }

    // 1. Run any Running generation job assigned to this signer.
    //
    // Item #59: bounded to the newest ids, same reasoning as the claim scan.
    // `get_job` is local, but it is called once per id and the store is a ~5 MB
    // ledger shared with the LLM provider (any write to it invalidates the
    // mtime cache, so a full 0..next sweep pays for a full re-parse). A
    // generation job we must act on is by construction recent: anything older
    // than RUNNING_SCAN_WINDOW ids has already hit its on-chain deadline, and
    // the expiry sweep — not a re-render — is what reclaims it.
    let next = client.get_next_job_id().await;
    let (run_from, run_to) = claim_scan_range(next, RUNNING_SCAN_WINDOW.max(claim_window));
    for id in run_from..run_to {
        if !gen_ids.contains(&id) {
            continue;
        }
        let job = match get_job(id) {
            Some(j) if is_gen(&j) => j,
            _ => continue,
        };
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            if state == "Running" && provider.as_deref() == Some(me) {
                // Skip a job we already proved wedges this host — re-rendering it
                // only to kill it again would stall the GPU with no progress.
                if is_wedged(id) {
                    continue;
                }
                run_job(client, id, job).await?;
            }
        }
    }
    Ok(())
}

async fn run_job(
    client: &ChainClient,
    id: u64,
    job: gpn_agent::llm::LlmJob,
) -> Result<(), Box<dyn std::error::Error>> {
    // Item #22 build-order step 3: provider-side policy hook. Refuse hard-harm
    // categories (defense in depth over the AP's moderation) and any model this
    // operator does not serve BEFORE spending GPU time; settle on-chain so the
    // requestor's escrow refunds and the capacity slot frees.
    let policy = gpn_agent::provider_policy::ProviderPolicy::from_env();
    if let gpn_agent::provider_policy::Decision::Refuse(r) = policy.check(&job.prompt, &job.model) {
        let cat = r.category.unwrap_or("model_policy");
        eprintln!(
            "[job {id}] \u{26a0} policy refusal (category={cat}) \u{2014} not running: {}",
            r.reason
        );
        let _ = LlmStore::new().set_error(id, &format!("gpn_policy_violation: {cat}"));
        match client.fail_job(id, 2).await {
            Ok(()) => println!("[job {id}] refused by provider policy \u{2014} escrow refunded"),
            Err(e) => eprintln!("[job {id}] fail_job (policy) failed: {e}"),
        }
        return Ok(());
    }
    // Item #22 build-order step 4: report/ban path. Refuse a job whose on-chain
    // requestor is on this operator's denylist (shared with the AP).
    if let Some((_state, _price, Some(requestor), _prov)) = client.get_job_state(id).await {
        if let Some(r) = policy.requestor_refused(&requestor) {
            eprintln!(
                "[job {id}] \u{26a0} requestor denylist refusal \u{2014} not running: {}",
                r.reason
            );
            let _ = LlmStore::new().set_error(id, "gpn_policy_violation: requestor_denylist");
            match client.fail_job(id, 2).await {
                Ok(()) => println!("[job {id}] requestor banned \u{2014} escrow refunded"),
                Err(e) => eprintln!("[job {id}] fail_job (denylist) failed: {e}"),
            }
            return Ok(());
        }
    }
    println!(
        "[job {id}] {} generation — model={} prompt=\"{:.40}\"",
        job.kind, job.model, job.prompt
    );
    let _ = LlmStore::new().set_status(id, "running");

    // Artifact output path.
    let ext = if job.kind == "image" { "png" } else { "mp4" };
    let out_dir = std::env::var("GPN_ARTIFACTS").unwrap_or_else(|_| {
        gpn_agent::utils::temp_dir()
            .join("gpn_artifacts")
            .to_string_lossy()
            .to_string()
    });
    std::fs::create_dir_all(&out_dir)?;
    let out_path = format!("{}/job_{}.{}", out_dir, id, ext);

    // Resolve empty/"auto" to a concrete default so the diffusers helper never
    // receives the literal "auto" (invalid HF repo id) and wedges the job.
    let model = match job.model.as_str() {
        "" | "auto" => {
            if job.kind == "video" {
                "ltx-video"
            } else {
                "sd-turbo"
            }
        }
        m => m,
    };
    let req_json = json!({
        "kind": job.kind,
        "model": model,
        "prompt": job.prompt,
        "negative_prompt": job.negative_prompt,
        "width": job.width.unwrap_or(512),
        "height": job.height.unwrap_or(512),
        "steps": job.steps.unwrap_or(30),
        "seed": job.seed.unwrap_or(0),
        "frames": job.frames.unwrap_or(16),
        "fps": job.fps.unwrap_or(8),
        "output": out_path,
    });
    let req_file = gpn_agent::utils::temp_path(&format!("gpn_diffuse_req_{}.json", id));
    std::fs::write(&req_file, serde_json::to_string(&req_json)?)?;

    // Run the diffusers helper (blocking; do it off the async runtime).
    // Yield the GPU to this job for its GPU-contending render ONLY (one GPU = one
    // job): the co-located node miner FULLY STOPS while the flag is held and
    // resumes the instant the guard drops. The guard is scoped to the helper
    // invocation and released BEFORE the on-chain settlement below — settlement
    // needs fresh blocks, and holding the flag across a block-waiting
    // `submit_result` would starve block production on a single-authority chain
    // and deadlock the provider (chain halt). No GPU is used during settlement.
    let script = diffuse_script();
    let timeout = diffuse_timeout(&job.kind);
    let req_file2 = req_file.clone();
    let out_res = {
        // Acquire INSIDE the blocking task: the wait can be long (up to
        // GPN_GPU_LOCK_TIMEOUT_SECS) and must not park an async worker.
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
            let _yield =
                gpn_agent::gpu_yield::GpuYieldGuard::acquire().map_err(|e| e.to_string())?;
            let mut cmd = std::process::Command::new("python3");
            cmd.arg(&script).arg(&req_file2);
            let (stdout, _stderr) = run_with_watchdog(cmd, timeout, &format!("job {id}"))?;
            serde_json::from_str(&stdout).map_err(|e| format!("bad helper output: {e} ({stdout})"))
        })
        .await
        .map_err(|e| e.to_string())?
    };
    // A wedged/timed-out helper is reported explicitly (never silent) and the
    // job is marked errored off-chain; on-chain it stays Running and is
    // reclaimed by the expiry sweep. The provider itself keeps serving.
    let out = match out_res {
        Ok(v) => v,
        Err(e) => {
            if e.contains("WEDGE") {
                eprintln!("[job {id}] ⚠ SILENT-WEDGE DETECTED — {e}");
                mark_wedged(id);
            } else if is_permanent_load_error(&e) {
                // Item #78: a model this host can never load (401/404/unknown
                // alias) will never succeed on a retry — fail the job so the
                // requestor's escrow refunds and our slot frees, instead of
                // leaving it `Running` until the on-chain deadline.
                mark_unloadable(resolve_model_alias(model));
                let me = client.signer_address_hex().unwrap_or_default();
                fail_unloadable_job(client, &me, id, model, &e).await;
                let _ = std::fs::remove_file(&req_file);
                return Ok(());
            } else {
                eprintln!("[job {id}] generation error: {e}");
            }
            let _ = LlmStore::new().set_error(id, &e);
            let _ = std::fs::remove_file(&req_file);
            return Ok(());
        }
    };

    if out.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        let artifact = out["artifact"].as_str().unwrap_or(&out_path).to_string();
        let media = out["media_type"]
            .as_str()
            .unwrap_or("application/octet-stream")
            .to_string();
        let hash = out["sha256"].as_str().unwrap_or("").to_string();
        let _ = LlmStore::new().complete_artifact(id, &artifact, &media, &hash);
        let mut hb = [0u8; 32];
        hb.copy_from_slice(&hex::decode(&hash)?);
        client.submit_result(id, hb, 0).await?;
        client.complete_job(id).await?;
        println!("[job {id}] done — artifact {artifact} ({media}), hash {hash:.16}…");
    } else {
        let err = out["error"]
            .as_str()
            .unwrap_or("unknown generation error")
            .to_string();
        let _ = LlmStore::new().set_error(id, &err);
        eprintln!("[job {id}] generation failed: {err}");
        // Same rationale as the helper-exit path above: a permanent model error
        // is not worth an on-chain deadline wait.
        if is_permanent_load_error(&err) {
            mark_unloadable(resolve_model_alias(model));
            let me = client.signer_address_hex().unwrap_or_default();
            fail_unloadable_job(client, &me, id, model, &err).await;
        }
    }
    let _ = std::fs::remove_file(&req_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> std::process::Command {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(script);
        c
    }

    #[test]
    fn watchdog_returns_output_on_success() {
        let (out, _err) = run_with_watchdog(sh("echo hello"), Duration::from_secs(10), "t_ok")
            .expect("should succeed");
        assert!(out.contains("hello"), "stdout was: {out}");
    }

    #[test]
    fn watchdog_kills_a_hung_helper_and_flags_wedge() {
        // A helper that never finishes must be killed at the budget, not hang forever.
        let start = std::time::Instant::now();
        let err = run_with_watchdog(sh("sleep 30"), Duration::from_secs(1), "t_wedge")
            .expect_err("expected a wedge error");
        assert!(err.contains("WEDGE"), "got: {err}");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "watchdog did not return promptly (took {:?})",
            start.elapsed()
        );
    }

    #[test]
    fn watchdog_reports_nonzero_exit_without_calling_it_a_wedge() {
        let err = run_with_watchdog(
            sh("echo boom >&2; exit 3"),
            Duration::from_secs(10),
            "t_fail",
        )
        .expect_err("expected a failure");
        assert!(err.contains("boom"), "got: {err}");
        assert!(!err.contains("WEDGE"), "false wedge: {err}");
    }

    #[test]
    fn diffuse_timeout_video_is_at_least_image() {
        assert!(diffuse_timeout("video") >= diffuse_timeout("image"));
        assert!(diffuse_timeout("image") > Duration::from_secs(0));
    }

    #[test]
    fn wedged_marker_roundtrip() {
        assert!(!is_wedged(424242));
        mark_wedged(424242);
        assert!(is_wedged(424242));
    }

    // ---- item #78: unloadable models are failed once, never retried forever ----

    #[test]
    fn aliases_resolve_to_the_same_repo_the_helper_loads() {
        // The AP's default gen model, the #76 repro name, and the video alias.
        assert_eq!(resolve_model_alias("sd-turbo"), "stabilityai/sd-turbo");
        assert_eq!(resolve_model_alias("sdxl-turbo"), "stabilityai/sdxl-turbo");
        assert_eq!(resolve_model_alias("ltx-video"), "Lightricks/LTX-Video");
        // A real repo id (and a local path) must pass through untouched.
        assert_eq!(
            resolve_model_alias("stabilityai/stable-diffusion-2-1"),
            "stabilityai/stable-diffusion-2-1"
        );
        assert_eq!(resolve_model_alias("/models/local"), "/models/local");
    }

    #[test]
    fn permanent_load_errors_are_told_apart_from_transient_ones() {
        // The live 01:34 failure: a retired/nonexistent repo answered 401.
        assert!(is_permanent_load_error(
            "Repository Not Found for url: https://huggingface.co/api/models/sdxl-turbo. Please make sure you specified the repo id. (401 Client Error)"
        ));
        assert!(is_permanent_load_error(
            "OSError: sdxl-turbo is not a valid model identifier"
        ));
        assert!(is_permanent_load_error("GatedRepoError: 403 Client Error"));
        // Transient: network/disk problems must NEVER fail a servable job.
        assert!(!is_permanent_load_error(
            "ConnectionError: HTTPSConnectionPool … Max retries exceeded"
        ));
        assert!(!is_permanent_load_error(
            "No space left on device (os error 28)"
        ));
        assert!(!is_permanent_load_error("CUDA out of memory"));
    }

    #[test]
    fn transient_pull_failures_give_up_after_a_bounded_number_of_attempts() {
        // 4 attempts in 8 s is exactly the storm #78 reported; the budget must
        // stop at MAX_PULL_ATTEMPTS instead of retrying at the poll cadence.
        assert!(!pull_give_up(1));
        assert!(!pull_give_up(MAX_PULL_ATTEMPTS - 1));
        assert!(pull_give_up(MAX_PULL_ATTEMPTS));
        assert!(pull_give_up(MAX_PULL_ATTEMPTS + 7));
    }

    #[test]
    fn helper_json_error_is_the_reported_reason() {
        // gpn_diffuse.py prints its failure on stdout and exits 1; the watchdog
        // must surface that text (not an empty stderr) so it can be classified.
        assert_eq!(
            json_reason(r#"{"ok": false, "error": "OSError: Repository Not Found (401)"}"#)
                .as_deref(),
            Some("OSError: Repository Not Found (401)")
        );
        // A success payload is not an error reason, and noise is ignored.
        assert!(json_reason(r#"{"ok": true, "artifact": "/tmp/a.png"}"#).is_none());
        assert!(json_reason("Traceback (most recent call last):").is_none());
    }

    #[test]
    fn unloadable_registry_roundtrip() {
        let repo = "example/definitely-not-a-real-repo-for-test";
        assert!(!is_unloadable(repo));
        mark_unloadable(repo);
        assert!(is_unloadable(repo));
        assert!(!is_failed_locally(918273));
        mark_failed_locally(918273);
        assert!(is_failed_locally(918273));
    }

    // ---- item #59: the claim scan must stay bounded and rate-limited ----

    #[test]
    fn claim_scan_only_reads_the_newest_window() {
        // The whole point of #59: 6 371 ids must not become 6 371 RPCs.
        assert_eq!(claim_scan_range(6371, 512), (5859, 6371));
        assert_eq!(
            claim_scan_range(6371, 512).1 - claim_scan_range(6371, 512).0,
            512
        );
        // A young chain scans everything from zero (no negative/undersized start).
        assert_eq!(claim_scan_range(7, 512), (0, 7));
        assert_eq!(claim_scan_range(0, 512), (0, 0));
        // A degenerate window still scans exactly one id, never `from > to`.
        assert_eq!(claim_scan_range(10, 0), (9, 10));
    }

    #[test]
    fn heavy_passes_are_rate_limited_but_never_starve() {
        let now = Instant::now();
        // Never run before => due.
        assert!(due(None, now, 30_000));
        // Just ran => not due (this is what stops the per-2 s RPC storm).
        assert!(!due(Some(now), now, 30_000));
        // Interval elapsed => due again.
        assert!(due(Some(now - Duration::from_millis(30_001)), now, 30_000));
        // A stamp in the future (clock jumped backwards) must not panic, and must
        // not be read as "a huge elapsed time" either — it simply waits.
        assert!(!due(Some(now + Duration::from_secs(3600)), now, 30_000));
    }
}
