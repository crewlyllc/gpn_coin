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
use gpn_agent::llm::{
    get_job, run_inference, run_inference_with_usage, set_response_with_usage, LlmStore,
};
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

/// Item #49: at most one "waiting for the create" line per id per minute, so a
/// slow create cannot spam the journal (and the counter above, which is reset by
/// unrelated arms, cannot make the warning chatter).
fn should_announce_wait(job_id: u64) -> bool {
    use std::time::{Duration, Instant};
    static LAST: OnceLock<Mutex<HashMap<u64, Instant>>> = OnceLock::new();
    let m = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let mut m = m.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match m.get(&job_id) {
        Some(t) if now.duration_since(*t) < Duration::from_secs(60) => false,
        _ => {
            m.insert(job_id, now);
            true
        }
    }
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

/// Item #74: how a failed `accept_job` should be treated.
///
/// A *transient* failure (the requestor's escrow cannot pay the accept-time
/// charge — `TokenError::FundsUnavailable` — or the tx never made it into a
/// block) must be retried later with backoff: the old code logged it and moved
/// on, so the job sat `Pending` until its on-chain deadline and the requestor's
/// escrow was refunded for work nobody ever attempted. A *permanent* failure
/// (the chain says the job is not in a claimable state, we are not its
/// provider, we are not registered, we are under the compute floor) must NOT be
/// retried: re-sending the same call can only fail again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceptOutcome {
    /// Worth trying again after a backoff.
    Retry,
    /// Re-sending the same call cannot succeed; stop.
    GiveUp,
}

const ACCEPT_RETRY_MAX: u32 = 5;
const ACCEPT_RETRY_BASE_MS: u64 = 2_000;
const ACCEPT_RETRY_CAP_MS: u64 = 60_000;

/// Classify an `accept_job` error string. Deliberately string-based: the chain
/// client surfaces a dispatch failure as `tx included but FAILED dispatch:
/// <pallet>::<ErrorName>`, and pool/connection failures as free text.
/// Unknown errors resolve to `GiveUp` — never a hot retry loop on a cause we do
/// not understand.
fn classify_accept_error(msg: &str) -> AcceptOutcome {
    let m = msg.to_ascii_lowercase();
    // State errors answer "no, and asking again changes nothing" until some
    // *other* event moves the job — the reconcile/adoption paths own those.
    const PERMANENT: [&str; 8] = [
        "jobnotpending",
        "jobnotfound",
        "alreadyaccepted",
        "notjobprovider",
        "providernotregistered",
        "providercomputeinsufficient",
        "badorigin",
        "call filter",
    ];
    if PERMANENT.iter().any(|p| m.contains(p)) {
        return AcceptOutcome::GiveUp;
    }
    // `MaxProviderCapacity` is transient (a slot frees when our running job
    // settles), and the funding/pool/timeout class is transient by definition.
    const TRANSIENT: [&str; 11] = [
        "fundsunavailable",
        "funds unavailable",
        "insufficient",
        "priority is too low",
        "transaction is outdated",
        "tx inclusion timed out",
        "timed out",
        "subscription dropped",
        "connection",
        "maxprovidercapacity",
        "pool is full",
    ];
    if TRANSIENT.iter().any(|p| m.contains(p)) {
        AcceptOutcome::Retry
    } else {
        AcceptOutcome::GiveUp
    }
}

/// True when the failure is the requestor being unable to fund the accept.
fn is_requestor_unfunded(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("fundsunavailable") || m.contains("funds unavailable")
}

/// Exponential backoff for the Nth retry (0-based): 2 s, 4 s, 8 s, 16 s, 32 s,
/// capped at 60 s. Bounded so a long-lived unfunded requestor cannot pin a GPU
/// slot, and so a retry never waits longer than one block-time-scale window.
fn accept_backoff_ms(attempts: u32) -> u64 {
    ACCEPT_RETRY_BASE_MS
        .saturating_mul(1u64 << attempts.min(6))
        .min(ACCEPT_RETRY_CAP_MS)
}

/// The retry a failed accept earns, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcceptRetryPlan {
    /// 1-based retry number (how many attempts have now been made).
    attempt: u32,
    delay_ms: u64,
    requestor_unfunded: bool,
}

/// Pure decision: given how many attempts have failed so far, does this error
/// earn another one?
fn accept_retry_plan(attempts: u32, err: &str) -> Option<AcceptRetryPlan> {
    if classify_accept_error(err) == AcceptOutcome::GiveUp || attempts >= ACCEPT_RETRY_MAX {
        return None;
    }
    Some(AcceptRetryPlan {
        attempt: attempts + 1,
        delay_ms: accept_backoff_ms(attempts),
        requestor_unfunded: is_requestor_unfunded(err),
    })
}

#[derive(Debug, Clone, Copy)]
struct AcceptRetryState {
    attempts: u32,
    next_at: Instant,
}

fn accept_retry_state() -> &'static Mutex<HashMap<u64, AcceptRetryState>> {
    static M: OnceLock<Mutex<HashMap<u64, AcceptRetryState>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// May this job be claimed right now, or are we inside an accept backoff?
fn accept_retry_gate(job_id: u64) -> bool {
    let m = accept_retry_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match m.get(&job_id) {
        Some(s) => Instant::now() >= s.next_at,
        None => true,
    }
}

/// Record a failed accept. Returns the plan when another attempt is scheduled;
/// `None` means give up (state error, unknown error, or budget exhausted).
fn accept_retry_record(job_id: u64, err: &str) -> Option<AcceptRetryPlan> {
    let mut m = accept_retry_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let attempts = m.get(&job_id).map(|s| s.attempts).unwrap_or(0);
    match accept_retry_plan(attempts, err) {
        Some(p) => {
            m.insert(
                job_id,
                AcceptRetryState {
                    attempts: p.attempt,
                    next_at: Instant::now() + Duration::from_millis(p.delay_ms),
                },
            );
            Some(p)
        }
        None => {
            m.remove(&job_id);
            None
        }
    }
}

/// A successful accept (or a job that is no longer ours to claim) clears any
/// backoff, so a later re-assignment starts with a fresh budget.
fn accept_retry_clear(job_id: u64) {
    accept_retry_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&job_id);
}

/// Log one accept failure at the right level, with the retry countdown.
fn log_accept_failure(job_id: u64, err: &str, plan: Option<AcceptRetryPlan>) {
    match plan {
        Some(p) => {
            let left = ACCEPT_RETRY_MAX - p.attempt;
            if p.requestor_unfunded {
                eprintln!(
                    "[provider] accept #{job_id} failed: requestor escrow unfunded — {left} retries \
                     left (retry {}/{ACCEPT_RETRY_MAX} in {}s): {err}",
                    p.attempt,
                    p.delay_ms / 1000
                );
            } else {
                eprintln!(
                    "[provider] accept #{job_id} failed (transient — retry {}/{ACCEPT_RETRY_MAX} in \
                     {}s, {left} retries left): {err}",
                    p.attempt,
                    p.delay_ms / 1000
                );
            }
        }
        None => eprintln!(
            "[provider] accept #{job_id} failed permanently — not retrying (the reconcile/adoption \
             path owns the job from here): {err}"
        ),
    }
}

/// What an on-chain read says about whether a job may be claimed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimRead {
    /// Chain has it in `Pending` — accept it.
    Claim,
    /// Chain has it, but in another state (already accepted/running/terminal).
    NotPending,
    /// Chain does not have the id at all yet: the AP's create is still in the
    /// pool. Claiming now would revert (`accept_job` needs the job to exist),
    /// so wait for inclusion instead (item #49).
    NotYetOnChain,
}

fn claim_read_gate(read: Option<&str>) -> ClaimRead {
    match read {
        Some("Pending") => ClaimRead::Claim,
        Some(_) => ClaimRead::NotPending,
        None => ClaimRead::NotYetOnChain,
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
/// One-line, once-per-transition logging for idle mode. The claim loop runs at
/// 300 ms, so an unthrottled notice would flood the log (the same lesson as the
/// reconcile pass).
fn idle_notice_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IDLE: AtomicBool = AtomicBool::new(false);
    if !IDLE.swap(true, Ordering::Relaxed) {
        println!(
            "[provider] idle mode: mining + new job claims are PAUSED (node keeps syncing); \
             resumes automatically at the deadline or on `gpn_node resume`"
        );
    }
}

fn idle_clear_notice() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IDLE: AtomicBool = AtomicBool::new(false);
    if IDLE.swap(false, Ordering::Relaxed) {
        println!("[provider] idle ended, resuming job claims");
    }
}

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

/// State for the provider side of the AP-hub off-chain escrow protocol
/// (item #2). The provider accepts signed assignments, runs the work, and signs
/// the result + its half of the channel settlement — all without a tx.
pub struct ChannelServeCtx {
    pub signer: std::sync::Arc<Sr25519Signer>,
    pub me: gpn_agent::hub_channel::PubKey,
    pub state_path: std::path::PathBuf,
    /// Chain client used to verify a channel exists/is funded on first sight.
    pub chain: Option<gpn_agent::chain::ChainClient>,
    /// Escape hatch for offline/dev (`GPN_CHANNEL_SKIP_CHAIN_CHECK=1`).
    pub skip_chain_check: bool,
}

type ProviderChannels = HashMap<u64, gpn_agent::hub_channel::ProviderChannel>;

/// Item #75: a process that died — or a request that panicked, item #101 —
/// between "persist the pre-paid accept" and "return the signed result" leaves
/// our mirror one nonce ahead of the AP's, which wedges the whole channel lane
/// (every later assignment is rejected `StaleStateBefore`). Rewind those
/// expired, unanswered accepts and persist the result. Returns the count.
fn rewind_stale_accepts(path: &std::path::Path, why: &str) -> usize {
    let mut chans: ProviderChannels = match std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(c) => c,
        None => return 0,
    };
    let now = gpn_agent::hub_channel::now_ms();
    let mut rewound = 0;
    for ch in chans.values_mut() {
        let n = ch.reconcile_stale_accepts(now);
        if n > 0 {
            println!(
                "[channel] channel {}: rewound {n} expired unanswered accept(s) to nonce {} — {why}",
                ch.channel_id, ch.state.nonce
            );
        }
        rewound += n;
    }
    if rewound > 0 {
        save_provider_channels(path, &chans);
    }
    rewound
}

fn load_provider_channels(path: &std::path::Path) -> ProviderChannels {
    // At load, i.e. before any new assignment is considered.
    rewind_stale_accepts(
        path,
        "the AP never acknowledged them (previous process died mid-job)",
    );
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_provider_channels(path: &std::path::Path, chans: &ProviderChannels) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(chans) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// `POST /channel/job` — one off-chain escrowed job.
///
/// Body: a signed `Assignment` (see `gpn_agent::hub_channel`). Response: a
/// signed `JobResultMsg` (output + payment update), or `{ok:false,error}`.
fn serve_channel_job(
    ctx: &ChannelServeCtx,
    rt: &tokio::runtime::Runtime,
    body: &str,
) -> (u16, serde_json::Value) {
    use gpn_agent::hub_channel::{Assignment, JobResultMsg, ProviderChannel, PubKey};

    let a: Assignment = match serde_json::from_str(body) {
        Ok(a) => a,
        Err(e) => {
            return (
                400,
                serde_json::json!({"ok": false, "error": format!("bad assignment: {e}")}),
            )
        }
    };
    if a.provider != ctx.me {
        return (
            409,
            serde_json::json!({"ok": false, "error": "assignment names a different provider"}),
        );
    }

    let mut chans = load_provider_channels(&ctx.state_path);
    let first_sight = !chans.contains_key(&a.channel_id);

    if first_sight {
        if !ctx.skip_chain_check {
            let Some(chain) = ctx.chain.as_ref() else {
                return (
                    503,
                    serde_json::json!({"ok": false, "error": "no chain client to verify the channel"}),
                );
            };
            let Some(c) = rt.block_on(chain.get_channel(a.channel_id)) else {
                return (
                    409,
                    serde_json::json!({"ok": false, "error": "unknown channel id"}),
                );
            };
            let want_req = format!("0x{}", hex::encode(a.requestor.0));
            if !c.is_open
                || c.provider_hex != format!("0x{}", hex::encode(ctx.me.0))
                || c.requestor_hex != want_req
                || c.deposit < a.deposit
            {
                return (
                    409,
                    serde_json::json!({"ok": false, "error": "channel does not match the assignment"}),
                );
            }
            chans.insert(
                a.channel_id,
                ProviderChannel::new(a.channel_id, c.deposit, PubKey(a.requestor.0)),
            );
        } else {
            chans.insert(
                a.channel_id,
                ProviderChannel::new(a.channel_id, a.deposit, PubKey(a.requestor.0)),
            );
        }
    }

    let ch = chans.get_mut(&a.channel_id).unwrap();
    // The diffusion provider shares this account's channel; the AP's signed
    // `state_before` is canonical, so follow it whenever our mirror lags.
    ch.sync_to_assignment(&a);
    if let Err(e) = ch.accept(&a) {
        return (
            409,
            serde_json::json!({"ok": false, "error": format!("assignment rejected: {e}")}),
        );
    }
    // Persist the accepted (pre-paid) state BEFORE running the work, so a crash
    // mid-inference cannot lose the requestor's signed payment.
    save_provider_channels(&ctx.state_path, &chans);

    let model_r = resolve_model(&a.model);
    let temp = (a.temperature_milli as f32) / 1000.0;
    // Item #75: this lane is the DOMINANT path (an off-chain escrowed chat is
    // served here, not through the chain claim loop) and it ran inference with
    // NO GPU lease at all — so "one GPU = one job" and "mining fully stops for
    // the job's duration" (#24) were both silently false for most traffic. The
    // payment state is already persisted above, so the lease is now held from
    // accept through completion: a concurrent image render either waits its
    // turn or gets a crisp "GPU busy" instead of a VRAM collision.
    let job_started = Instant::now();
    // `spawn_blocking` must be *called* inside a runtime context, so the spawn
    // happens when this async block is polled — not before `block_on`.
    let acquired = rt.block_on(async {
        tokio::task::spawn_blocking(gpn_agent::gpu_yield::GpuYieldGuard::acquire).await
    });
    let gpu_wait = job_started.elapsed();
    let gpu = match acquired {
        Ok(Ok(g)) => Ok(g),
        Ok(Err(e)) => Err(format!("GPU busy: {e}")),
        Err(join) => Err(format!("GPU lock task failed: {join}")),
    };
    let infer_started = Instant::now();
    let out = match &gpu {
        Ok(_) => rt.block_on(run_inference_with_usage(
            &model_r,
            &a.prompt,
            temp,
            a.max_tokens.max(1),
        )),
        Err(msg) => {
            eprintln!("[channel] job {} not run: {msg}", a.job_id);
            Err(msg.clone().into())
        }
    };
    let infer_took = infer_started.elapsed();
    let ch = chans.get_mut(&a.channel_id).unwrap();
    let msg: JobResultMsg = match out {
        Ok((text, _hash, usage)) => {
            let (pt, ct) = usage
                .map(|u| (u.prompt_tokens, u.completion_tokens))
                .unwrap_or((0, 0));
            ch.sign_result(&ctx.signer, a.job_id, &text, pt, ct)
                .unwrap_or_else(|e| panic!("sign_result after accept must succeed: {e}"))
        }
        Err(e) => {
            let err = e.to_string();
            ch.sign_refund(&ctx.signer, a.job_id, &err)
                .unwrap_or_else(|e| panic!("sign_refund after accept must succeed: {e}"))
        }
    };
    let balance_provider = ch.state.balance_provider;
    save_provider_channels(&ctx.state_path, &chans);
    println!(
        "[channel] job {} on channel {} {} (provider balance {}) timing: gpu-wait={} infer={} accept\u{2192}done={}",
        a.job_id,
        a.channel_id,
        if msg.ok { "served" } else { "refunded" },
        balance_provider,
        fmt_dur(Some(gpu_wait)),
        fmt_dur(Some(infer_took)),
        fmt_dur(Some(job_started.elapsed()))
    );
    // The lease is released here, i.e. after the signed result and the channel
    // state are persisted — the whole job, not just the inference call.
    drop(gpu);
    (200, serde_json::to_value(&msg).unwrap_or_default())
}

/// Off-chain, chain-free inference endpoint ("passthrough" mode).
/// `POST /infer {model,prompt,temperature,max_tokens}` → `{ok,text,model}`. NO
/// chain, escrow, commitment, or billing — free/private tier. Loopback bind.
///
/// Also serves `POST /channel/job`: the off-chain **escrowed** path (item #2),
/// where payment is a mutually-signed channel update instead of a tx.
fn spawn_infer_server(listen: &str, chan: Option<ChannelServeCtx>) {
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
        if chan.is_some() {
            println!("[channel] off-chain escrow endpoint on http://{listen} (POST /channel/job)");
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("infer runtime");
        for req in server.incoming_requests() {
            // Item #101: one request must never take the lane down with it.
            //
            // Before this, every request ran bare on the ONE thread that owns
            // this port, so any panic inside the handler unwound out of the
            // `incoming_requests()` loop and killed the thread for the rest of
            // the process's life: the listener was dropped (nothing bound to
            // :8094 any more), every later chat silently fell back to on-chain
            // escrow, and the systemd unit still read `active` with a healthy
            // PID — a silent, permanent degradation of the dominant lane.
            // Observed live on 2026-10-23 (a panic in the channel handler,
            // item #75's own first cut). `gpn_diffusion_provider` already
            // isolates per request by spawning a thread; this lane does not.
            // The panicking request itself is dropped (no result is signed for
            // it and the client sees a closed connection, which the AP already
            // classifies as not-attempted), but the lane survives.
            let ok = with_panic_guard("request handler", || {
                handle_infer_request(req, &rt, chan.as_ref())
            });
            if !ok {
                // The panic may have left a pre-paid accept persisted with no
                // signed result (`serve_channel_job` writes that BEFORE the
                // work). Expired ones are rewound by the next request's load;
                // this also heals any that have since timed out.
                if let Some(ctx) = chan.as_ref() {
                    let _ = rewind_stale_accepts(
                        &ctx.state_path,
                        "the request panicked before a result was signed (#101)",
                    );
                }
            }
        }
    });
}

/// Run one fallible piece of request handling without letting a panic escape.
///
/// Returns `true` when it completed. A panic is logged LOUDLY (an unwinding
/// handler is never acceptable as a silent background event) and swallowed, so
/// the caller's loop keeps serving.
fn with_panic_guard<F: FnOnce()>(what: &str, f: F) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(()) => true,
        Err(_) => {
            eprintln!(
                "⚠ [infer] {what} panicked — the listening lane stays up; this request is \
                 dropped without a result (the client retries / the AP fails over)"
            );
            false
        }
    }
}

/// Handle exactly one HTTP request on the passthrough/channel lane.
fn handle_infer_request(
    mut req: tiny_http::Request,
    rt: &tokio::runtime::Runtime,
    chan: Option<&ChannelServeCtx>,
) {
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if req.method() == &tiny_http::Method::Get && path == "/channel/status" {
        // Capability probe: the AP checks this BEFORE opening (and
        // funding) a channel, so a provider that cannot serve channel
        // jobs never costs the AP a locked deposit.
        let hdr =
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let body = match chan {
            Some(ctx) => {
                let ids: Vec<u64> = load_provider_channels(&ctx.state_path)
                    .keys()
                    .copied()
                    .collect();
                serde_json::json!({
                    "ok": true, "channel": true,
                    "provider": ctx.me.to_hex(), "channels": ids,
                })
            }
            None => serde_json::json!({"ok": false, "channel": false}),
        };
        let _ = req.respond(tiny_http::Response::from_string(body.to_string()).with_header(hdr));
        return;
    }
    if req.method() == &tiny_http::Method::Post && path == "/channel/job" {
        let mut body = String::new();
        let _ = req.as_reader().read_to_string(&mut body);
        let hdr =
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        let (code, resp) = match chan {
            Some(ctx) => serve_channel_job(ctx, rt, &body),
            None => (
                404,
                serde_json::json!({"ok": false, "error": "channel mode disabled on this provider"}),
            ),
        };
        let _ = req.respond(
            tiny_http::Response::from_string(resp.to_string())
                .with_header(hdr)
                .with_status_code(code),
        );
        return;
    }
    let is_infer = req.method() == &tiny_http::Method::Post && path == "/infer";
    if !is_infer {
        let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        return;
    }
    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("auto");
    let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
    let temp = v.get("temperature").and_then(|x| x.as_f64()).unwrap_or(0.7) as f32;
    let max_tokens = v.get("max_tokens").and_then(|x| x.as_u64()).unwrap_or(256) as u32;
    let model_r = resolve_model(model);
    // One GPU = one job, on this path too: the passthrough `/infer`
    // endpoint drives the same GPU as the chain/channel lanes and as
    // gpn_diffusion_provider, so it must take the same cross-process
    // lock. (It did NOT before 2026-10-22 — a direct /infer call during
    // a render collided in VRAM just like chat did.)
    let resp = match gpn_agent::gpu_yield::GpuYieldGuard::acquire() {
        Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
        Ok(_yield) => match rt.block_on(run_inference(&model_r, prompt, temp, max_tokens)) {
            Ok((text, _hash)) => {
                serde_json::json!({"ok": true, "text": text, "model": model_r})
            }
            Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
        },
    };
    let hdr =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let _ = req.respond(tiny_http::Response::from_string(resp.to_string()).with_header(hdr));
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
    // Item #97: name the unit the attested number is in, once, at boot.
    println!("[attest] gflops-unit-v97-provider: only an idle measurement is submitted");
    println!("[attest] {}", gpn_agent::compute_unit::describe());
    println!(
        "[attest] eligibility floor {} GFLOPS — {}",
        gpn_agent::compute_unit::MIN_ELIGIBLE_GFLOPS,
        gpn_agent::compute_unit::floor_derivation()
    );

    // Item #61: do not start serving on an unreadable chain. `gpn-provider` came
    // up 15 s before the RPC after the 04:40 full-stack restart, read `NextJobId`
    // as 0, and replayed eight completed jobs as "not on chain yet". Wait
    // (bounded) for a real answer; if it never comes, start DEGRADED — the pass
    // gate then refuses to claim until the chain answers (see
    // `warn_chain_unreadable_once`).
    {
        let (tries, delay_ms) = startup_ready_budget_from_env();
        match wait_for_chain_ready(&client, tries, delay_ms).await {
            Some(tip) => println!("[provider] chain ready: NextJobId={tip}"),
            None => eprintln!(
                "[provider] chain still unreadable after {}ms — starting in DEGRADED mode \
                 (nothing will be claimed or replayed until NextJobId reads back)",
                tries as u64 * delay_ms
            ),
        }
    }

    // #46 Phase 1 — provider-PULL assignments. The AP can only push to us if our
    // heartbeat port is inbound-reachable; behind NAT/CGNAT it never is. The pull
    // loop asks the AP for our own assignments instead and writes them into the
    // same stores the claim loop already reads (push stays on unless
    // GPN_ASSIGN_SOURCE=pull).
    //
    // Queue #65: nothing in a modern node ever attests, so the on-chain
    // `compute_gflops` is frozen at whatever a legacy `gpn-agent` run measured
    // (box1: 188, taken before the GFLOPS-unit fix) and eventually goes stale —
    // a capable node then routes as a weak one forever. Measure and (re)attest
    // here, once, before the claim loop starts: this process has not submitted a
    // single nonce yet, so it cannot race the claim lane (the #31 class).
    //
    // Item #93: a busy GPU defers the attempt instead of measuring under
    // contention; `attest_retry` below then retries from the main loop, i.e.
    // still between claim passes, so the nonce never races the claim lane.
    let mut attest_retry = gpn_agent::attest_idle::RetrySchedule::from_env();
    let mut attest_at = match maybe_refresh_attestation(&client, &addr).await {
        outcome if outcome.wants_retry() => {
            let delay = attest_retry.on_busy();
            if let AttestOutcome::SkippedBusy(reason) = &outcome {
                eprintln!("[attest] deferred ({reason}) — retrying in {delay}s");
            } else {
                eprintln!("[attest] attempt did not complete — retrying in {delay}s");
            }
            Some(std::time::Instant::now() + std::time::Duration::from_secs(delay))
        }
        _ => {
            attest_retry.reset();
            None
        }
    };
    if let Some(cfg) = gpn_agent::assign_pull::PullConfig::from_env(&me) {
        // #100: this process serves text inference only — its poll must not
        // advertise the image/video/fine-tune capabilities of its co-located
        // siblings, or the AP will route work here that nothing will claim.
        gpn_agent::assign_pull::spawn(cfg.with_capabilities(&["LLMInference"]));
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
        // Item #2: accept off-chain escrowed jobs over the same loopback server.
        let chan = {
            let mode = std::env::var("GPN_ESCROW_MODE")
                .unwrap_or_else(|_| "channel".to_string())
                .to_ascii_lowercase();
            if mode == "onchain" || mode == "off" {
                None
            } else {
                let state_path = match std::env::var("GPN_PROVIDER_CHANNEL_STATE") {
                    Ok(p) => std::path::PathBuf::from(p),
                    Err(_) => {
                        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                        std::path::PathBuf::from(format!("{home}/.gpn/provider_channels.json"))
                    }
                };
                gpn_agent::hub_channel::PubKey::from_hex(&me).map(|pk| ChannelServeCtx {
                    signer: std::sync::Arc::new(
                        Sr25519Signer::from_uri(&signer_uri).expect("channel signer"),
                    ),
                    me: pk,
                    state_path,
                    chain: Some(client.clone()),
                    skip_chain_check: std::env::var("GPN_CHANNEL_SKIP_CHAIN_CHECK")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false),
                })
            }
        };
        spawn_infer_server(&listen, chan);
    }

    // Item #72 — progress-based liveness. The loop below had no bounded wait:
    // a chain read over a half-open websocket, a stalled block import or any
    // future unbounded `await` just parks it, the unit stays `active`, nothing
    // is logged, and the only symptom is a job that never moves (the 20:24
    // wedge: ~23 min silent with #100 sitting on-chain `Pending`). Every phase
    // transition now bumps `~/.gpn/provider-tick.json`, and a plain thread (a
    // wedged runtime must not supervise itself) turns a stall into a WARN + a
    // real restart via `Restart=always`.
    let progress = std::sync::Arc::new(std::sync::Mutex::new(
        gpn_agent::provider_watchdog::Progress::new(
            gpn_agent::provider_watchdog::tick_path(),
            gpn_agent::provider_watchdog::stale_after_from_env(),
        ),
    ));
    if gpn_agent::provider_watchdog::watchdog_enabled() {
        let budget = gpn_agent::provider_watchdog::stale_after_from_env();
        println!(
            "[watchdog] armed: no progress for {}s in a normal phase (gpu-wait/infer phase get \
             their own longer budgets) ⇒ WARN + exit {} for a restart; tick {}",
            budget.as_secs(),
            gpn_agent::provider_watchdog::STALL_EXIT_CODE,
            gpn_agent::provider_watchdog::tick_path().display()
        );
        gpn_agent::provider_watchdog::spawn(progress.clone(), Duration::from_secs(15));
    } else {
        eprintln!("[watchdog] DISABLED (GPN_PROVIDER_WATCHDOG=0) — a wedged loop will not restart");
    }

    // Item #73: adopt what we already own BEFORE the loop starts. A job claimed
    // by the previous process (and possibly outside the reconcile window) is
    // either resumed now or moved to a terminal state now — never left to the
    // chance of a later tick. Nothing has taken a nonce yet, so this cannot race
    // the claim lane.
    clear_stale_gpu_yield();
    startup_adopt(&client, &me, &addr, fast_claim, &progress).await;
    set_phase(&progress, "idle");

    loop {
        // Item #93: retry a deferred attestation between passes. Running it here
        // (same task as the claim lane) keeps the nonce serialised: a second
        // concurrent submitter could read the same account nonce and one tx
        // would be dropped.
        if let Some(at) = attest_at {
            if std::time::Instant::now() >= at {
                let outcome = maybe_refresh_attestation(&client, &addr).await;
                if outcome.wants_retry() {
                    let delay = attest_retry.on_busy();
                    if let AttestOutcome::SkippedBusy(reason) = &outcome {
                        eprintln!(
                            "[attest] GPU still busy ({reason}) after {} attempt(s) — next try in \
                             {delay}s",
                            attest_retry.attempts()
                        );
                    } else {
                        eprintln!("[attest] retry did not complete — next try in {delay}s");
                    }
                    attest_at =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(delay));
                } else {
                    attest_retry.reset();
                    attest_at = None;
                }
            }
        }
        if let Err(e) = process_once(&client, &me, &addr, fast_claim, &progress).await {
            eprintln!("[provider] cycle error: {e}");
        }
        if let Ok(mut p) = progress.lock() {
            p.finish_pass();
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// Item #72: mark the step the provider is entering. Cheap, never held across
/// an `await`, and silently ignored if the cell is poisoned (a watchdog that
/// panics must not take the provider with it).
fn set_phase(
    progress: &std::sync::Arc<std::sync::Mutex<gpn_agent::provider_watchdog::Progress>>,
    phase: &str,
) {
    if let Ok(mut p) = progress.lock() {
        p.set_phase(phase);
    }
}

/// This box's VRAM in MiB (0 = unknown), probed once per process.
fn node_vram_mb() -> u64 {
    static VRAM: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *VRAM.get_or_init(gpn_agent::gpu::detect_vram_mb)
}

/// Resolve a requested model to a concrete one. `auto`/empty means "pick any
/// model this machine has" — choose the first from the local Ollama, falling
/// back to the guaranteed baseline model.
///
/// Item 3: models whose weights cannot fit this box's VRAM are removed from the
/// candidate set, so a job requesting `deepseek-r1:14b` on an 8 GB card is
/// served by a model that actually runs instead of OOMing mid-inference (or
/// silently crawling on CPU). Internal/toy models stay usable — this filters
/// on *fit*, not on the public catalogue.
fn resolve_model(requested: &str) -> String {
    let base = gpn_agent::llm::infer::default_ollama_url();
    let vram_mb = node_vram_mb();
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
    let installed: Vec<String> = installed
        .into_iter()
        .filter(|m| {
            let fit = gpn_agent::models::check_model_vram(m, vram_mb);
            if let Some(reason) = fit.skip_reason(m) {
                println!("[provider] {reason} — not claiming it");
                false
            } else {
                true
            }
        })
        .collect();
    let has = |name: &str| {
        installed
            .iter()
            .any(|n| n == name || n.split(':').next() == Some(name))
    };
    if !requested.is_empty() && requested != "auto" {
        if let Some(reason) =
            gpn_agent::models::check_model_vram(requested, vram_mb).skip_reason(requested)
        {
            println!("[provider] {reason} — falling back to a model that fits");
            return installed
                .iter()
                .find(|n| n.as_str() == "llama3.2:1b")
                .or_else(|| installed.first())
                .cloned()
                .unwrap_or_else(|| "llama3.2:1b".to_string());
        }
    }
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
fn read_settled_from_with_tip(
    path: &str,
    chain_tip: u64,
) -> (std::collections::HashSet<u64>, bool) {
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
/// attempt cap, or when the requested model is a kind this box can never serve.
fn should_attempt(id: u64, model: &str) -> bool {
    fail_count(id) < MAX_INFER_ATTEMPTS && unrunnable_reason(model).is_none()
}

/// Why this provider can never run `model`, or `None` if it can try (item #76).
///
/// Uses the SHARED classifier — the same table the access point admits and
/// routes with. A private copy here is how a diffusion model could be routed to
/// an LLM box and then skipped forever: the two lists drifted.
fn unrunnable_reason(model: &str) -> Option<String> {
    match gpn_agent::jobkind::model_kind(model) {
        Some(k) if k.is_generation() => Some(format!(
            "model '{model}' is a {} model — this is an LLM provider",
            k.label()
        )),
        _ => None,
    }
}

/// Item #76: a job this provider OWNS but provably cannot run must be failed
/// on-chain — escrow refunded, slots freed — never skipped silently.
///
/// The silent skip is what left a mis-kinded job `Running` until its on-chain
/// deadline: nothing was ever attempted, so the inference-failure path (and its
/// attempt cap) never ran. One failure per job per process (the chain call is
/// idempotent, the log line is not worth repeating every 300 ms).
async fn fail_unrunnable(client: &ChainClient, id: u64, why: &str, fast: bool) {
    static ONCE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u64>>> =
        std::sync::OnceLock::new();
    let once = ONCE.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    if let Ok(mut done) = once.lock() {
        if !done.insert(id) {
            return; // already failed in this process
        }
    }
    eprintln!(
        "[job {id}] cannot be run here: {why} — failing on-chain (reason {FAIL_CANNOT_RUN}); \
         escrow refunded, slot freed (item #76: never leave it Running)"
    );
    let _ = LlmStore::new().set_error(id, &format!("gpn_wrong_kind: {why}"));
    let r = if fast {
        client.fail_job_fast(id, FAIL_CANNOT_RUN).await
    } else {
        client.fail_job(id, FAIL_CANNOT_RUN).await
    };
    match r {
        Ok(()) => println!("[job {id}] failed on-chain after a kind check — escrow refunded"),
        Err(e) if is_benign_state_err(&e.to_string()) => {}
        Err(e) => eprintln!("[job {id}] fail_job (wrong kind) failed: {e}"),
    }
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
    // Item #53: report the backend's real token counts so the AP can answer with a
    // truthful OpenAI `usage` block for a REMOTE provider too (absent = omitted).
    if let Some(p) = j.prompt_tokens {
        body["prompt_tokens"] = serde_json::Value::from(p);
    }
    if let Some(c) = j.completion_tokens {
        body["completion_tokens"] = serde_json::Value::from(c);
    }
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

/// Item #56: how long a single `/api/pull` may block before we consider the
/// worker wedged and let another one start. Was 3600 s — a wedged pull could
/// hold a worker thread for an hour.
const MODEL_PULL_TIMEOUT_SECS: u64 = 600;

/// Also the in-flight window: while a prep worker started less than this ago is
/// running for a model, no second worker is started for it. A failed pull
/// records its finish time here, so a broken model is retried every ~10 min
/// instead of once per poll pass.
const MODEL_PREP_STUCK_AFTER: Duration = Duration::from_secs(MODEL_PULL_TIMEOUT_SECS);

/// Item #57: may the model-prep kick consider this assignment at all?
///
/// A job the chain has already settled (Completed/Failed/Cancelled/expired) is
/// written off in `llm-settled.json`, but it stays in `assignments.json` and in
/// the local store with a stale `queued` status forever. Without this gate the
/// pre-claim prep loop re-kicked `/api/tags` + `/api/pull` for it every
/// `MODEL_PRESENCE_TTL` (measured: 24 re-fires in ~40 min for #6253/#6254 after
/// they were cancelled on chain). Ids at/above the tip are predicted labels and
/// are never treated as settled (#29).
fn prep_candidate(id: u64, chain_tip: u64, settled: &std::collections::HashSet<u64>) -> bool {
    !(id < chain_tip && settled.contains(&id))
}

enum PrepAction {
    /// No worker holds this model (or the previous one is past the stuck
    /// window) — start one.
    Spawn,
    /// A worker is already working on it — do nothing.
    Busy,
}

/// Pure decision: may a prep worker be started for a model whose current book
/// entry is `prev`?
fn prep_action(prev: Option<Instant>, now: Instant) -> PrepAction {
    match prev {
        // `checked_` so a stamp in the future (clock moved backwards) reads as
        // "not in flight" rather than locking the model out until the clocks
        // agree again.
        Some(started) => match now.checked_duration_since(started) {
            Some(elapsed) if elapsed < MODEL_PREP_STUCK_AFTER => PrepAction::Busy,
            _ => PrepAction::Spawn,
        },
        None => PrepAction::Spawn,
    }
}

/// Model → when its last prep worker started (or finished, on failure).
fn model_prep() -> &'static Mutex<HashMap<String, Instant>> {
    static BOOK: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    BOOK.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pure: did `ensure_model` actually PULL, or did it find the model present?
/// Only a real pull may produce the operator-facing "was not installed"
/// notice — a cold presence cache is "unknown", never "absent" (item #58).
pub fn pull_happened(note: &str) -> bool {
    note.starts_with("pulled ")
}

/// Item #56: start (or find already running) a BACKGROUND probe+pull for
/// `model`, and return immediately.
///
/// Root cause this exists for: the pre-claim loop used to call `ensure_model`
/// synchronously for every assigned job, and `ensure_model` blocks on
/// `POST /api/pull` with a 3600 s timeout. One client asking for an uninstalled
/// model therefore parked the whole claim loop for 10 m 33 s (2026-10-21
/// 15:10) and every other job — all using an already-installed model — missed
/// the AP's 120 s window and was cancelled.
///
/// Correctness note: nothing is blocked *waiting* for this. The job can still
/// be claimed and run this pass — `resolve_model` already falls back to an
/// installed model when the requested one is absent — so a cold pull costs
/// model fidelity for a few jobs, never availability for all of them.
fn kick_model_prep(model: &str, job_id: Option<u64>) -> PrepAction {
    if model.is_empty() || model == "auto" {
        return PrepAction::Busy; // nothing concrete to prepare
    }
    if gpn_agent::llm::infer::model_presence_cached(model) == Some(true) {
        return PrepAction::Busy; // already present: zero work, zero HTTP
    }
    let now = Instant::now();
    {
        let Ok(mut book) = model_prep().lock() else {
            return PrepAction::Busy; // poisoned book: never block the claim lane
        };
        if let PrepAction::Busy = prep_action(book.get(model).copied(), now) {
            return PrepAction::Busy;
        }
        book.insert(model.to_string(), now);
    }
    let owned = model.to_string();
    std::thread::spawn(move || {
        match ensure_model(&owned) {
            Ok(note) => {
                // Item #58: the "not installed — pulling" notice belongs HERE,
                // not at the call site. `kick_model_prep` only knows the
                // presence CACHE (`None` on a cold cache = unknown, not
                // "absent"), so the caller used to announce a pull for models
                // that were already on disk and never got pulled (measured:
                // 7 notices / 6 claims in 40 min with 0 `POST /api/pull`).
                // `ensure_model` is the only place that knows, so it decides.
                if let Some(id) = job_id {
                    if pull_happened(&note) {
                        println!(
                            "[provider] job #{id}: model {owned} was not installed — pulled in \
                             the background (this job ran on the installed fallback meanwhile, so \
                             other jobs were NOT held up)"
                        );
                    }
                }
                println!("[provider] model prep: {note}");
            }
            Err(e) => {
                if !is_ollama_unavailable(&e) {
                    eprintln!("[provider] model prep {owned}: {e}");
                }
            }
        }
        if let Ok(mut book) = model_prep().lock() {
            if gpn_agent::llm::infer::model_presence_cached(&owned) == Some(true) {
                // Present now: the cache short-circuits future passes, so the
                // book entry is no longer needed.
                book.remove(&owned);
            } else {
                // Failed (or not confirmed): keep the finish time so the retry
                // waits out the stuck window instead of spinning on a pull.
                book.insert(owned.clone(), Instant::now());
            }
        }
    });
    PrepAction::Spawn
}

/// Ensure `model` is present locally; if not, pull it from Ollama / HuggingFace.
/// Returns a human-readable note about what happened.
///
/// BLOCKING — may sit in `/api/pull` for up to `MODEL_PULL_TIMEOUT_SECS`.
/// Never call this from the claim loop; use `kick_model_prep` there.
fn ensure_model(model: &str) -> Result<String, String> {
    if model.is_empty() || model == "auto" {
        return Ok("no specific model requested".into());
    }
    // Item #48: the claim loop re-asked this for every assigned-but-unfinished
    // job on EVERY pass, and each ask cost two blocking `/api/tags` GETs — a
    // measured ~2.4 req/s of ollama traffic (and ollama journal noise) that never
    // stopped, driven by long-stale `queued` store entries. A cached "present"
    // answer makes the steady state zero HTTP.
    if gpn_agent::llm::infer::model_presence_cached(model) == Some(true) {
        return Ok(format!("model {model} already present"));
    }
    let base = gpn_agent::llm::infer::default_ollama_url();
    let cli = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(MODEL_PULL_TIMEOUT_SECS))
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
                gpn_agent::llm::infer::remember_model_presence(model, true);
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
        gpn_agent::llm::infer::remember_model_presence(model, true);
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

/// How far back from the tip the startup adoption sweep (item #73) reads chain
/// state for ids it holds no live payload for. The assignment store grows
/// forever (2865 ids and counting), and one `get_job_state` per historical id
/// would stall startup for minutes — the same O(history) trap finding #34 hit.
/// An id older than this with no local payload cannot be resumed anyway (the
/// work is gone), and it cannot still be `Accepted`/`Running`: it would have
/// outlived `max_duration` and been reclaimed by the on-chain expiry sweep. Ids
/// with a *live* local payload (queued/running) are always examined regardless
/// of age — that is exactly the #97/#105 case.
const ADOPT_WINDOW: u64 = 512;

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
            let ineffective = if landed {
                prev.ineffective + 1
            } else {
                prev.ineffective
            };
            gate.insert(
                (id, step),
                HealAttempt {
                    last: now,
                    nonce: None,
                    ineffective,
                },
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
                HealAttempt {
                    last: now,
                    nonce: None,
                    ineffective: 0,
                },
            );
            true
        }
    }
}

/// Mark a transition as *just attempted* without a nonce (item #49). The claim
/// path submits `accept` through the same nonce stream as the heal path, so the
/// reconcile pass must not re-drive the same `accept` while the claim's tx is
/// still in the pool — that is one wasted extrinsic per job (the pallet answers
/// `JobNotPending` once it lands). Inserting the gate entry makes the ordinary
/// retry floor apply instead of an immediate duplicate submit.
fn heal_mark_attempt(id: u64, step: HealStep) {
    let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
    gate.entry((id, step)).or_insert(HealAttempt {
        last: Instant::now(),
        nonce: None,
        ineffective: 0,
    });
}

/// Record the nonce a heal transition was submitted with, so the next pass can
/// tell whether it was included. Called only right after a successful submit.
fn heal_record_nonce(id: u64, step: HealStep, nonce: u64) {
    let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = gate.get(&(id, step)).copied() {
        gate.insert(
            (id, step),
            HealAttempt {
                nonce: Some(nonce),
                ..prev
            },
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

// ── Item #61: chain-readiness before we replay anything ───────────────────
//
// A restart can bring us up before the node's RPC answers (the 04:40:00
// full-stack restart did: `gpn-provider` came up 15 s early). `NextJobId` then
// reads `0`, which is indistinguishable from a genesis chain, so the settled-set
// tip filter below keeps **nothing** and every historical assignment in
// `assignments.json` looks adoptable again — the log filled with
// `not on chain yet (create in flight)` replays of #3998/#5629/#6253-#6258.
// Item #51 taught the AP the difference between "the chain says 0" and "the read
// failed"; this is the provider half: wait a bounded while for a readable tip at
// startup, and if it is still unreadable, skip the pass instead of trusting a 0.
const STARTUP_READY_TRIES: u32 = 20;
const STARTUP_READY_DELAY_MS: u64 = 1_000;

/// `(tries, delay_ms)` for the startup readiness wait, floored/capped so a typo
/// can neither skip it (0 tries) nor hang startup for hours.
fn startup_ready_budget(tries: Option<u64>, delay_ms: Option<u64>) -> (u32, u64) {
    let tries = tries.unwrap_or(STARTUP_READY_TRIES as u64).clamp(1, 600) as u32;
    let delay = delay_ms.unwrap_or(STARTUP_READY_DELAY_MS).clamp(50, 5_000);
    (tries, delay)
}

fn startup_ready_budget_from_env() -> (u32, u64) {
    let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
    startup_ready_budget(num("GPN_STARTUP_READY_TRIES"), num("GPN_STARTUP_READY_MS"))
}

/// A pass may only run when the tip is **readable**. `Some(0)` is legitimate (a
/// brand-new chain); `None` means every chain read failed, and proceeding would
/// treat the empty settled set as authoritative.
fn chain_tip_readable(tip: Option<u64>) -> bool {
    tip.is_some()
}

// ─── Queue #65: keep this node's on-chain attestation real ──────────────────

/// What to do about the on-chain attestation, decided from
/// `(compute_gflops, submitted_at block)` — the exact two fields the pallet
/// keeps and the two that matter:
///
/// * no record at all → attest (the node was never attested at all)
/// * `compute_gflops == 0` → attest (a zero record is a broken measurement someone wrote: box1's complaint)
/// * older than `refresh_blocks` → attest (age = block number; `MaxAttestationAge` is 14 400)
/// * otherwise → keep it (re-attesting costs a tx and a GEMM run)
///
/// `head == 0` (unreadable) never counts as "old": without a head there is no
/// evidence of staleness, and a spurious attestation on every start is noise.
#[derive(Debug, PartialEq, Eq)]
enum AttestPlan {
    Attest,
    Skip,
}

fn attest_plan(rec: Option<(u64, u64)>, head: u64, refresh_blocks: u64) -> AttestPlan {
    match rec {
        None => AttestPlan::Attest,
        Some((0, _)) => AttestPlan::Attest,
        Some((_, at)) if head > at && head.saturating_sub(at) > refresh_blocks => {
            AttestPlan::Attest
        }
        Some(_) => AttestPlan::Skip,
    }
}

fn attest_refresh_blocks_from_env() -> u64 {
    std::env::var("GPN_ATTEST_REFRESH_BLOCKS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(7_200) // half of `MaxAttestationAge` (14 400 blocks ≈ 60 h)
}

/// `GPN_ATTEST_FORCE=1` — attest at the next opportunity even though the record
/// is young. **This is the explicit re-attest path** for a record that is wrong
/// but not provably impossible (e.g. box1's contended `188` on a 44 TFLOPS
/// card): freshness alone would otherwise keep it for up to 7 200 blocks.
fn attest_force_from_env() -> bool {
    std::env::var("GPN_ATTEST_FORCE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Set once the chain has answered a submission with `BelowEligibilityFloor`.
///
/// The runtime constant and this binary can disagree while a staged upgrade is
/// pending. Retrying a rejected submission every hour would be tx spam for a
/// condition only a governed runtime change can fix; a restart (after the
/// upgrade enacts) clears the flag and retries. Nothing is worked around: the
/// honest measurement simply is not submittable yet.
static FLOOR_REJECTED_BY_CHAIN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// `GPUAttestation::MinEligibilityGflops` in the **canonical measurement unit**
/// (`gpn_agent::compute_unit`). This is 250 GFLOPS, not 3 000: the old constant
/// was written on the spec-sheet-TFLOPS scale while the benchmark reports the
/// tiled GEMM throughput (~5.5 % of peak), so an honest 823 GFLOPS reading on a
/// 15 TFLOPS card was refused. The runtime constant is being changed to match
/// (staged; see `shared/artifacts/project7/staged/gflops-floor-v1.md`) — until
/// that upgrade enacts, a submission below the OLD floor is rejected on-chain
/// and reported as such instead of retried in a loop.
const ELIGIBILITY_FLOOR_GFLOPS: u64 = gpn_agent::compute_unit::MIN_ELIGIBLE_GFLOPS;

/// The floor that actually decides whether a submission is accepted: the
/// **chain's** `MinEligibilityGflops` when it is readable, raised to our own
/// canonical floor if the chain's is lower.
///
/// Item #104. The two numbers are allowed to disagree while a staged runtime
/// change is pending: this binary ships `250`
/// (`compute_unit::MIN_ELIGIBLE_GFLOPS`) but the live chain still carries the
/// old spec-sheet `3000`, so an honest `824 GFLOPS` measurement was submitted at
/// **every provider restart**, each one an included-but-failed extrinsic
/// (`GPUAttestation::BelowEligibilityFloor`), a burned fee and an `ERROR` line —
/// for a condition only a governed `set_code` can fix. Reading the live constant
/// makes the skip automatic and self-healing: the moment the upgrade enacts, the
/// same logic submits and succeeds with no code change.
///
/// `chain` is `None` when the constant could not be read; that is **not**
/// evidence, so the local floor is used (fail open, never silently permissive).
fn effective_eligibility_floor(local: u64, chain: Option<u64>) -> u64 {
    match chain {
        Some(f) => local.max(f),
        None => local,
    }
}

/// Should a fresh measurement replace the on-chain record?
///
/// A measurement may always **improve** the record. It must not casually
/// downgrade one — but a record that is *physically impossible* for the card in
/// the box is not a working attestation, it is a stale artefact, and it must be
/// correctable, otherwise the box is stuck with it forever (item #87).
///
/// That is exactly box1's and this host's history: before the
/// raw-FLOP/s → GFLOPS fix, `compute_gflops` was written in raw units
/// (≈ GFLOPS×1e9), so this 15.1 TFLOPS card carries **30 000** GFLOPS on chain —
/// twice its FP32 peak, which no GEMM can reach. `attest_measurement_useful`
/// used to require `measured > on_chain`, so an honest measurement could never
/// replace it: the record was permanent by construction.
///
/// * below the eligibility floor                    → keep (chain refuses it)
/// * better than the record                         → attest
/// * record above 1.5× the card's estimated FP32 peak → attest (legacy units:
///   a value above peak is not this card's capability under any measurement)
/// * otherwise                                      → keep (one tx saved)
///
/// `peak_gflops` comes from the model-name estimate (`detect_gpu_capability`);
/// 0 means "unknown device", in which case no downward correction is attempted.
fn attest_measurement_useful(measured: u64, on_chain: Option<u64>, peak_gflops: u64) -> bool {
    if measured < ELIGIBILITY_FLOOR_GFLOPS {
        return false;
    }
    let record = on_chain.unwrap_or(0);
    if record == 0 {
        return true;
    }
    if measured > record {
        return true;
    }
    // A record that cannot be this card's measurement is a legacy artefact, not
    // a capability claim: correcting it downward is the whole point (#97).
    gpn_agent::compute_unit::record_implausible(record, peak_gflops)
}

/// What a single attestation attempt did. The caller uses this to decide
/// whether to retry later (item #93: a busy GPU is a *deferral*, not a failure).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AttestOutcome {
    /// `GPN_ATTEST_REFRESH=0` — the operator opted out.
    Disabled,
    /// The on-chain record is current; nothing to do.
    UpToDate,
    /// The GPU was busy: nothing was measured and nothing was submitted.
    SkippedBusy(String),
    /// Measured on an idle GPU, but the measurement does not improve (or is not
    /// allowed to replace) the record — e.g. below the chain's eligibility
    /// floor.
    NotUseful,
    /// A fresh attestation was submitted.
    Attested,
    /// The measurement itself could not be taken (no device, kernel error) or
    /// the submission failed.
    Failed,
}

impl AttestOutcome {
    /// Should the caller schedule another attempt?
    fn wants_retry(&self) -> bool {
        matches!(self, AttestOutcome::SkippedBusy(_) | AttestOutcome::Failed)
    }
}

/// Measure this box's real GPU throughput and submit a fresh attestation when
/// the on-chain record is missing, zeroed or stale. Never fatal: a node must
/// still mine and serve if the chain/RPC/GPU probe misbehaves. **Never attests
/// 0** — a failed measurement must not downgrade a good record (item #5).
///
/// Item #93: the measurement is only taken while the GPU is **idle** and is the
/// best of several runs. box1 spent months re-attesting 26/44/45 GFLOPS for a
/// 16 GB card because every attempt ran while the operator's own AI workload
/// held the card; those readings never cleared the eligibility floor, so the
/// bad record could not be replaced and routing kept gating on it.
async fn maybe_refresh_attestation(client: &ChainClient, addr: &str) -> AttestOutcome {
    if std::env::var("GPN_ATTEST_REFRESH")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        println!("[attest] GPN_ATTEST_REFRESH=0 — leaving the on-chain attestation alone");
        return AttestOutcome::Disabled;
    }
    if FLOOR_REJECTED_BY_CHAIN.load(std::sync::atomic::Ordering::Relaxed) {
        println!(
            "[attest] the chain rejected the last measurement with BelowEligibilityFloor — not \
             retrying until this process restarts (the fix is the staged runtime change, not a \
             different number)"
        );
        return AttestOutcome::UpToDate;
    }
    let head = client.head_block_number().await.unwrap_or(0);
    let rec = client
        .get_attestation_meta(addr)
        .await
        .map(|(_gpus, gflops, at)| (gflops, at));
    let refresh = attest_refresh_blocks_from_env();
    // Item #97: a legacy/unit-bug record must not be shielded by the freshness
    // window — that is how `30000` survived on a 15 TFLOPS card for months.
    let peak_gflops = {
        let (_m, tflops) = gpn_agent::gpu::detect_gpu_capability();
        (tflops * 1000.0).round().max(0.0) as u64
    };
    let legacy_record = rec
        .map(|(g, _)| gpn_agent::compute_unit::record_implausible(g, peak_gflops))
        .unwrap_or(false);
    let forced = attest_force_from_env();
    let planned = attest_plan(rec, head, refresh);
    let plan = if forced || (planned == AttestPlan::Skip && legacy_record) {
        AttestPlan::Attest
    } else {
        planned
    };
    match plan {
        AttestPlan::Skip => {
            let (gflops, at) = rec.unwrap_or((0, 0));
            println!(
                "[attest] on-chain attestation is current: {gflops} GFLOPS at block {at} \
                 (head {head}, refresh every {refresh} blocks) — not re-attesting"
            );
            AttestOutcome::UpToDate
        }
        AttestPlan::Attest => {
            let why = match rec {
                None => "no attestation on chain".to_string(),
                Some((0, at)) => format!("on-chain compute_gflops is 0 (written at block {at})"),
                Some((gflops, at)) if forced => format!(
                    "GPN_ATTEST_FORCE=1 — re-attesting by operator request ({gflops} GFLOPS at block {at})"
                ),
                Some((gflops, at)) if legacy_record => format!(
                    "on-chain {gflops} GFLOPS at block {at} is above this card's estimated FP32 peak \
                     ({peak_gflops} GFLOPS) — a legacy/unit-bug record; replacing it with a measurement"
                ),
                Some((gflops, at)) => {
                    format!("{gflops} GFLOPS attested at block {at}, older than {refresh} blocks")
                }
            };
            println!("[attest] re-attesting: {why}");
            let device: usize = std::env::var("GPN_GPU_INDEX")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            // Item #93: an attestation is only ever measured on an **idle** GPU.
            // Blocking (OpenCL + the settle sleep), so it stays off the runtime.
            let policy = gpn_agent::attest_idle::IdlePolicy::from_env();
            let runs = gpn_agent::attest_idle::runs_from_env();
            let settle = gpn_agent::attest_idle::settle_from_env();
            let measured = tokio::task::spawn_blocking(move || {
                let mut probe = gpn_agent::attest_idle::RealProbe::new(device);
                gpn_agent::attest_idle::measure_when_idle(&mut probe, &policy, runs, || {
                    // Let the co-located miner see the yield flag and stand
                    // down before utilisation is read, otherwise our own miner
                    // reads as "someone else's workload" forever.
                    std::thread::sleep(settle);
                })
            })
            .await
            .unwrap_or_else(|e| gpn_agent::attest_idle::IdleMeasurement::Failed {
                reason: format!("measurement task failed: {e}"),
            });
            let (set, load) = match measured {
                gpn_agent::attest_idle::IdleMeasurement::Busy {
                    reason,
                    load,
                    after_settle,
                } => {
                    // This is the line an operator greps for: it is loud, it
                    // names the cause, and it says nothing was submitted.
                    eprintln!(
                        "{}",
                        gpn_agent::attest_idle::busy_summary(&load, &reason, after_settle, settle)
                    );
                    return AttestOutcome::SkippedBusy(reason);
                }
                gpn_agent::attest_idle::IdleMeasurement::Failed { reason } => {
                    eprintln!(
                        "[attest] ⚠ GPU compute measurement failed ({reason}) — not attesting"
                    );
                    return AttestOutcome::Failed;
                }
                gpn_agent::attest_idle::IdleMeasurement::Measured { set, load } => (set, load),
            };
            let gflops = set.chosen();
            if set.unstable {
                println!(
                    "[attest] idle measurement: {} — spread over {:+.0}%, taking the best \
                     ({} GFLOPS); a moving measurement means contention or thermal throttling",
                    set.describe(),
                    gpn_agent::attest_idle::SPREAD_THRESHOLD_PCT,
                    gflops
                );
            } else {
                println!(
                    "[attest] idle measurement: {} — taking {} GFLOPS",
                    set.describe(),
                    gflops
                );
            }
            if set.zero_samples > 0 {
                eprintln!(
                    "[attest] ⚠ {} of {} run(s) returned 0 GFLOPS (failed launch, not a slow \
                     card) and were discarded",
                    set.zero_samples,
                    set.zero_samples + set.samples.len()
                );
            }
            let on_chain = rec.map(|(g, _)| g).filter(|g| *g > 0);
            // Item #104: never pay for a tx the live runtime is certain to
            // reject. The chain's own constant is authoritative here — this
            // binary's compiled floor may be ahead of (or behind) the upgrade
            // that is actually enacted.
            let chain_floor = client.gpu_attestation_floor().await;
            let floor = effective_eligibility_floor(ELIGIBILITY_FLOOR_GFLOPS, chain_floor);
            if gflops < floor {
                let source = match chain_floor {
                    Some(f) if f > ELIGIBILITY_FLOOR_GFLOPS => format!(
                        "the live runtime constant `MinEligibilityGflops = {f}` (this binary ships \
                         {ELIGIBILITY_FLOOR_GFLOPS}; the staged recalibration \
                         `shared/artifacts/project7/staged/gflops-floor-v1.md` is not enacted yet)"
                    ),
                    _ => format!("the eligibility floor {floor}"),
                };
                println!(
                    "[attest] measured {gflops} GFLOPS, below {source} — not submitting (the chain \
                     would reject it with BelowEligibilityFloor and charge for the tx). No number is \
                     fabricated: this clears by itself once the floor is recalibrated and the \
                     provider restarts."
                );
                return AttestOutcome::NotUseful;
            }
            // Item #93(3): never let a wrong record sit there silently.
            if let Some(d) = gpn_agent::attest_idle::record_divergence(
                on_chain,
                gflops,
                gpn_agent::attest_idle::RECORD_TOLERANCE_PCT,
            ) {
                eprintln!("[attest] ⚠ STALE/WRONG RECORD: {d}");
                eprintln!(
                    "[attest] ⚠ routing gates on the on-chain value, so this box is being routed \
                     as a {} GFLOPS card until the record is replaced",
                    d.on_chain
                );
            }
            // Publish the conditions with the number, so `/status` can say
            // "attested-idle" (and an operator can tell it from a stale one).
            let at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut state = gpn_agent::attest_idle::AttestState {
                measured_gflops: gflops,
                samples: set.samples.clone(),
                median_gflops: set.median,
                spread_pct: set.spread_pct,
                conditions: "idle".to_string(),
                util_percent: load.util_percent,
                vram_used_mb: load.vram_used_mb,
                vram_total_mb: load.vram_total_mb,
                runs: set.samples.len() + set.zero_samples,
                at_unix,
                submitted: false,
                on_chain_before: on_chain,
            };
            gpn_agent::attest_idle::write_state(&state);
            // `peak_gflops` (the model-name estimate, computed before the plan)
            // is only used to recognise an impossible record, never to invent a
            // number.
            if !attest_measurement_useful(gflops, on_chain, peak_gflops) {
                println!(
                    "[attest] measured {gflops} GFLOPS vs {} on chain (floor {ELIGIBILITY_FLOOR_GFLOPS}, \
                     estimated FP32 peak {peak_gflops}) — keeping the existing record, not re-attesting",
                    on_chain
                        .map(|g| g.to_string())
                        .unwrap_or_else(|| "none".to_string())
                );
                if gflops < ELIGIBILITY_FLOOR_GFLOPS && on_chain.is_some() {
                    eprintln!(
                        "[attest] ⚠ the chain will refuse a record below {ELIGIBILITY_FLOOR_GFLOPS} \
                         GFLOPS, so the existing {} GFLOPS record cannot be replaced yet. If the \
                         chain still carries the OLD 3000 floor, this is the staged runtime change \
                         `MinEligibilityGflops` (see docs/run-a-node.md \"What compute_gflops actually \
                         measures\"), not a reason to submit a bad number.",
                        on_chain.unwrap_or(0)
                    );
                }
                return AttestOutcome::NotUseful;
            }
            if let Some(rec) = on_chain {
                if gflops <= rec {
                    println!(
                        "[attest] on-chain {rec} GFLOPS exceeds this card's estimated FP32 peak \
                         ({peak_gflops} GFLOPS) — a legacy-unit record; correcting it to the \
                         measured {gflops} GFLOPS"
                    );
                }
            }
            println!(
                "[attest] measured {gflops} GFLOPS on an idle GPU — submitting a fresh attestation"
            );
            let challenge = client
                .get_attestation_challenge()
                .await
                .unwrap_or([0u8; 32]);
            let (model, _tflops) = gpn_agent::gpu::detect_gpu_capability();
            let info = gpn_agent::gpu::GPUInfo {
                model_name: model,
                driver_version: String::new(),
                vram_total_bytes: gpn_agent::gpu::detect_vram_mb() * 1024 * 1024,
                vram_used_mb: load.vram_used_mb.unwrap_or(0),
                temperature_celsius: 0.0,
                gpu_utilization_percent: load.util_percent.unwrap_or(0) as u8,
                current_hashrate_mhs: 0.0,
                compute_capability: String::new(),
                cuda_version: None,
                gpu_count: 1,
            };
            let gpu_hash = gpn_agent::gpu::hash_gpu_info(&info);
            // Bounded: `submit_attestation` waits for inclusion (up to its 300 s
            // timeout). Startup must not hang on a stalled chain.
            let mut floor_rejected = false;
            let submitted = match tokio::time::timeout(
                std::time::Duration::from_secs(90),
                client.submit_attestation(challenge, gpu_hash, 1, gflops),
            )
            .await
            {
                Ok(Ok(())) => {
                    println!("[attest] attested {gflops} GFLOPS on chain");
                    true
                }
                Ok(Err(e)) => {
                    let msg = e.to_string();
                    if msg.contains("BelowEligibilityFloor") {
                        floor_rejected = true;
                        FLOOR_REJECTED_BY_CHAIN.store(true, std::sync::atomic::Ordering::Relaxed);
                        eprintln!(
                            "[attest] the chain rejected {gflops} GFLOPS with BelowEligibilityFloor: the \
                             runtime still carries the OLD floor (3000 GFLOPS, spec-sheet scale). The fix is \
                             the staged `MinEligibilityGflops = 250` runtime change \
                             (shared/artifacts/project7/staged/gflops-floor-v1.md) — it cannot be worked \
                             around off-chain, and submitting a fabricated number is not an option. \
                             This box keeps its current record and is routed to ungated jobs only."
                        );
                    }
                    eprintln!("[attest] submit_attestation failed: {msg}");
                    false
                }
                Err(_) => {
                    eprintln!(
                        "[attest] submit_attestation timed out after 90s (chain stalled?) — \
                         continuing; the claim lane re-reads the chain anyway"
                    );
                    false
                }
            };
            if submitted {
                state.submitted = true;
                gpn_agent::attest_idle::write_state(&state);
                AttestOutcome::Attested
            } else if floor_rejected {
                // Not a retryable failure: only a runtime change can fix it, and
                // `maybe_refresh_attestation` refuses to try again this process.
                AttestOutcome::NotUseful
            } else {
                AttestOutcome::Failed
            }
        }
    }
}

/// Item #61: one WARN per process when the tip cannot be read at all. Repeating
/// it every 500 ms pass would bury the signal (and the log) it is meant to be.
fn warn_chain_unreadable_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "[provider] cannot read NextJobId — chain reads are failing. Skipping claim/\
             reconcile passes (no job claimed, no historical assignment replayed) until \
             the chain answers again."
        );
    }
}

/// Bounded startup wait for a readable `NextJobId`.
async fn wait_for_chain_ready(client: &ChainClient, tries: u32, delay_ms: u64) -> Option<u64> {
    for attempt in 0..tries.max(1) {
        if let Some(tip) = client.try_get_next_job_id().await {
            return Some(tip);
        }
        if attempt + 1 < tries.max(1) {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
    }
    None
}

type ProgressCell = std::sync::Arc<std::sync::Mutex<gpn_agent::provider_watchdog::Progress>>;

// ── Queue #73: deterministic startup adoption of the jobs we already own ─────
//
// The claim/reconcile loops only ever look at the trailing `RECONCILE_WINDOW`
// chain ids, so a job claimed *before* a restart and then left behind by the
// window was never revisited deterministically: `Running` #97 was re-run only
// because a reconcile tick happened to land on it, and `Accepted` #105 (its
// requestor momentarily rate-limited) sat there until the on-chain deadline.
// The assignment store, unlike the chain window, is the durable record of what
// this provider owns — so startup enumerates THAT and decides once, per id.

/// Shape of the off-chain payload we hold for one of our assigned jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadShape {
    /// No local `LlmJob` at all — we can never run it (item #41's shape).
    Missing,
    /// Stored with a finished answer + on-chain commitment: only settlement is
    /// outstanding (the inference already happened before the restart).
    Result,
    /// Stored and still queued/running: inference can be (re)run.
    Queued,
    /// Stored but marked errored (policy refusal, failed attempt).
    Errored,
}

/// What the startup sweep does for one assigned id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adopt {
    /// Terminal, unknown, or another provider's job — leave it alone.
    Skip,
    /// `Pending`: the claim loop owns it, and must wait for create inclusion
    /// before accepting (item #49 — never claim optimistically).
    LeavePending,
    /// `Accepted`/`Running` with a queued payload: run it now.
    ResumeRun,
    /// `Accepted`/`Running` with a stored result: re-submit the result + complete.
    ResumeSettle,
    /// `AwaitingVerification`: the result landed, only completion is outstanding.
    Complete,
    /// Provably unrunnable by this provider: fail it so the escrow refunds now
    /// and the capacity slot frees instead of burning the on-chain deadline.
    Fail(u8),
}

/// `fail_job` reason: the provider never held a payload for this job.
const FAIL_NO_PAYLOAD: u8 = 4;
/// `fail_job` reason: the payload cannot be run here (wrong kind of model, or
/// the per-job attempt cap is already exhausted).
const FAIL_CANNOT_RUN: u8 = 5;

/// Pure adoption decision for one `(chain state, our payload)` pair.
fn adopt_action(
    state: &str,
    provider_is_me: bool,
    kind_is_ours: bool,
    payload: PayloadShape,
    runnable: bool,
) -> Adopt {
    // The diffusion provider shares this signer account, so `provider == me` is
    // true for image/video jobs too. Never touch the other kind's work.
    if !kind_is_ours {
        return Adopt::Skip;
    }
    // `Pending` belongs to the claim loop (item #49). Terminal states need
    // nothing, and an unrecognised state (a newer pallet variant) must never be
    // acted on blindly either.
    match state {
        "Pending" => return Adopt::LeavePending,
        "Accepted" | "Running" | "AwaitingVerification" => {}
        _ => return Adopt::Skip,
    }
    if !provider_is_me {
        return Adopt::Skip;
    }
    if state == "AwaitingVerification" {
        return Adopt::Complete;
    }
    match payload {
        PayloadShape::Missing => Adopt::Fail(FAIL_NO_PAYLOAD),
        PayloadShape::Errored => Adopt::Fail(FAIL_CANNOT_RUN),
        PayloadShape::Result => Adopt::ResumeSettle,
        PayloadShape::Queued if runnable => Adopt::ResumeRun,
        PayloadShape::Queued => Adopt::Fail(FAIL_CANNOT_RUN),
    }
}

/// Is the requested model on this box? `None` = the probe could not answer
/// (ollama down / no tags), which is NOT evidence of absence.
fn model_local_state(model: &str, models: &[String]) -> Option<bool> {
    if model.is_empty() || model == "auto" {
        return None; // nothing concrete requested: anything this box serves
    }
    if models.is_empty() {
        return None;
    }
    Some(gpn_agent::ollama::model_present(models, model))
}

// ---------------------------------------------------------------------------
// Item #75 — per-job latency attribution.
//
// "Claim→start is the long pole" was an inference made from the *outside* (slow
// e2e throughput vs the card's raw tok/s); the provider itself never timed its
// own legs, so nobody could say whether the time went to the AP's create
// inclusion, the claim, the GPU wait, the inference or the settlement. These
// three clocks are cheap (one hash-map entry per in-flight job, removed on
// completion) and every job now prints one line naming the dominant leg.
// ---------------------------------------------------------------------------

/// When we first saw a job's id, when we submitted the claim, and when inference
/// actually started. All optional: a job adopted at startup (#73) has no claim
/// of ours, so those legs are reported as `n/a` rather than as a fake zero.
#[derive(Default, Clone, Copy)]
struct JobClock {
    first_seen: Option<Instant>,
    claimed: Option<Instant>,
    infer_start: Option<Instant>,
}

fn job_clocks() -> &'static Mutex<HashMap<u64, JobClock>> {
    static CLOCKS: OnceLock<Mutex<HashMap<u64, JobClock>>> = OnceLock::new();
    CLOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the first time this process laid eyes on the job id (the assignment
/// arriving is the closest thing the provider has to "the client asked").
fn mark_first_seen(id: u64) {
    if let Ok(mut m) = job_clocks().lock() {
        let e = m.entry(id).or_default();
        if e.first_seen.is_none() {
            e.first_seen = Some(Instant::now());
        }
    }
}

fn mark_claimed(id: u64) {
    if let Ok(mut m) = job_clocks().lock() {
        m.entry(id).or_default().claimed = Some(Instant::now());
    }
}

fn mark_infer_start(id: u64) {
    if let Ok(mut m) = job_clocks().lock() {
        m.entry(id).or_default().infer_start = Some(Instant::now());
    }
}

fn take_clock(id: u64) -> JobClock {
    job_clocks()
        .lock()
        .ok()
        .and_then(|mut m| m.remove(&id))
        .unwrap_or_default()
}

/// `n/a` (unknown), `0.9s` (sub-minute) or `2m03s`. Minutes matter: the create
/// inclusion wait is routinely longer than a minute, and a raw seconds count
/// makes a 57 s wait and a 570 s wait look alike at a glance.
fn fmt_dur(d: Option<Duration>) -> String {
    match d {
        None => "n/a".to_string(),
        Some(d) if d.as_secs() >= 60 => {
            format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
        }
        Some(d) => format!("{:.1}s", d.as_secs_f64()),
    }
}

/// The single line an operator (or the next debugging run) can read instead of
/// re-deriving the latency from four unrelated journal entries.
fn timing_line(
    id: u64,
    clock: JobClock,
    infer: Option<Duration>,
    settle: Option<Duration>,
    now: Instant,
) -> String {
    let assign_to_claim = match (clock.first_seen, clock.claimed) {
        (Some(f), Some(c)) => c.checked_duration_since(f),
        _ => None,
    };
    let claim_to_infer = match (clock.claimed, clock.infer_start) {
        (Some(c), Some(i)) => i.checked_duration_since(c),
        _ => None,
    };
    let claim_to_done = clock.claimed.map(|c| now.saturating_duration_since(c));
    format!(
        "[job {id}] timing: assign\u{2192}claim={} claim\u{2192}infer={} infer={} settle={} claim\u{2192}done={}",
        fmt_dur(assign_to_claim),
        fmt_dur(claim_to_infer),
        fmt_dur(infer),
        fmt_dur(settle),
        fmt_dur(claim_to_done)
    )
}

/// Prints the timing line on EVERY exit path (RAII) — an early `return` or a
/// `?` inside settlement must not hide the job's leg breakdown.
struct JobTiming {
    id: u64,
    started: Instant,
    infer: Option<Duration>,
    settle: Option<Duration>,
}

impl JobTiming {
    fn new(id: u64) -> Self {
        Self {
            id,
            started: Instant::now(),
            infer: None,
            settle: None,
        }
    }
}

impl Drop for JobTiming {
    fn drop(&mut self) {
        // `run=` is the total time this process spent inside `run_job`; it
        // differs from `claim→done` for a job adopted at startup (#73), where
        // the claim happened in a previous process.
        println!(
            "{} run={}",
            timing_line(
                self.id,
                take_clock(self.id),
                self.infer,
                self.settle,
                Instant::now()
            ),
            fmt_dur(Some(self.started.elapsed()))
        );
    }
}

/// Crash recovery for the miner-yield flag (part of item #73).
///
/// A provider killed with SIGKILL never runs `GpuYieldGuard`'s `Drop`, so
/// `~/.gpn/gpu-yield` survives and the co-located miner stays parked forever —
/// the flag has no owner, pid or timestamp, and nothing else ever clears it.
/// The flag is only meaningful while a GPU lock *holder* exists, so prove that:
/// take the lock briefly. If we get it there is no mid-job provider and the
/// leftover flag is stale (our own `Drop` then clears it); if we cannot, another
/// process genuinely owns the GPU and its flag must stay.
fn clear_stale_gpu_yield() {
    let flag = gpn_agent::gpu_yield::yield_flag_path();
    if !flag.exists() {
        return;
    }
    match gpn_agent::gpu_yield::GpuYieldGuard::acquire_with_timeout(Duration::from_millis(1_000)) {
        Ok(guard) => {
            drop(guard);
            println!(
                "[provider] cleared a stale GPU yield flag (no lock holder) — the miner is no \
                 longer parked by a killed process"
            );
        }
        Err(_) => println!(
            "[provider] GPU yield flag present and the GPU lock is held — another provider is \
             mid-job, leaving it alone"
        ),
    }
}

/// Enumerate the ids assigned to us and settle each one deterministically.
///
/// Runs ONCE at startup, before the claim loop takes its first nonce, so it can
/// never race the claim lane (item #31's class). Every id produces one log line
/// naming the state, the payload shape and the action taken — a stranded job
/// must never be silent again.
async fn startup_adopt(
    client: &ChainClient,
    me: &str,
    addr: &str,
    fast: bool,
    progress: &ProgressCell,
) {
    if std::env::var("GPN_STARTUP_ADOPT")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        eprintln!("[reconcile] startup adoption DISABLED (GPN_STARTUP_ADOPT=0)");
        return;
    }
    let mut assigned = read_assigned(me, addr);
    if assigned.is_empty() {
        return; // standalone provider: nothing was ever assigned off-chain
    }
    set_phase(progress, "adopt");
    // No tip ⇒ no evidence about any id, and acting on `None` reads would fail
    // jobs that are perfectly healthy (item #61). Skip; the loops retry later.
    let Some(tip) = client.try_get_next_job_id().await else {
        warn_chain_unreadable_once();
        return;
    };
    assigned.sort_unstable_by_key(|(id, _, _)| *id);
    let models = gpn_agent::nodestatus::ollama_models(&gpn_agent::llm::infer::default_ollama_url());
    let (mut resumed, mut settled, mut completed, mut failed) = (0u32, 0u32, 0u32, 0u32);
    let (mut examined, mut live) = (0u32, 0u32);
    let mut skipped_old = 0u32;
    for (id, _model, _pull) in assigned {
        // Ids at/above the tip were never issued by the chain (an AP label
        // drift): they are not jobs, so nothing to adopt (item #29's rule).
        if id >= tip {
            continue;
        }
        // Bound the sweep: only recent ids, plus anything we still hold a live
        // payload for (see ADOPT_WINDOW).
        let local_status = get_job(id).map(|j| j.status);
        let live_payload = matches!(local_status.as_deref(), Some("queued") | Some("running"));
        if !live_payload && id.saturating_add(ADOPT_WINDOW) < tip {
            skipped_old += 1;
            continue;
        }
        examined += 1;
        let Some((state, _price, _req, provider)) = client.get_job_state(id).await else {
            continue; // not on chain (create still pooled) or unreadable: leave it
        };
        let job = get_job(id);
        let payload = match &job {
            None => PayloadShape::Missing,
            Some(j) if j.status == "done" && j.result_hash.as_deref().and_then(hex32).is_some() => {
                PayloadShape::Result
            }
            Some(j) if j.status == "error" => PayloadShape::Errored,
            Some(_) => PayloadShape::Queued,
        };
        let runnable = job
            .as_ref()
            .map(|j| should_attempt(id, &j.model))
            .unwrap_or(false);
        let action = adopt_action(
            &state,
            provider.as_deref() == Some(me),
            job_kind_is_ours(id),
            payload,
            runnable,
        );
        let action_name = match action {
            Adopt::Skip => {
                continue;
            }
            Adopt::LeavePending => {
                live += 1;
                println!(
                    "[reconcile] startup adopt #{id} state={state}: left to the claim loop \
                     (create inclusion is awaited — no optimistic claim)"
                );
                continue;
            }
            Adopt::ResumeRun => "resume-inference",
            Adopt::ResumeSettle => "resume-settlement",
            Adopt::Complete => "complete",
            Adopt::Fail(_) => "fail",
        };
        live += 1;
        println!(
            "[reconcile] startup adopt #{id} state={state} payload={} model={} action={action_name}",
            match payload {
                PayloadShape::Missing => "none",
                PayloadShape::Result => "result",
                PayloadShape::Queued => "queued",
                PayloadShape::Errored => "errored",
            },
            job.as_ref().map(|j| j.model.as_str()).unwrap_or("-")
        );
        match action {
            Adopt::ResumeRun => {
                let j = job.as_ref().expect("ResumeRun implies a payload");
                // An explicit model this box does not hold is NOT terminal: the
                // background prep worker pulls it (item #56, never blocking) and
                // the ordinary run loop resumes the job once it lands.
                if model_local_state(&j.model, &models) == Some(false) {
                    println!(
                        "[reconcile] startup adopt #{id}: model {} is not installed — queued a \
                         background pull; the run loop will resume it",
                        j.model
                    );
                    let _ = kick_model_prep(&j.model, Some(id));
                    continue;
                }
                match run_job(client, id, j, fast, progress).await {
                    Ok(()) => {
                        resumed += 1;
                        println!("[reconcile] startup adopt #{id}: inference resumed and settled");
                    }
                    Err(e) => eprintln!("[reconcile] startup adopt #{id}: resume failed: {e}"),
                }
            }
            Adopt::ResumeSettle => {
                let j = job.as_ref().expect("ResumeSettle implies a payload");
                let bytes = j.result_hash.as_deref().and_then(hex32).unwrap();
                let toks =
                    ((j.response.as_deref().unwrap_or("").chars().count() as u32) / 4).max(1);
                let sub = if fast {
                    client.submit_result_fast(id, bytes, toks).await
                } else {
                    client.submit_result(id, bytes, toks).await
                };
                match sub {
                    Ok(()) => {
                        let done = if fast {
                            client.complete_job_fast(id).await
                        } else {
                            client.complete_job(id).await
                        };
                        match done {
                            Ok(()) => {
                                settled += 1;
                                println!(
                                    "[reconcile] startup adopt #{id}: result re-submitted and completed"
                                );
                            }
                            Err(e) if is_benign_state_err(&e.to_string()) => settled += 1,
                            Err(e) => {
                                eprintln!("[reconcile] startup adopt #{id}: complete failed: {e}")
                            }
                        }
                    }
                    Err(e) if is_benign_state_err(&e.to_string()) => {}
                    Err(e) => {
                        eprintln!("[reconcile] startup adopt #{id}: submit_result failed: {e}")
                    }
                }
            }
            Adopt::Complete => {
                let res = if fast {
                    client.complete_job_fast(id).await
                } else {
                    client.complete_job(id).await
                };
                match res {
                    Ok(()) => {
                        completed += 1;
                        println!("[reconcile] startup adopt #{id}: completed (slot freed)");
                    }
                    Err(e) if is_benign_state_err(&e.to_string()) => {}
                    Err(e) => eprintln!("[reconcile] startup adopt #{id}: complete failed: {e}"),
                }
            }
            Adopt::Fail(reason) => {
                let res = if fast {
                    client.fail_job_fast(id, reason).await
                } else {
                    client.fail_job(id, reason).await
                };
                match res {
                    Ok(()) => {
                        failed += 1;
                        let why = if reason == FAIL_NO_PAYLOAD {
                            "no off-chain payload (cannot ever run it)"
                        } else {
                            "payload is not runnable here (model/attempts)"
                        };
                        println!(
                            "[reconcile] startup adopt #{id}: failed on-chain (reason {reason}: \
                             {why}) — escrow refunded, slot freed"
                        );
                    }
                    Err(e) if is_benign_state_err(&e.to_string()) => {}
                    Err(e) => eprintln!("[reconcile] startup adopt #{id}: fail_job failed: {e}"),
                }
            }
            Adopt::Skip | Adopt::LeavePending => {}
        }
    }
    // Always say what the sweep saw: "examined 2865 assigned ids, 0 were
    // non-terminal" is the difference between a quiet healthy start and the
    // silent no-op this item was opened for.
    println!(
        "[reconcile] startup adoption: examined {examined} assigned id(s) below tip {tip} \
         ({skipped_old} older than {ADOPT_WINDOW} ids without a live payload, left to the expiry \
         sweep); {live} non-terminal → {resumed} resumed, {settled} settled, {completed} completed, \
         {failed} failed"
    );
    set_phase(progress, "idle");
}

async fn process_once(
    client: &ChainClient,
    me: &str,
    addr: &str,
    fast_claim: bool,
    progress: &ProgressCell,
) -> Result<(), Box<dyn std::error::Error>> {
    set_phase(progress, "pass");
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
        set_phase(progress, "reconcile");
        // Item #61: no tip ⇒ nothing to reconcile *and* no evidence the chain is
        // empty. Skip the pass rather than scanning a bogus `0..0` window.
        let Some(next) = client.try_get_next_job_id().await else {
            warn_chain_unreadable_once();
            return Ok(());
        };
        // Item #49: the reconcile pass used to fail completely silently (a `0`
        // from `get_next_job_id`, or a `None` state for every id) so a stranded
        // job looked like "nothing to do". `GPN_RECONCILE_DEBUG=1` prints one
        // summary line per pass plus a line per candidate id.
        let debug = std::env::var("GPN_RECONCILE_DEBUG")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        let mut scanned = 0u32;
        let mut decoded = 0u32;
        // Account nonce at the start of this pass: the heal gate keys retries on
        // the nonce each attempted tx was signed with. A block being produced is
        // NOT evidence that our tx landed (it may be queued behind other people's
        // extrinsics), but the account nonce only advances once the txs below it
        // are applied. `None` (unreadable) is conservative — it never counts as
        // inclusion.
        let account_nonce = client.account_nonce_at_head(me).await;
        for id in next.saturating_sub(RECONCILE_WINDOW)..next {
            scanned += 1;
            if let Some((state, _price, _req, provider_now)) = client.get_job_state(id).await {
                decoded += 1;
                if debug {
                    println!(
                        "[reconcile] #{id} state={state} provider={} healable={}",
                        provider_now.as_deref().unwrap_or("-"),
                        heal_step_for_state(&state) != HealStep::Wait
                    );
                }
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
                    if heal.is_none() {
                        // Nothing off-chain to heal with — say so when asked, since
                        // this used to be another silent reason a stranded job was
                        // never re-driven (item #49).
                        if debug {
                            println!(
                                "[reconcile] #{id} Pending but no off-chain result in the store \
                                 — nothing to heal"
                            );
                        }
                        continue;
                    }
                    if !heal_attempt_allowed(id, step, account_nonce) {
                        if debug {
                            println!("[reconcile] #{id} accept held by the retry gate");
                        }
                        continue;
                    }
                    {
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
                                if debug {
                                    println!(
                                        "[reconcile] #{id} accept rejected as a benign state race: {e}"
                                    );
                                }
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
                                client
                                    .submit_result_fast_nonce(id, bytes, toks)
                                    .await
                                    .map(Some)
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
        // Item #49: never be silent about a pass that looked at nothing or could
        // not read anything — both mean "we would never heal a stranded job" and
        // used to be indistinguishable from "nothing to do".
        if debug {
            println!("[reconcile] next={next} scanned={scanned} decoded={decoded}");
        }
        if scanned > 0 && decoded == 0 {
            eprintln!(
                "[provider] reconcile: scanned {scanned} id(s) from next={next} but decoded NONE \
                 — chain reads are failing (stranded jobs would not heal)"
            );
        } else if next == 0 {
            // Once per process: a 0 tip is indistinguishable from an empty chain
            // and is exactly how the 15:1x RPC outage hid (silent `0`).
            use std::sync::atomic::{AtomicBool, Ordering};
            static WARNED_ZERO: AtomicBool = AtomicBool::new(false);
            if !WARNED_ZERO.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[provider] reconcile: NextJobId read as 0 — chain read failing? \
                     (stranded jobs would not heal; repeated after every start)"
                );
            }
        }
    } // end reconcile gate

    // ── IDLE gate (the whole point of "go play a game") ──────────────────
    //
    // While the operator idles the node we take **no new job**: we do not poll
    // the AP, do not accept, do not start. Mining is parked by the node itself
    // (service.rs) from the same file flag.
    //
    // Reconcile above already ran, so anything we owned is either finished or
    // healed to a terminal/payable state — we never leave a job wedged. Jobs
    // are claimed and executed inside a single pass, so no work is cut in half
    // here: the pass either claims or it does not.
    if !gpn_agent::idle::claims_allowed() {
        idle_notice_once();
        return Ok(());
    }
    idle_clear_notice();

    // 0. Auto-claim: if an access-point scheduler has assigned us specific jobs,
    //    claim ONLY those; otherwise (standalone) claim any claimable LLM job.
    let assigned = read_assigned(me, addr);
    // finding #34: skip ids we've already settled so the unbounded
    // assignments.json can't drive O(history) chain reads every poll. The set
    // is in-memory here and persisted once per pass (below).
    //
    // Item #29: only trust ids the chain has actually issued (id < NextJobId);
    // a write-off above the tip is a predicted label and must never blacklist a
    // future real job.
    //
    // Item #57: loaded BEFORE the model-prep loop too — a job the chain already
    // finished or cancelled stays in `assignments.json` with a stale `queued`
    // store entry, so the prep kick used to re-fire for it every
    // MODEL_PRESENCE_TTL for as long as the process lived.
    //
    // Item #61: the tip must be READABLE. With an unreadable tip this used to be
    // `0`, the tip filter discarded the whole settled set, and every historical
    // assignment was replayed — so a start during a chain-down window turned
    // completed jobs into permanent "waiting for inclusion" noise.
    set_phase(progress, "claim");
    let settled_path = settled_store_path();
    let chain_tip = match client.try_get_next_job_id().await {
        Some(tip) if chain_tip_readable(Some(tip)) => tip,
        _ => {
            warn_chain_unreadable_once();
            return Ok(());
        }
    };
    let (mut settled, pruned) = read_settled_from_with_tip(&settled_path, chain_tip);
    let targets: Vec<u64> = if assigned.is_empty() {
        // Strict read (#110 family): a failed attestation read used to become
        // `0 GFLOPS`, which silently narrowed the marketplace to
        // `min_gflops = 0` jobs and looked like an idle provider. If the chain
        // cannot be read we do not claim this pass at all.
        let attested = gpn_agent::strict::Strict::from_result(
            "llm_provider.chain_attestation",
            client.get_attestation_record_result(addr).await,
        );
        let attested_gflops = match attested {
            gpn_agent::strict::Strict::Known(Some((_, f, _))) => f,
            // The chain answered: never attested. `min_gflops = 0` only — true.
            gpn_agent::strict::Strict::Known(None) => 0,
            gpn_agent::strict::Strict::Unknown(reason) => {
                eprintln!(
                    "[provider] claim pass skipped: attestation record UNKNOWN ({reason}) — \
                     not claiming at an invented 0 GFLOPS"
                );
                return Ok(());
            }
        };
        client
            .list_claimable_jobs(attested_gflops)
            .await
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    } else {
        // Model-aware: prepare any requested-but-missing model. Item #56: this
        // is now a BACKGROUND kick, never a blocking pull — a cold model used to
        // park the whole claim lane for minutes and starve every other job.
        // Only LLM jobs: ignore image/video jobs the scheduler may have (mis)assigned,
        // otherwise we spam `ollama pull` for foreign models (e.g. sd-turbo) forever.
        // Also skip finished jobs and `auto`/empty requests (nothing concrete to pull).
        for (id, model, pull) in &assigned {
            // Item #57: never prepare a model for a job the chain has already
            // settled (completed/cancelled/expired). Those entries linger in
            // `assignments.json` forever with a `queued` store status, so
            // without this the kick re-fires every MODEL_PRESENCE_TTL.
            if !prep_candidate(*id, chain_tip, &settled) {
                continue;
            }
            let Some(job) = get_job(*id) else { continue };
            if job.kind != "llm" || job.status == "done" || job.status == "error" {
                continue;
            }
            if model.is_empty() || model == "auto" {
                continue;
            }
            let _ = pull;
            // No notice here: the background worker prints one only when a pull
            // actually happens (item #58).
            let _ = kick_model_prep(model, Some(*id));
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
        // Item #75: start this job's clock the first time we consider it, so
        // `assign→claim` measures the create-inclusion wait the #49 gate
        // imposes (that is where the e2e time actually goes).
        mark_first_seen(job_id);
        // Only claim a job the chain actually has in `Pending`. A job that is
        // absent (AP create still in the pool) is WAITED for, not claimed
        // optimistically — an early accept always reverts (item #49) — but the
        // wait is bounded so a never-created id can't be rescanned forever.
        match claim_read_gate(
            client
                .get_job_state(job_id)
                .await
                .as_ref()
                .map(|(state, _, _, _)| state.as_str()),
        ) {
            ClaimRead::Claim => {
                missing_counter().lock().unwrap().remove(&job_id);
            }
            ClaimRead::NotPending => {
                missing_counter().lock().unwrap().remove(&job_id);
                // Conclusively not claimable (Accepted/Running/terminal) —
                // remember it so we never re-query this id again.
                settled.insert(job_id);
                settled_dirty = true;
                continue;
            }
            ClaimRead::NotYetOnChain => {
                // Not on-chain yet — the AP's no-wait `create_job` is still in
                // flight. DO NOT claim optimistically (item #49): `accept_job`
                // requires the job to exist, so a claim dispatched before the
                // create is included *reverts* — it burns 3 extrinsics, the
                // provider never learns (fast path is pool-ack only), the
                // submit_result reverts too, and the job strands `Pending` even
                // though we ran it. Waiting costs only the block time the create
                // needs anyway. Bound the counter so a never-created id isn't
                // re-checked forever.
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
                drop(m);
                if should_announce_wait(job_id) {
                    println!(
                        "[provider] job #{job_id} is not on chain yet (create in flight) — \
                         waiting for inclusion before claiming (no optimistic claim)"
                    );
                }
                continue;
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
        // Item #74: honour an accept backoff before touching the job again, so a
        // transient failure (unfunded requestor / stalled tx) is retried with
        // exponential backoff instead of being dropped or hammered every pass.
        if !accept_retry_gate(job_id) {
            continue;
        }
        println!("[provider] claiming LLM job #{job_id}");
        if fast_claim {
            // accept_job + start_job as one nonce-ordered burst, no block wait.
            match client.claim_job_pipelined(job_id).await {
                Ok(()) => {
                    accept_retry_clear(job_id);
                    // Item #75: the claim is submitted — this is the instant the
                    // `claim→infer` leg is measured from.
                    mark_claimed(job_id);
                    // The claim's own `accept` is now in the pool: stop the
                    // reconcile pass from re-driving it (item #49).
                    heal_mark_attempt(job_id, HealStep::Accept);
                    just_claimed.push(job_id);
                    // One GPU = one job: accept AT MOST ONE per pass; the next job
                    // is only claimed once this one has been run to completion.
                    break;
                }
                Err(e) => {
                    let msg = e.to_string();
                    let plan = accept_retry_record(job_id, &msg);
                    log_accept_failure(job_id, &msg, plan);
                    continue;
                }
            }
        }
        if let Err(e) = client.accept_job(job_id).await {
            let msg = e.to_string();
            let plan = accept_retry_record(job_id, &msg);
            log_accept_failure(job_id, &msg, plan);
            continue;
        }
        accept_retry_clear(job_id);
        mark_claimed(job_id);
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
                if job.kind != "llm" {
                    continue;
                }
                // Item #76: a payload this box can never run is FAILED now, not
                // skipped — a skip leaves the job `Running` until the on-chain
                // deadline with its escrow locked.
                if let Some(why) = unrunnable_reason(&job.model) {
                    fail_unrunnable(client, id, &why, true).await;
                    continue;
                }
                if should_attempt(id, &job.model) {
                    if let Err(e) = run_job(client, id, &job, true, progress).await {
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
                // Item #76: fail what we provably cannot run (kind mismatch) instead
                // of skipping it forever while it burns its on-chain deadline.
                if let Some(why) = unrunnable_reason(&job.model) {
                    fail_unrunnable(client, id, &why, fast_claim).await;
                    continue;
                }
                if !should_attempt(id, &job.model) {
                    // Attempt cap already exhausted (run_job failed it on-chain).
                    continue;
                }
                run_job(client, id, &job, fast_claim, progress).await?;
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
    progress: &ProgressCell,
) -> Result<(), Box<dyn std::error::Error>> {
    set_phase(progress, &format!("run #{id}"));
    // Item #75: the timing report prints on every exit path from here on.
    let mut timing = JobTiming::new(id);
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
    // Take the cross-process GPU lock BEFORE touching the model. This is a
    // real mutex since 2026-10-22: a concurrent image render in
    // gpn_diffusion_provider (same box, different process) now makes us WAIT
    // for the GPU instead of colliding with it in VRAM. The blocking acquire
    // runs off the async runtime so a long wait cannot stall other tasks.
    // While the lock is held the co-located node miner is also told to stop
    // (`~/.gpn/gpu-yield`); item #75 widened that window to the whole job.
    set_phase(progress, "gpu-wait");
    let gpu = match tokio::task::spawn_blocking(gpn_agent::gpu_yield::GpuYieldGuard::acquire).await
    {
        Ok(Ok(g)) => g,
        Ok(Err(e)) => {
            // Timed out waiting: fail the job loudly so the requestor is
            // refunded and the capacity slot frees. Never run concurrently.
            let msg = e.to_string();
            eprintln!("[job {id}] {msg}");
            let _ = LlmStore::new().set_error(id, &msg);
            return_result(id);
            let r = if fast {
                client.fail_job_fast(id, 1).await
            } else {
                client.fail_job(id, 1).await
            };
            match r {
                Ok(()) => println!("[job {id}] failed on-chain — GPU busy, escrow refunded"),
                Err(er) => eprintln!("[job {id}] fail_job (gpu busy) failed: {er}"),
            }
            return Ok(());
        }
        Err(join_err) => {
            let msg = format!("GPU lock task failed: {join_err}");
            eprintln!("[job {id}] {msg}");
            let _ = LlmStore::new().set_error(id, &msg);
            return_result(id);
            return Ok(());
        }
    };
    // Item #75: the lease is held from the claim through COMPLETION, not just
    // for the `run_inference` call — item #24's intent ("mining fully stops for
    // the job's duration") was narrowed to the inference call as a single-
    // authority safety measure, which left the miner resuming between the claim
    // and the first token (and again during settlement).
    //
    // One deliberate exception is kept from #24: on the SLOW path
    // `submit_result`/`complete_job` wait for block inclusion (bounded at
    // 300 s). Parking this box's sealer across that wait would halt the chain if
    // it were the only authority, so the lease is dropped before settlement
    // there. The fast path (`*_fast`) is pool-ack only — no block wait — so the
    // lease is held to the very end.
    let mut gpu_lease = Some(gpu);
    set_phase(progress, "infer");
    mark_infer_start(id);
    let infer_started = Instant::now();
    let result =
        run_inference_with_usage(&model, &job.prompt, job.temperature, job.max_tokens).await;
    timing.infer = Some(infer_started.elapsed());
    set_phase(progress, "settle");
    if !fast {
        gpu_lease = None;
    }
    let settle_started = Instant::now();
    match result {
        Ok((text, hash, usage)) => {
            // Salted commitment: on-chain value = sha256(salt ‖ answer). The salt is
            // random + kept OFF-chain, so the on-chain value is opaque to observers
            // but anyone holding the salt can verify the answer. (A bare
            // sha256(answer) would leak short answers by brute force.)
            let salt = random_salt();
            let commit = salted_commit(&salt, &text);
            let commit_hex = format!("0x{}", hex::encode(commit));
            // Item #53: the backend's real token counts go into the SAME write that
            // marks the job `done`. A separate `set_usage` left a window in which
            // the AP's 300 ms chat poll saw `done` before the counts existed and
            // answered `usage {0,0,0}` (the very symptom #53 removed).
            let _ = set_response_with_usage(
                id,
                &text,
                &commit_hex,
                Some(&salt),
                usage.map(|u| (u.prompt_tokens, u.completion_tokens)),
            );
            // Return the result to the requesting AP (remote providers; item #14).
            return_result(id);
            // On-chain billing tokens: the backend count when it is real (sum of
            // prompt+completion, which is what the requestor is charged for), else
            // the chars/4 heuristic used before.
            let est_tokens: u32 = match usage {
                Some(u) if u.total_tokens > 0 => u.total_tokens,
                _ => ((text.chars().count() as u32) / 4).max(1),
            };
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
    };
    timing.settle = Some(settle_started.elapsed());
    // Release the GPU only now (fast path) — the timing line prints from
    // `timing`'s Drop, i.e. on every path including the `?` returns above.
    drop(gpu_lease);
    Ok(())
}

#[cfg(test)]
mod model_prep_tests {
    use super::{prep_action, pull_happened, PrepAction, MODEL_PREP_STUCK_AFTER};
    use std::time::{Duration, Instant};

    /// Item #56: a cold-model pull must never park the claim lane. The book
    /// allows exactly one worker per model, and a wedged worker stops counting
    /// as in-flight once its window expires so a stuck pull cannot pin a model
    /// forever.
    #[test]
    fn model_prep_starts_once_and_releases_after_the_stuck_window() {
        let now = Instant::now();
        // Nothing in flight => start a worker.
        assert!(matches!(prep_action(None, now), PrepAction::Spawn));
        // Just started => do not stack a second pull for the same model.
        assert!(matches!(
            prep_action(Some(now - Duration::from_secs(1)), now),
            PrepAction::Busy
        ));
        assert!(matches!(
            prep_action(
                Some(now - MODEL_PREP_STUCK_AFTER + Duration::from_secs(1)),
                now
            ),
            PrepAction::Busy
        ));
        // Window elapsed (wedged pull, or a recorded failure) => retry allowed.
        assert!(matches!(
            prep_action(Some(now - MODEL_PREP_STUCK_AFTER), now),
            PrepAction::Spawn
        ));
        assert!(matches!(
            prep_action(Some(now - Duration::from_secs(3600)), now),
            PrepAction::Spawn
        ));
        // A clock that appears to jump backwards must not lock the model out.
        assert!(matches!(
            prep_action(Some(now + Duration::from_secs(60)), now),
            PrepAction::Spawn
        ));
    }

    /// Item #58: the "was not installed — pulled in the background" notice is
    /// emitted only for a real pull. `kick_model_prep` fires on a COLD presence
    /// cache (`None` = unknown), which is not evidence the model is absent, so
    /// announcing a pull there produced a misleading journal line for models
    /// that were already on disk (7 notices / 6 claims, 0 `POST /api/pull`).
    #[test]
    fn only_a_real_pull_announces_a_pull() {
        // What `ensure_model` returns when it found the model locally.
        assert!(!pull_happened("model llama3.2:1b already present"));
        // What it returns after actually pulling.
        assert!(pull_happened("pulled model qwen2.5:7b on demand"));
        // Defensive: an empty/other note is never an announcement.
        assert!(!pull_happened(""));
        assert!(!pull_happened("no specific model requested"));
    }
}

#[cfg(test)]
mod prep_candidate_tests {
    //! Item #57: the model-prep kick must stop re-firing for jobs the chain has
    //! already settled (they linger in `assignments.json` with a stale store
    //! status, so the loop used to retry them every `MODEL_PRESENCE_TTL`).
    use super::prep_candidate;
    use std::collections::HashSet;

    #[test]
    fn settled_jobs_are_not_prepared_again() {
        let mut settled = HashSet::new();
        settled.insert(6253u64);
        settled.insert(6254u64);
        // Cancelled on chain and written off ⇒ never prepared again.
        assert!(!prep_candidate(6253, 6270, &settled));
        // A live, un-settled job is still prepared.
        assert!(prep_candidate(6261, 6270, &settled));
        // An id at/above the tip is a predicted label: a mark against it must
        // not hide a real job that later takes that id (#29).
        assert!(prep_candidate(6253, 6253, &settled));
        assert!(prep_candidate(6253, 6250, &settled));
        // Nothing settled ⇒ everything eligible.
        assert!(prep_candidate(6261, 6270, &HashSet::new()));
    }
}

#[cfg(test)]
mod accept_retry_tests {
    //! Item #74: a failed `accept_job` must be retried with bounded backoff for
    //! transient causes (unfunded requestor escrow, stalled/outbid tx) and must
    //! NOT be retried for state errors — the old code logged and moved on, so the
    //! job sat `Pending` until its deadline while the requestor's escrow paid for
    //! work nobody attempted.
    use super::{
        accept_backoff_ms, accept_retry_plan, classify_accept_error, is_requestor_unfunded,
        AcceptOutcome, ACCEPT_RETRY_MAX,
    };

    #[test]
    fn transient_failures_are_retryable_and_state_errors_are_not() {
        // The item's own case: the requestor cannot fund the accept.
        assert_eq!(
            classify_accept_error("tx included but FAILED dispatch: Token::FundsUnavailable"),
            AcceptOutcome::Retry
        );
        // A tx that never reached a block is worth another try.
        assert_eq!(
            classify_accept_error("tx inclusion timed out after 300s (tx not in a best block)"),
            AcceptOutcome::Retry
        );
        assert_eq!(
            classify_accept_error("tx dropped: The background task closed connection closed"),
            AcceptOutcome::Retry
        );
        // A slot frees when our running job settles.
        assert_eq!(
            classify_accept_error("ComputeMarketplace::MaxProviderCapacity"),
            AcceptOutcome::Retry
        );
        // State answers: re-sending the identical call changes nothing.
        for e in [
            "ComputeMarketplace::JobNotPending",
            "ComputeMarketplace::JobNotFound",
            "ComputeMarketplace::AlreadyAccepted",
            "ComputeMarketplace::NotJobProvider",
            "ComputeMarketplace::ProviderNotRegistered",
            "ComputeMarketplace::ProviderComputeInsufficient",
        ] {
            assert_eq!(classify_accept_error(e), AcceptOutcome::GiveUp, "{e}");
        }
        // `ProviderComputeInsufficient` contains "insufficient" but must not be
        // mistaken for a funding problem.
        assert!(!is_requestor_unfunded(
            "tx included but FAILED dispatch: ComputeMarketplace::ProviderComputeInsufficient"
        ));
        // An error we do not understand is never a retry loop.
        assert_eq!(
            classify_accept_error("some novel failure"),
            AcceptOutcome::GiveUp
        );
    }

    #[test]
    fn retries_are_bounded_and_back_off_exponentially_up_to_the_cap() {
        // 2 s, 4 s, 8 s, 16 s, 32 s then capped at 60 s.
        let delays: Vec<u64> = (0..ACCEPT_RETRY_MAX).map(accept_backoff_ms).collect();
        assert_eq!(delays, vec![2_000, 4_000, 8_000, 16_000, 32_000]);
        assert_eq!(accept_backoff_ms(6), 60_000);
        assert_eq!(accept_backoff_ms(99), 60_000);

        let err = "tx included but FAILED dispatch: Token::FundsUnavailable";
        // First failure schedules retry #1 at 2 s and names the cause.
        let first = accept_retry_plan(0, err).expect("funding failures are retryable");
        assert_eq!(first.attempt, 1);
        assert_eq!(first.delay_ms, 2_000);
        assert!(first.requestor_unfunded);
        // Every attempt in the budget earns the next one...
        for attempts in 0..ACCEPT_RETRY_MAX {
            assert!(
                accept_retry_plan(attempts, err).is_some(),
                "attempt {attempts}"
            );
        }
        // ...and then the budget is spent: no sixth attempt.
        assert!(accept_retry_plan(ACCEPT_RETRY_MAX, err).is_none());
        // A permanent error never earns even the first retry.
        assert!(accept_retry_plan(0, "ComputeMarketplace::JobNotPending").is_none());
    }
}

#[cfg(test)]
mod claim_gate_tests {
    use super::{claim_read_gate, payload_gate, ClaimGate, ClaimRead};

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

    /// Item #49: only a job the chain already holds in `Pending` may be claimed.
    /// An absent id is the AP's `create_job` still in the pool — claiming then
    /// reverts (`JobNotFound`/`JobNotPending`), so it must be waited for, not
    /// claimed optimistically.
    #[test]
    fn claim_waits_for_the_create_to_land_instead_of_reverting() {
        assert_eq!(claim_read_gate(Some("Pending")), ClaimRead::Claim);
        // Absent from chain state (create still pooled) => never claim.
        assert_eq!(claim_read_gate(None), ClaimRead::NotYetOnChain);
        // Any other state is conclusively not claimable.
        for s in [
            "Accepted",
            "Running",
            "AwaitingVerification",
            "Completed",
            "Failed",
            "Cancelled",
            "?",
        ] {
            assert_eq!(claim_read_gate(Some(s)), ClaimRead::NotPending, "state {s}");
        }
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
    fn seed_attempt(id: u64, step: HealStep, age: Duration, nonce: Option<u64>, ineffective: u32) {
        let mut gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
        gate.insert(
            (id, step),
            HealAttempt {
                last: Instant::now() - age,
                nonce,
                ineffective,
            },
        );
    }

    #[test]
    fn heal_gate_admits_one_attempt_per_cooldown() {
        // Unique ids: the gate is process-global and tests run in parallel.
        assert!(heal_attempt_allowed(9_100_001, HealStep::Accept, Some(100)));
        // No nonce evidence yet (nothing submitted): still blocked in the cooldown.
        assert!(!heal_attempt_allowed(
            9_100_001,
            HealStep::Accept,
            Some(101)
        ));
        // A different transition is a different key — it may proceed at once.
        assert!(heal_attempt_allowed(9_100_001, HealStep::Start, Some(101)));
    }

    #[test]
    fn heal_gate_records_the_nonce_it_was_submitted_with() {
        assert!(heal_attempt_allowed(
            9_100_002,
            HealStep::Complete,
            Some(42)
        ));
        heal_record_nonce(9_100_002, HealStep::Complete, 77);
        let gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            gate.get(&(9_100_002, HealStep::Complete))
                .and_then(|a| a.nonce),
            Some(77)
        );
    }

    #[test]
    fn retry_waits_for_our_own_nonce_not_for_the_head() {
        // Cooldown elapsed, but the account nonce still equals the one our tx was
        // signed with ⇒ our tx is queued behind a full block (the dominant case:
        // the head advances every ~11 s, ours does not land). Do NOT burn a new
        // nonce on a duplicate. This is what made the head-based #33 gate a no-op.
        seed_attempt(
            9_100_003,
            HealStep::Start,
            Duration::from_secs(51),
            Some(100),
            0,
        );
        assert!(!heal_attempt_allowed(9_100_003, HealStep::Start, Some(100)));
        // Account nonce passed ours ⇒ the tx was applied (real inclusion evidence).
        seed_attempt(
            9_100_004,
            HealStep::Start,
            Duration::from_secs(51),
            Some(100),
            0,
        );
        assert!(heal_attempt_allowed(9_100_004, HealStep::Start, Some(101)));
        // Unreadable account nonce is conservative: never evidence.
        seed_attempt(
            9_100_005,
            HealStep::Start,
            Duration::from_secs(51),
            Some(100),
            0,
        );
        assert!(!heal_attempt_allowed(9_100_005, HealStep::Start, None));
        // Absolute floor still guarantees liveness for a genuinely dropped tx.
        seed_attempt(
            9_100_006,
            HealStep::Start,
            Duration::from_secs(121),
            Some(100),
            0,
        );
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
        seed_attempt(
            9_100_007,
            HealStep::Accept,
            Duration::from_secs(51),
            Some(100),
            1,
        );
        assert!(!heal_attempt_allowed(
            9_100_007,
            HealStep::Accept,
            Some(101)
        ));
        // After the doubled backoff it retries, and the counter escalates.
        seed_attempt(
            9_100_008,
            HealStep::Accept,
            Duration::from_secs(91),
            Some(100),
            1,
        );
        assert!(heal_attempt_allowed(9_100_008, HealStep::Accept, Some(101)));
        {
            let gate = heal_gate().lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(
                gate.get(&(9_100_008, HealStep::Accept))
                    .map(|a| a.ineffective),
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
        assert_eq!(
            heal_step_for_state("AwaitingVerification"),
            HealStep::Complete
        );
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
        assert!(
            dropped,
            "pruning must mark the cache dirty so the file shrinks"
        );
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

    #[test]
    fn an_unreadable_tip_never_passes_for_an_empty_chain() {
        // Item #61 regression. `get_next_job_id()` answers 0 for every failure, so
        // a start during a chain-down window looked like a genesis chain: the
        // settled tip filter kept nothing and old assignments were replayed.
        assert!(!chain_tip_readable(None), "a failed read is not evidence");
        assert!(
            chain_tip_readable(Some(0)),
            "a brand-new chain legitimately reads 0"
        );
        assert!(chain_tip_readable(Some(6_400)));
    }

    #[test]
    fn startup_ready_budget_is_bounded_and_typofriendly() {
        // Defaults: 20 x 1 s — long enough for a co-started node, short enough
        // that a genuinely dead chain does not hold serving hostage.
        assert_eq!(startup_ready_budget(None, None), (20, 1_000));
        // Garbage/absurd values are clamped, never a zero-attempt "wait".
        assert_eq!(startup_ready_budget(Some(0), Some(0)), (1, 50));
        assert_eq!(
            startup_ready_budget(Some(10_000), Some(10_000_000)),
            (600, 5_000)
        );
        // Honest values pass through untouched.
        assert_eq!(startup_ready_budget(Some(5), Some(250)), (5, 250));
    }

    // ── Queue #65: the node keeps its own attestation real ──────────────

    #[test]
    fn attestation_is_refreshed_only_when_missing_zeroed_or_stale() {
        // Never attested (box1's genesis case for a fresh box).
        assert_eq!(attest_plan(None, 175_000, 7_200), AttestPlan::Attest);
        // A zeroed record is a measurement that failed somewhere — repair it
        // even if it was written a minute ago (item #5's whole point).
        assert_eq!(
            attest_plan(Some((0, 175_000)), 175_001, 7_200),
            AttestPlan::Attest
        );
        // Current record: re-attesting would burn a tx and a GEMM run for
        // nothing. This is the steady state — startups must be quiet.
        assert_eq!(
            attest_plan(Some((30_000, 170_000)), 175_000, 7_200),
            AttestPlan::Skip
        );
        // At exactly the refresh age, still current (strictly older re-attests).
        assert_eq!(
            attest_plan(Some((30_000, 167_800)), 175_000, 7_200),
            AttestPlan::Skip
        );
        // One block past it: stale.
        assert_eq!(
            attest_plan(Some((30_000, 167_799)), 175_000, 7_200),
            AttestPlan::Attest
        );
        // box1's live record: 188 GFLOPS written at block 51 on a 175k chain.
        assert_eq!(
            attest_plan(Some((188, 51)), 175_000, 7_200),
            AttestPlan::Attest
        );
        // Unreadable head (0) is NOT evidence of staleness — no spurious tx on
        // a cold start, and a record dated in the future never "ages".
        assert_eq!(attest_plan(Some((30_000, 51)), 0, 7_200), AttestPlan::Skip);
        assert_eq!(
            attest_plan(Some((30_000, 999_999)), 175_000, 7_200),
            AttestPlan::Skip
        );
    }

    #[test]
    fn a_measurement_only_ever_improves_the_on_chain_record() {
        // 15.1 TFLOPS card → 15 100 GFLOPS estimated peak; it measures ~823 in
        // the canonical unit (item #97).
        const PEAK: u64 = 15_100;
        // Below the eligibility floor (250 in the MEASURED unit): the chain
        // rejects it (`BelowEligibilityFloor`), so never pay for that tx.
        assert!(!attest_measurement_useful(0, None, PEAK));
        assert!(!attest_measurement_useful(249, None, PEAK));
        assert!(!attest_measurement_useful(249, Some(188), PEAK));
        // At the floor it is eligible...
        assert!(attest_measurement_useful(250, None, PEAK));
        // ...but a *plausible* record is never downgraded by a weaker reading.
        assert!(!attest_measurement_useful(250, Some(800), PEAK));
        assert!(!attest_measurement_useful(800, Some(800), PEAK));
        // box1: an old contended 188 record, and a real idle measurement of the
        // 5070 Ti (~2 400 estimated) beats it.
        assert!(attest_measurement_useful(2_400, Some(188), 44_000));
        // A zeroed record is always repaired by any eligible reading.
        assert!(attest_measurement_useful(250, Some(0), PEAK));
        // A record ABOVE the card's FP32 peak is a legacy-unit artefact (this
        // host: 30 000 on a 15.1 TFLOPS card) and an honest measurement may
        // replace it — otherwise the bad record is permanent by construction.
        assert!(attest_measurement_useful(823, Some(30_000), PEAK));
        // Just-inside-plausible records keep the never-downgrade protection
        // (15 100 × 1.5 = 22 650).
        assert!(!attest_measurement_useful(823, Some(22_000), PEAK));
        assert!(attest_measurement_useful(823, Some(22_700), PEAK));
        // Unknown device (peak 0) → never attempt a downward correction.
        assert!(!attest_measurement_useful(823, Some(30_000), 0));
    }

    /// Item #97 migration: a legacy record is **replaced**, never reinterpreted.
    /// 30 000 is not "30 000 in peak units ⇒ 1 650 measured" — it is garbage, and
    /// the measured value simply overwrites it. This test pins that decision.
    #[test]
    fn a_legacy_record_is_replaced_by_a_measurement_not_converted() {
        const PEAK: u64 = 15_100;
        assert!(attest_measurement_useful(823, Some(30_000), PEAK));
        // The same measurement against the *converted* interpretation would be
        // a different number and must NOT be what gets submitted.
        assert_ne!(
            823,
            (30_000.0 * gpn_agent::compute_unit::MEASURED_FRACTION_OF_PEAK) as u64
        );
        // An already-correct record is left alone (no tx churn).
        assert!(!attest_measurement_useful(823, Some(825), PEAK));
    }

    /// A *low-end but honest* measurement must be accepted once the floor is in
    /// the measured unit (item #97). A 2.9 TFLOPS-peak card measures ~160 and
    /// still fails; the weakest supported 8 GB card measures ~340+ and passes.
    #[test]
    fn an_honest_measurement_at_the_recalibrated_floor_is_accepted() {
        assert_eq!(ELIGIBILITY_FLOOR_GFLOPS, 250);
        assert!(!attest_measurement_useful(249, None, 0));
        assert!(attest_measurement_useful(250, None, 0));
        assert!(attest_measurement_useful(340, None, 6_500));
        // The reference card's real reading is comfortably inside.
        assert!(attest_measurement_useful(823, None, 15_100));
    }

    /// Item #104: a submission must clear the floor the **live chain** enforces,
    /// not the one this binary was compiled with. Pinned by the live journal:
    /// `[attest] submit_attestation failed: … BelowEligibilityFloor` at every
    /// restart, paying a fee for a tx that could never land.
    #[test]
    fn a_submission_must_clear_the_live_chain_floor_not_the_compiled_one() {
        // The exact live case: 824 measured, chain still carries 3000.
        let floor = effective_eligibility_floor(250, Some(3_000));
        assert_eq!(floor, 3_000);
        assert!(
            824 < floor,
            "824 GFLOPS must not be submitted under a 3000 floor"
        );
        // Once the staged recalibration enacts, the same measurement submits.
        assert_eq!(effective_eligibility_floor(250, Some(250)), 250);
        assert!(824 >= effective_eligibility_floor(250, Some(250)));
        // An unreadable constant is not evidence — fail open, and never let an
        // implausibly low chain value lower our own canonical floor either.
        assert_eq!(effective_eligibility_floor(250, None), 250);
        assert_eq!(effective_eligibility_floor(250, Some(50)), 250);
        assert_eq!(effective_eligibility_floor(250, Some(0)), 250);
    }
}

// ── Queue #73: startup adoption of jobs we already own ──────────────────────

#[cfg(test)]
mod startup_adoption_tests {
    use super::*;

    /// Item #76: the provider's "can I run this?" answer must be the SHARED
    /// classifier, so a diffusion model is recognised as unrunnable instead of
    /// being claimed, skipped silently and left `Running` until the deadline.
    #[test]
    fn a_generation_model_is_never_runnable_on_an_llm_box() {
        for m in [
            "sdxl-turbo",
            "sd-turbo",
            "flux-dev",
            "ltx-video",
            "playground-v2",
        ] {
            assert!(
                unrunnable_reason(m).is_some(),
                "{m} must be recognised as a generation model"
            );
        }
        // Real chat/ML tags stay runnable (the resolve_model fallback owns cold pulls).
        for m in ["llama3.2:1b", "qwen2.5:7b", "resnet50", "auto", ""] {
            assert!(unrunnable_reason(m).is_none(), "{m} must stay runnable");
        }
        // should_attempt keeps both halves of its old contract.
        assert!(should_attempt(u64::MAX, "llama3.2:1b"));
        assert!(!should_attempt(u64::MAX, "sdxl-turbo"));
    }

    #[test]
    fn adoption_resumes_or_settles_stale_jobs_instead_of_leaving_them() {
        // The two live symptoms the item was opened for. #97 was `Running` with
        // the payload still queued (the restart killed inference) ⇒ re-run it.
        assert_eq!(
            adopt_action("Running", true, true, PayloadShape::Queued, true),
            Adopt::ResumeRun
        );
        // #105 was `Accepted` with a queued payload ⇒ re-run it too (the old
        // code only ever started+completed result-less zombies, refunding work
        // it could still have done).
        assert_eq!(
            adopt_action("Accepted", true, true, PayloadShape::Queued, true),
            Adopt::ResumeRun
        );
        // Inference finished before the restart, only settlement is missing.
        assert_eq!(
            adopt_action("Running", true, true, PayloadShape::Result, false),
            Adopt::ResumeSettle
        );
        // Result already on chain: completion alone frees the capacity slot.
        assert_eq!(
            adopt_action(
                "AwaitingVerification",
                true,
                true,
                PayloadShape::Result,
                false
            ),
            Adopt::Complete
        );
    }

    #[test]
    fn adoption_fails_only_what_it_can_never_run() {
        // Item #41's shape: accepted/running with no off-chain payload at all.
        assert_eq!(
            adopt_action("Running", true, true, PayloadShape::Missing, false),
            Adopt::Fail(FAIL_NO_PAYLOAD)
        );
        // A payload the operator cannot run (foreign model, attempt cap hit).
        assert_eq!(
            adopt_action("Running", true, true, PayloadShape::Queued, false),
            Adopt::Fail(FAIL_CANNOT_RUN)
        );
        // Already errored off-chain (policy refusal / failed attempts).
        assert_eq!(
            adopt_action("Accepted", true, true, PayloadShape::Errored, true),
            Adopt::Fail(FAIL_CANNOT_RUN)
        );
    }

    #[test]
    fn adoption_never_claims_pending_or_touches_foreign_work() {
        // Item #49: `Pending` belongs to the claim loop, which waits for the
        // create to be included. Adopting it here would accept optimistically.
        assert_eq!(
            adopt_action("Pending", true, true, PayloadShape::Queued, true),
            Adopt::LeavePending
        );
        // Same signer, other kind (image/video): not our job.
        assert_eq!(
            adopt_action("Running", true, false, PayloadShape::Missing, false),
            Adopt::Skip
        );
        // Accepted by a different provider — never fail someone else's job.
        assert_eq!(
            adopt_action("Running", false, true, PayloadShape::Missing, false),
            Adopt::Skip
        );
        // Terminal and unknown states are left alone.
        for s in ["Completed", "Failed", "Cancelled", "?}"] {
            assert_eq!(
                adopt_action(s, true, true, PayloadShape::Queued, true),
                Adopt::Skip,
                "state {s} must not be adopted"
            );
        }
    }

    #[test]
    fn a_model_probe_that_cannot_answer_is_not_evidence_of_absence() {
        let have = vec!["llama3.2:1b".to_string()];
        assert_eq!(model_local_state("llama3.2:1b", &have), Some(true));
        assert_eq!(model_local_state("qwen2.5:7b", &have), Some(false));
        // ollama down / no tags ⇒ unknown, which must never fail a job.
        assert_eq!(model_local_state("qwen2.5:7b", &[]), None);
        // `auto`/empty requests are always servable here.
        assert_eq!(model_local_state("auto", &have), None);
        assert_eq!(model_local_state("", &have), None);
    }
}

#[cfg(test)]
mod job_timing_tests {
    use super::{fmt_dur, mark_claimed, mark_first_seen, take_clock, timing_line, JobClock};
    use std::time::{Duration, Instant};

    /// Item #75: the point of the instrumentation is that ONE line says which leg
    /// consumed the wall clock. The live shape this was written for: the AP's
    /// create takes ~1 min to be included, the claim is instant, inference is a
    /// few seconds — i.e. `assign→claim` dominates and `claim→infer` does not,
    /// which is exactly the opposite of the pre-fix assumption.
    #[test]
    fn timing_line_names_the_leg_that_consumed_the_wall_clock() {
        let now = Instant::now();
        let clock = JobClock {
            first_seen: now.checked_sub(Duration::from_secs(59)),
            claimed: now.checked_sub(Duration::from_secs(2)),
            infer_start: now.checked_sub(Duration::from_millis(1700)),
        };
        let line = timing_line(
            6505,
            clock,
            Some(Duration::from_millis(1500)),
            Some(Duration::from_millis(200)),
            now,
        );
        assert!(line.starts_with("[job 6505] timing:"), "{line}");
        assert!(line.contains("assign\u{2192}claim=57.0s"), "{line}");
        assert!(line.contains("claim\u{2192}infer=0.3s"), "{line}");
        assert!(line.contains("infer=1.5s"), "{line}");
        assert!(line.contains("settle=0.2s"), "{line}");
        assert!(line.contains("claim\u{2192}done=2.0s"), "{line}");
    }

    /// A job adopted at startup (#73) was claimed by a *previous* process, so the
    /// provider genuinely does not know the earlier legs — `n/a`, never a fake
    /// zero that would flatter the routing path.
    #[test]
    fn a_resumed_job_reports_unknown_legs_instead_of_zeros() {
        let now = Instant::now();
        let line = timing_line(
            6503,
            JobClock::default(),
            Some(Duration::from_secs(3)),
            None,
            now,
        );
        assert!(line.contains("assign\u{2192}claim=n/a"), "{line}");
        assert!(line.contains("claim\u{2192}infer=n/a"), "{line}");
        assert!(line.contains("claim\u{2192}done=n/a"), "{line}");
        assert!(line.contains("infer=3.0s"), "{line}");
    }

    #[test]
    fn durations_read_as_seconds_below_a_minute_and_minutes_above_it() {
        assert_eq!(fmt_dur(None), "n/a");
        assert_eq!(fmt_dur(Some(Duration::from_millis(0))), "0.0s");
        assert_eq!(fmt_dur(Some(Duration::from_millis(950))), "0.9s");
        assert_eq!(fmt_dur(Some(Duration::from_secs(59))), "59.0s");
        // 57 s and 570 s must not look alike at a glance.
        assert_eq!(fmt_dur(Some(Duration::from_secs(60))), "1m00s");
        assert_eq!(fmt_dur(Some(Duration::from_secs(570))), "9m30s");
    }

    /// The static clock map is keyed by job id and must not leak: the entry is
    /// removed when the job reports, and a re-claim starts a fresh clock.
    #[test]
    fn a_job_clock_is_recorded_once_and_released_on_report() {
        mark_first_seen(9_001);
        mark_first_seen(9_001);
        let first = take_clock(9_001);
        let seen = first.first_seen.expect("first sighting recorded");
        assert!(first.claimed.is_none(), "no claim yet");
        // Released: a second report for the same id has nothing to say.
        let second = take_clock(9_001);
        assert!(second.first_seen.is_none() && second.claimed.is_none());

        mark_first_seen(9_002);
        mark_claimed(9_002);
        let claimed = take_clock(9_002);
        assert!(claimed.claimed >= claimed.first_seen, "seen before claim");
        // `seen` is only used to prove the first clock really recorded a time.
        let _ = seen;
    }
}

#[cfg(test)]
mod item101_panic_isolation {
    use super::with_panic_guard;
    use std::cell::Cell;

    /// The exact regression: before item #101 the handler ran bare on the ONE
    /// thread that owns the listening port, so a panic propagated out of the
    /// request loop and the lane was dead for the rest of the process's life
    /// (port unbound, every later chat silently on-chain, unit still `active`).
    #[test]
    fn a_panicking_request_is_contained_and_the_lane_keeps_serving() {
        let ran_after = Cell::new(0);
        let ok = with_panic_guard("request handler", || {
            panic!("simulated handler panic (item #101)");
        });
        assert!(!ok, "a panic must be reported as not-completed");
        // The caller keeps looping: nothing was unwound into it.
        for _ in 0..3 {
            let served = with_panic_guard("request handler", || {
                ran_after.set(ran_after.get() + 1);
            });
            assert!(served, "a healthy request still reports completion");
        }
        assert_eq!(ran_after.get(), 3, "the lane served every later request");
    }

    #[test]
    fn a_healthy_request_reports_completion() {
        let hit = Cell::new(false);
        assert!(with_panic_guard("request handler", || hit.set(true)));
        assert!(hit.get());
    }
}
