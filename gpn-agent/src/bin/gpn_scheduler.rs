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
use gpn_agent::hub_channel::GenParams;
use gpn_agent::image_jobs::{self, ImageArtifact, ImageJob};
use gpn_agent::jobkind::{admit, model_kind, JobKind};
use gpn_agent::pricing::{self, params_b_for, roofline_tok_per_s, PricingConfig};
use gpn_agent::routing::{self, RttOverrides};
use serde_json::{json, Value};
use std::collections::HashMap;
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
    recent_jobs()
        .lock()
        .unwrap()
        .push((id, Instant::now(), max_tokens));
}

/// When each job's lease was last handed to a provider by `GET /assignments`.
///
/// Item #91 (janitor vs claim race): the janitor decides "stale" from the
/// **on-chain** state plus the job's create age, but the `accept`/`start` txs
/// that move a job off `Pending` queue behind block time (60–120 s per
/// transition on this chain). A job that was just delivered to a provider is
/// therefore indistinguishable from an abandoned one, so the AP's own
/// `cancel_job` can win the race against the provider's `accept`: the start then
/// fails `JobNotPending`, the provider's work has no on-chain job, and the poll
/// reports `succeeded` while the chain disagrees (observed 2026-10-23, #6528).
/// Delivery is the earliest claim signal an access point has, so it arms a short
/// veto on the staleness clock.
fn lease_delivered() -> &'static Mutex<HashMap<u64, Instant>> {
    static L: OnceLock<Mutex<HashMap<u64, Instant>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How long a delivered lease keeps the janitor's cancel suspended: one
/// `accept` plus one `start` inclusion, with slack for a slow block. A provider
/// that wins the claim needs both to land before the chain stops reading
/// `Pending`; a genuinely dead provider is still reclaimed by the on-chain
/// deadline (and by the fast path once this window lapses).
const CLAIM_IN_FLIGHT_SECS: u64 = 240;

/// Stamps older than this are dropped: they can no longer veto anything, and the
/// map must not grow with the job history.
const LEASE_STAMP_RETENTION_SECS: u64 = 3600;

/// Record that job `id`'s lease was handed to a provider at `now`.
fn note_lease_delivered(id: u64, now: Instant) {
    let mut map = lease_delivered().lock().unwrap();
    map.retain(|_, t| now.duration_since(*t).as_secs() < LEASE_STAMP_RETENTION_SECS);
    map.insert(id, now);
}

/// Seconds since this job's lease was last delivered to a provider.
fn lease_delivered_secs(id: u64, now: Instant) -> Option<u64> {
    let map = lease_delivered().lock().unwrap();
    map.get(&id).map(|t| now.duration_since(*t).as_secs())
}

/// Is a claim for this job plausibly in flight (its lease was delivered within
/// [`CLAIM_IN_FLIGHT_SECS`])?
fn claim_in_flight_now(id: u64, now: Instant) -> bool {
    claim_in_flight(lease_delivered_secs(id, now))
}

/// Ids already reported as claim-in-flight, so the 30 s janitor pass cannot
/// repeat the same explanation for the same job.
fn claim_veto_logged() -> &'static Mutex<Vec<u64>> {
    static V: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(Vec::new()))
}

/// Explain one vetoed cancel, once per job.
fn note_claim_veto(id: u64, age_secs: u64, delivered_secs: u64) {
    let mut seen = claim_veto_logged().lock().unwrap();
    if seen.contains(&id) {
        return;
    }
    seen.push(id);
    println!(
        "[janitor] #{id} has been Pending {age_secs}s, but its lease was handed to a \
         provider {delivered_secs}s ago — a claim is in flight; not cancelling"
    );
}

/// Pure half of [`claim_in_flight_now`]: `Some(secs since delivery)`.
/// No stamp at all is not a claim.
fn claim_in_flight(delivered_secs: Option<u64>) -> bool {
    matches!(delivered_secs, Some(s) if s < CLAIM_IN_FLIGHT_SECS)
}

/// Attempts for the off-chain payload write before we give up on a job.
const PAYLOAD_PUT_ATTEMPTS: u32 = 3;
/// Small pause between payload-write attempts (the store lock is held for
/// milliseconds, so a contended write usually succeeds on the retry).
const PAYLOAD_PUT_BACKOFF_MS: u64 = 50;

/// Persist a job's off-chain payload, **verifying** the write (item #41
/// residual). Every previous call site discarded the `Result` (`let _ = put(..)`),
/// so a failed write left an on-chain job with escrow locked and no payload:
/// no provider can ever run it (#41 skips payload-less jobs on purpose), so it
/// wedges until the on-chain deadline refunds it and the caller waits out a
/// timeout. A job is therefore only advertised (lease written) once its payload
/// is durable; a write that survives `PAYLOAD_PUT_ATTEMPTS` fails loudly instead.
fn persist_job_payload(
    store: &gpn_agent::llm::LlmStore,
    id: u64,
    job: &gpn_agent::llm::LlmJob,
) -> bool {
    for attempt in 1..=PAYLOAD_PUT_ATTEMPTS {
        match store.put(id, job.clone()) {
            Ok(()) => return true,
            Err(e) => eprintln!(
                "[store] payload write for job #{id} failed (attempt {attempt}/{PAYLOAD_PUT_ATTEMPTS}): {e}"
            ),
        }
        if attempt < PAYLOAD_PUT_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(PAYLOAD_PUT_BACKOFF_MS));
        }
    }
    false
}

/// OpenAI-shaped `usage` block for a completed chat job (item #53).
///
/// The counts come from the inference backend (via the provider's store). An
/// unknown count is reported as **0**, never invented — a client that meters or
/// bills from `usage` must be able to tell "not reported" from a real number,
/// and the pre-fix hard-coded zeros were indistinguishable from a genuine 0.
fn usage_json(usage: Option<(u32, u32)>) -> serde_json::Value {
    let (prompt_tokens, completion_tokens) = usage.unwrap_or((0, 0));
    json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens.saturating_add(completion_tokens)
    })
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

/// How many of the most recent job ids `GET /jobs` enumerates. The handler does
/// one chain RPC (`get_job_state`) per id, so the old `0..next_id` scan was O(N)
/// and, once the chain passed ~3.6k jobs, wedged the endpoint (HTTP000 after
/// 40s). Monitoring only ever needs the trailing window (orphans are recent);
/// per-id lookups (`GET /job/{id}`) cover anything older.
const JOBS_LIST_WINDOW: u64 = 256;
fn stale_secs(max_tokens: u64) -> u64 {
    let gen = (max_tokens / JANITOR_TOK_PER_S.max(1)) * JANITOR_SLACK;
    (JANITOR_BASE_SECS + gen).clamp(JANITOR_MIN_SECS, JANITOR_MAX_SECS)
}

/// Stale floor for job kinds whose **on-chain** `Pending` window is not a proxy
/// for a stranded job.
///
/// The token-scaled window above assumes the only latency is inference, but a
/// fine-tune, a render or a video takes minutes of *provider* work, and the
/// accept/start txs that move the job off `Pending` queue behind block time
/// (60–120 s per transition on this chain). With `max_tokens == 0` — which every
/// non-LLM payload has — a long job therefore looked "abandoned" long before any
/// provider could possibly have started it.
/// Observed 2026-10-23: fine-tune #6528 was pulled by the provider at 12:30:48
/// and was actively tuning at 12:34:35, but the janitor had already issued a
/// requestor `cancel_job` at 12:33:14 ("waited >120s", `max_tokens 0`); the
/// provider's `start` then failed `JobNotPending` — a guaranteed-wasted dispatch
/// that on a slightly slower block would have thrown away the finished adapter.
fn kind_stale_floor(kind: &str) -> u64 {
    match kind {
        // LoRA/QLoRA fine-tune + adapter export + model registration.
        "finetune" => 1800,
        // Diffusion/video renders, including a cold model pull.
        "image" | "video" | "gen" => 900,
        // Vector/raw-completion jobs are usually fast but still ollama-bound.
        "ml" => 300,
        // LLM/chat keeps the token-scaled window.
        _ => 0,
    }
}

/// The effective stale window for a tracked job: token-scaled, but never shorter
/// than the floor its kind needs to legitimately leave `Pending`.
fn stale_secs_for_kind(kind: &str, max_tokens: u64) -> u64 {
    stale_secs(max_tokens).max(kind_stale_floor(kind))
}

/// Terminal job states — a job here no longer holds an `ActiveJobCount` slot,
/// so the janitor stops tracking it.
fn is_terminal_state(state: &str) -> bool {
    matches!(state, "Completed" | "Cancelled" | "Failed")
}

/// Extra slack added to a job's token-scaled stale window before an *unreadable*
/// chain record is treated as "the job is gone".
///
/// A missing `ComputeMarketplace::Jobs[id]` is ambiguous: the no-wait fast create
/// path is pool-ack only, so a job we just created has no storage item for a
/// block or more, and a transient RPC failure looks identical. Treating that as
/// "gone" dropped the tracking entry and lost its `max_tokens`, after which the
/// adopt scan re-added it with `0` — collapsing the token-scaled window to the
/// 120 s floor and exposing a large legitimate job to a premature self-cancel.
const JANITOR_ABSENT_GRACE_SECS: u64 = 120;

/// Should a tracked job survive a reconcile pass in which its on-chain record
/// could not be read? Yes until it has aged past its own stale window plus
/// [`JANITOR_ABSENT_GRACE_SECS`] — a create that never lands still ages out
/// here and frees the slot, so nothing leaks.
fn survives_absent_read(age_secs: u64, max_tokens: u64) -> bool {
    age_secs <= stale_secs(max_tokens).saturating_add(JANITOR_ABSENT_GRACE_SECS)
}

/// Does this job's off-chain record already carry a provider result?
///
/// "Work was delivered" is strictly stronger evidence than any age heuristic.
/// A job whose result the AP already stored must never be requestor-cancelled:
/// the only outcomes are losing the provider's payment for finished work or
/// burning a tx the chain rejects as non-`Pending`. Observed 2026-10-18: #5534 was
/// claimed and answered at 18:58 but its accept/start txs sat in the pool behind
/// ~61 s blocks, so at 19:00 it still read `Pending` on-chain and the janitor
/// cancelled a fully served job.
fn job_has_result(status: &str) -> bool {
    matches!(status, "done" | "error")
}

/// Should the janitor issue a requestor `cancel_job` for this tracked job?
///
/// Only for our own still-`Pending` job past its token-scaled stale window, and
/// **never** once the provider's result is already in hand — that would cancel
/// delivered work (observed 2026-10-18 on #5534: answered at 18:58, still read
/// `Pending` on-chain at 19:00 because the accept/start txs were queued behind
/// ~61 s blocks).
/// `claim_in_flight` vetoes the decision outright: a lease handed to a provider
/// within [`CLAIM_IN_FLIGHT_SECS`] means the `accept`/`start` that moves the job
/// off `Pending` may still be in the pool, and cancelling would strand the
/// provider's work (item #91).
fn should_cancel_stale(
    state: &str,
    age_secs: u64,
    stale_for_job: u64,
    delivered: bool,
    claim_in_flight: bool,
) -> bool {
    state == "Pending" && !delivered && !claim_in_flight && age_secs > stale_for_job
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

/// How many trailing job ids the janitor re-scans on **every** pass to adopt
/// orphans it never tracked. Needed because the startup seed above reads
/// `NextJobId` at process start: a `create_job` submitted by the previous
/// process (no-wait fast path) can still be in the tx pool then, land a block
/// later, and thus be *newer* than the seeded counter — invisible to the seed,
/// never in `recent_jobs()`, and therefore never cancelled or expired by us
/// while it holds an `ActiveJobCount` slot. Observed 2026-10-18: #5503/#5504.
const JANITOR_ADOPT_WINDOW: u64 = 32;

/// Every 30s, reclaim this AP's orphaned jobs:
///   1. **Primary — pallet `expire_job` (permissionless):** any non-terminal job
///      past its on-chain `created_at + max_duration` deadline is reclaimed via the
///      marketplace's own timeout. Being permissionless and state-agnostic, this
///      also clears provider-abandoned `Accepted`/`Running` orphans, which a
///      requestor-side `cancel_job` can never touch.
///   2. **Fast path — `cancel_job` (requestor-only, Pending-only):** our own
///      still-`Pending` jobs past the token-scaled [`stale_secs`] window get a
///      quick requestor cancel, well before the on-chain deadline.
///
/// Both use the shared nonce stream (`*_fast`) so they can't desync request-path
/// txs. Tracking is seeded from recent chain job ids so restarts don't leak slots.
fn spawn_janitor(client: Arc<ChainClient>, rt: Arc<tokio::runtime::Runtime>) {
    // Re-adopt non-terminal jobs from the last AP epoch so the on-chain expiry can
    // still reclaim them after a restart (the in-memory list is empty then).
    // An unreadable counter seeds NOTHING: seeding `0..0` (or `0..32`) would claim a
    // bogus range and, worse, silently normalise the allocator onto id 0.
    match rt.block_on(client.try_get_next_job_id()) {
        None => println!("[janitor] cannot read NextJobId — skipping the restart re-adopt scan"),
        Some(next) => {
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
                println!(
                    "[janitor] re-adopted {seeded} non-terminal job(s) from #{start}..#{} after restart",
                    next.saturating_sub(1)
                );
            }
        }
    }
    // Our own address in the same hex form `get_job_state` reports for requestor.
    let me = client.signer_address_hex();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        // Adopt untracked non-terminal jobs of ours from the trailing id window
        // (creates that were in the pool when this process seeded `next`).
        if let Some(me) = me.as_deref() {
            let next = rt.block_on(client.get_next_job_id());
            let scan_from = next.saturating_sub(JANITOR_ADOPT_WINDOW);
            let tracked: Vec<u64> = recent_jobs()
                .lock()
                .unwrap()
                .iter()
                .map(|(id, _, _)| *id)
                .collect();
            for id in scan_from..next {
                if tracked.contains(&id) {
                    continue;
                }
                if let Some((state, _, req, _)) = rt.block_on(client.get_job_state(id)) {
                    if req.as_deref() == Some(me) && !is_terminal_state(&state) {
                        record_created(id, 0);
                        println!(
                            "[janitor] adopted untracked #{id} ({state}) — created in-flight across an AP restart"
                        );
                    }
                }
            }
        }
        let now = Instant::now();
        let head = rt.block_on(client.head_block_number()).unwrap_or(0);
        let snapshot: Vec<(u64, Instant, u64)> = recent_jobs().lock().unwrap().clone();
        let mut keep: Vec<(u64, Instant, u64)> = Vec::with_capacity(snapshot.len());
        for (id, t, mt) in snapshot {
            let age_secs = now.duration_since(t).as_secs();
            let Some((state, _, _, _)) = rt.block_on(client.get_job_state(id)) else {
                // Absent ≠ gone: our own create may still be in the pool. Keep the
                // entry (and its real `max_tokens`) through the grace window so the
                // token-scaled stale threshold stays meaningful and the adopt scan
                // does not re-add it as `max_tokens 0`.
                if survives_absent_read(age_secs, mt) {
                    keep.push((id, t, mt));
                }
                continue;
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
            // The floor is kind-aware: a fine-tune/render needs minutes to leave
            // `Pending` legitimately, so the token-scaled window alone would let
            // the AP cancel its own in-flight long job.
            let job = gpn_agent::llm::get_job(id);
            let stale_for_job = match job.as_ref() {
                Some(j) => stale_secs_for_kind(&j.kind, mt),
                None => stale_secs(mt),
            };
            if state == "Pending" && age_secs > stale_for_job {
                // Never cancel work the provider already delivered: the on-chain
                // state lags the real work whenever our accept/start txs are
                // queued behind slow blocks. Leave it to the completion path (and
                // to the pallet `expire_job` past its on-chain deadline).
                let delivered = job
                    .as_ref()
                    .map(|job| job_has_result(&job.status))
                    .unwrap_or(false);
                // Item #91: a lease this AP handed out moments ago is a claim in
                // flight — the provider's accept/start may still be in the pool,
                // so cancelling now would strand work that has no on-chain job.
                let delivered_secs = lease_delivered_secs(id, now);
                let in_flight = claim_in_flight(delivered_secs);
                if !should_cancel_stale(&state, age_secs, stale_for_job, delivered, in_flight) {
                    if in_flight {
                        if let Some(secs) = delivered_secs {
                            note_claim_veto(id, age_secs, secs);
                        }
                    }
                    keep.push((id, t, mt));
                    continue;
                }
                match rt.block_on(client.cancel_job_fast(id)) {
                    Ok(()) => {
                        println!(
                            "[janitor] cancelled stale Pending #{id} ({}, max_tokens {mt}, waited >{stale_for_job}s)",
                            job.as_ref().map(|j| j.kind.as_str()).unwrap_or("unknown")
                        );
                        // Keep tracking (with a fresh retry clock) instead of
                        // dropping the entry: dropping it let the adopt scan
                        // re-add the id as `max_tokens 0`, collapsing the
                        // token-scaled window to the 120 s floor.
                        keep.push((id, Instant::now(), mt));
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

/// Predicted job ids whose `create_job` tx is **submitted but not yet included**.
///
/// The fast path returns the predicted id as soon as the pool acks, which is a
/// block or more (10–45 s) before the id exists in `ComputeMarketplace::Jobs`.
/// `GET /job/{id}` resolved only from chain storage, so it answered
/// `{"error":"not found"}` for a job the AP had *just handed the caller* — clients
/// and monitors read that as a lost job (change-log 2026-10-16 20:05 flagged it as
/// an anomaly). Track in-flight creates so the status endpoint can report the truth:
/// `Pending`, not yet included. Entries expire so a genuinely failed create cannot
/// masquerade as pending forever.
fn pending_creates() -> &'static Mutex<HashMap<u64, Instant>> {
    static P: OnceLock<Mutex<HashMap<u64, Instant>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How long a predicted id may be reported as "pending inclusion". Must exceed the
/// create-confirm window (45 s default) plus a slow block; a create that lands after
/// this is visible from chain state anyway, and one that never lands must not linger.
const PENDING_CREATE_TTL: Duration = Duration::from_secs(180);

/// Is a create submitted `elapsed` ago still plausibly awaiting inclusion?
fn pending_create_is_live(elapsed: Duration) -> bool {
    elapsed <= PENDING_CREATE_TTL
}

fn mark_create_pending(id: u64) {
    let mut m = pending_creates().lock().unwrap();
    let now = Instant::now();
    m.retain(|_, at| pending_create_is_live(now.duration_since(*at)));
    m.insert(id, now);
}

fn create_is_pending(id: u64) -> bool {
    let mut m = pending_creates().lock().unwrap();
    let now = Instant::now();
    m.retain(|_, at| pending_create_is_live(now.duration_since(*at)));
    m.contains_key(&id)
}

fn clear_create_pending(id: u64) {
    pending_creates().lock().unwrap().remove(&id);
}

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

/// `GPN_FAST_AP` (default ON): return the predicted id as soon as the pool accepts the
/// create instead of waiting for block inclusion.
fn fast_ap_enabled() -> bool {
    std::env::var("GPN_FAST_AP")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// Per-request settlement-routing log. Off by default (one line per request is
/// noisy); `GPN_ROUTING_DEBUG=1` turns it on for a debugging session.
fn routing_debug() -> bool {
    std::env::var("GPN_ROUTING_DEBUG")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

// ── chat-mode economics ─────────────────────────────────────────────────────
//
// `/v1/chat/completions` derives ONE price card per request (see
// `gpn_agent::pricing`), so switching payment modes can never become a way to
// pay less (or be charged more): both the on-chain and channel paths price the
// same `prompt_tokens×input_frac + max_tokens` output-equivalents. The buyer
// price is GPU-independent; a faster card earns the provider more per hour.
/// Default max output tokens for a chat completion when the request omits
/// `max_tokens`/`max_completion_tokens`.
const DEFAULT_CHAT_MAX_TOKENS: u32 = 256;
/// Hard ceiling on a caller-supplied `max_tokens`. It bounds the escrow
/// commitment (and therefore the channel price) so one request cannot lock an
/// absurd amount of the AP's channel balance.
const MAX_CHAT_MAX_TOKENS: u32 = 4096;

/// Resolve the caller's output-token allowance.
///
/// Item #63: the AP used to ignore this entirely and bill/commit a hard-coded
/// 512 for every request, so a 4-token answer cost the same as a 512-token one
/// (~58x over-charge measured live). The caller's value now drives the inference
/// cap, the escrow commitment and the channel price ceiling.
fn chat_max_tokens(v: &serde_json::Value) -> u32 {
    let asked = v
        .get("max_tokens")
        .and_then(|x| x.as_u64())
        .or_else(|| v.get("max_completion_tokens").and_then(|x| x.as_u64()))
        .filter(|x| *x > 0)
        .unwrap_or(DEFAULT_CHAT_MAX_TOKENS as u64);
    asked.min(MAX_CHAT_MAX_TOKENS as u64) as u32
}

/// How long a create may take to appear on-chain before we give up on it. GPN blocks are
/// PoW-mined on a 15 s target with observed gaps of 2–30 s, so the old fixed 2 s window was
/// shorter than a single block: a create that landed normally was treated as "did not land",
/// the allocator was rewound to a stale tip, and the *next* create reused the id the slow tx
/// was about to take (the documented +N label drift).
fn create_confirm_ms() -> u64 {
    std::env::var("GPN_CREATE_CONFIRM_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 200)
        .unwrap_or(45_000)
}

/// Poll `ComputeMarketplace::NextJobId` until it moves past `next_before` (our create was
/// included) or the deadline expires. Returns the id the chain actually assigned.
fn confirm_created(
    rt: &tokio::runtime::Runtime,
    client: &gpn_agent::chain::ChainClient,
    next_before: u64,
    predicted: u64,
) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_millis(create_confirm_ms());
    while Instant::now() < deadline {
        let after = rt.block_on(client.get_next_job_id());
        if after > next_before {
            let landed = after - 1;
            if landed != predicted {
                println!(
                    "[ids] create landed as #{landed} (predicted #{predicted}) — realigning allocator"
                );
                align_alloc(after);
            }
            return Some(landed);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

/// Outcome of one `create_job` submission.
struct CreateOutcome {
    created: bool,
    id: u64,
    err: Option<String>,
}

/// Submit a `create_job` through the AP's ONE fast nonce stream and resolve the job id.
///
/// Root cause this exists for: the access point used to have **two** nonce allocators for
/// the same hot-wallet account — `submit_signed` (stop-and-wait creates: `POST /jobs`, and
/// the `GPN_FAST_AP=0` fallback) read the nonce straight from chain state, while
/// `NonceStream` (fast path: `/v1/chat/completions`, the janitor) kept its own pipeline.
/// When both were used, whichever submitted second reused a nonce the other already owned:
/// the pool rejected it (`1014 Priority is too low`) or, if the other tx had been included
/// meanwhile, the chain rejected it (`1010 Transaction is outdated`) — a create lost
/// outright, and its locally-consumed id left the predictor one ahead of the chain.
///
/// Now **every** create goes through the stream (single nonce authority). `fast` only decides
/// whether we report the predicted id immediately or wait for inclusion and report the id the
/// chain actually assigned.
///
/// MUST be called with `CREATE_LOCK` held (exactly one create in flight).
fn submit_create<F>(
    rt: &tokio::runtime::Runtime,
    client: &gpn_agent::chain::ChainClient,
    next_before: u64,
    fast: bool,
    submit: F,
) -> CreateOutcome
where
    F: FnOnce() -> Result<(), Box<dyn std::error::Error>>,
{
    let predicted = alloc_predicted_job_id(next_before);
    mark_create_pending(predicted);
    match submit() {
        Ok(()) if fast => CreateOutcome {
            created: true,
            id: predicted,
            err: None,
        },
        Ok(()) => match confirm_created(rt, client, next_before, predicted) {
            Some(id) => CreateOutcome {
                created: true,
                id,
                err: None,
            },
            None => {
                // Pool-acked but not visible yet: keep the prediction, don't rewind the
                // allocator (the tx is still in the pool and will take that id).
                println!(
                    "[ids] create not confirmed within {} ms — keeping predicted #{predicted}",
                    create_confirm_ms()
                );
                CreateOutcome {
                    created: true,
                    id: predicted,
                    err: None,
                }
            }
        },
        Err(e) => {
            println!("[create] create_job submit rejected: {e}");
            // The create may still have landed despite a slow / un-acked pool submit.
            match confirm_created(rt, client, next_before, predicted) {
                Some(id) => CreateOutcome {
                    created: true,
                    id,
                    err: None,
                },
                None => {
                    // Genuinely did not land: realign to the *current* tip (never to the
                    // pre-submit snapshot — the chain may have advanced meanwhile).
                    let tip = rt.block_on(client.get_next_job_id());
                    align_alloc(if tip > next_before { tip } else { next_before });
                    CreateOutcome {
                        created: false,
                        id: predicted,
                        err: Some(e.to_string()),
                    }
                }
            }
        }
    }
}

/// A known provider endpoint + its last-seen health.
#[derive(Clone, Debug)]
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
    /// Job types this provider advertises it can serve (`capabilities[]` from
    /// the heartbeat). Defaults to the full set when a box omits the field, so
    /// a node designed to serve everything is routable for everything.
    capabilities: Vec<String>,
    /// libp2p peer id published by the heartbeat (`system_localPeerId`). Only
    /// present on boxes that can talk to their node RPC.
    peer_id: Option<String>,
    /// Public, dialable p2p multiaddrs (each ends in `/p2p/<peer_id>`).
    /// Sanitised on ingest — never relayed blindly.
    p2p_multiaddrs: Vec<String>,
    /// The provider's libp2p TCP port, advertised even behind NAT (the
    /// directory combines it with the host it reaches the provider on).
    p2p_port: Option<u16>,
    active: u64,
    capacity: u64,
    latency_ms: u64,
    last_seen: Option<Instant>,
    /// When this provider last delivered a pull poll (`GET /assignments`) — i.e.
    /// when we last proved we can actually HAND it work.
    ///
    /// The `POST /assign` push was deleted (#46 Phase 3), so a provider's own
    /// poll is the ONLY transport left. A box that only ever *answered* `/status`
    /// (a push-era build, or one pointed at a different access point) is
    /// reachable and reports `healthy: true`, yet no lease can ever reach it —
    /// routing to it black-holes the job until the janitor cancels it (#6237).
    pulled_seen: Option<Instant>,
}

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
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

/// How long a capability snapshot received in a pull poll is trusted. The
/// poll long-poll is 25 s, so a live provider refreshes this every ~25 s; three
/// missed polls (75 s) retire it, which is the same order as the 5 s `/status`
/// poll's tolerance for a box that has gone away.
const POLL_CAPS_TTL_SECS: u64 = 90;

/// Latency assumed for a provider we have never dialled (pull-only behind
/// NAT). It is deliberately neutral: a client-measured RTT hint still wins in
/// `pick()`, and an AP-measured value (when a `/status` endpoint exists) is
/// preferred over it.
fn pull_default_latency_ms() -> u64 {
    std::env::var("GPN_PULL_DEFAULT_LATENCY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100)
}

/// Accept only a plausible libp2p peer id (base58, ed25519 `12D3Koo…` or
/// legacy `Qm…`).
fn clean_peer_id(raw: &str) -> Option<String> {
    gpn_agent::seedlist::clean_peer_id(raw)
}

/// Structural multiaddr check: `/proto/value/…`, every protocol whitelisted,
/// numeric ports in range, and a trailing `/p2p/<peer-id>` (without one the
/// client cannot dial it, so it is not worth publishing).
///
/// Delegates to [`gpn_agent::seedlist::clean_published_multiaddr`] so the access
/// point and the provider heartbeat share ONE definition — a round-trip test
/// there proves the emitter's output is accepted here.
fn clean_p2p_multiaddr(raw: &str) -> Option<String> {
    gpn_agent::seedlist::clean_published_multiaddr(raw)
}

fn p2p_port_of(multiaddr: &str) -> Option<u16> {
    let (_, rest) = multiaddr.split_once("/tcp/")?;
    let port: u32 = rest.split('/').next()?.parse().ok()?;
    if (1..=65535).contains(&port) {
        Some(port as u16)
    } else {
        None
    }
}

/// Extract + sanitise the peer-publication fields from a heartbeat/caps value.
///
/// A peer id is required before any multiaddr is trusted: every address must
/// end in `/p2p/<that same id>`, so a provider cannot advertise an address for
/// somebody else's identity.
fn p2p_fields(v: &Value) -> (Option<String>, Vec<String>, Option<u16>) {
    let peer_id = v
        .get("peer_id")
        .and_then(|x| x.as_str())
        .and_then(clean_peer_id);

    let mut multiaddrs: Vec<String> = Vec::new();
    if let Some(pid) = peer_id.as_deref() {
        if let Some(arr) = v.get("multiaddrs").and_then(|x| x.as_array()) {
            for raw in arr.iter().filter_map(|s| s.as_str()) {
                if let Some(ma) = clean_p2p_multiaddr(raw) {
                    let trailing = ma.rsplit_once("/p2p/").map(|(_, id)| id).unwrap_or("");
                    if trailing == pid {
                        multiaddrs.push(ma);
                    }
                }
            }
        }
        multiaddrs.sort();
        multiaddrs.dedup();
    }

    // A p2p port is only meaningful **together with the peer id that owns it**:
    // the directory synthesises `<host the AP reached>:<p2p_port>/p2p/<peer_id>`,
    // so publishing a port without its own identity would let one node's port
    // be combined with another node's peer id. No identity → no port.
    let p2p_port = if peer_id.is_some() {
        v.get("p2p_port")
            .and_then(|x| x.as_u64())
            .filter(|p| (1..=65535).contains(p))
            .map(|p| p as u16)
            .or_else(|| multiaddrs.iter().find_map(|m| p2p_port_of(m)))
    } else {
        None
    };

    (peer_id, multiaddrs, p2p_port)
}

/// Build a registry entry from a capability snapshot that arrived in a pull
/// poll (#46 Phase 3 step 2). This is the SAME field set `/status` provides,
/// minus anything the provider did not report (defaults mirror the `/status`
/// path so a partial snapshot cannot make an entry unroutable).
fn provider_from_caps(addr: &str, v: &Value, status_url: String, latency_ms: u64) -> Provider {
    let arr_of = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let caps = arr_of("capabilities");
    let (peer_id, p2p_multiaddrs, p2p_port) = p2p_fields(v);
    Provider {
        status_url,
        address: addr.to_string(),
        gflops: v.get("gflops").and_then(|x| x.as_u64()).unwrap_or(0),
        models: arr_of("models"),
        can_pull: v.get("can_pull").and_then(|x| x.as_bool()).unwrap_or(false),
        tok_per_s: v.get("tok_per_s").and_then(|x| x.as_f64()).unwrap_or(40.0),
        owner_cost_usd_hr: v
            .get("owner_cost_usd_hr")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.03),
        mem_bw_gbps: v
            .get("mem_bw_gbps")
            .and_then(|x| x.as_f64())
            .unwrap_or(250.0),
        vram_mb: v.get("vram_mb").and_then(|x| x.as_u64()).unwrap_or(0),
        ram_mb: v.get("ram_mb").and_then(|x| x.as_u64()).unwrap_or(0),
        gpu_class: v
            .get("gpu_class")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string(),
        tflops: v.get("tflops").and_then(|x| x.as_f64()).unwrap_or(0.0),
        capabilities: if caps.is_empty() {
            default_capabilities()
        } else {
            caps
        },
        peer_id,
        p2p_multiaddrs,
        p2p_port,
        active: v.get("active").and_then(|x| x.as_u64()).unwrap_or(0),
        capacity: v
            .get("capacity")
            .and_then(|x| x.as_u64())
            .unwrap_or(1)
            .max(1),
        latency_ms,
        last_seen: Some(Instant::now()),
        pulled_seen: None,
    }
}

/// Record a capability snapshot delivered by `GET /assignments?…&caps=…`.
///
/// Keyed by the canonical ss58 address (the poll's `provider` may be hex), so
/// a snapshot merges with an on-chain-discovered entry for the same box rather
/// than duplicating it.
fn ingest_caps(
    poll_caps: &Arc<Mutex<std::collections::HashMap<String, (Instant, Value)>>>,
    provider: &str,
    caps: &Value,
) {
    let addr = if provider.starts_with("0x") || provider.starts_with("0X") {
        hex_to_ss58(provider).unwrap_or_else(|| provider.to_string())
    } else {
        provider.to_string()
    };
    // The snapshot's own `address` must never redirect the entry: the caller is
    // authenticated as `provider`, so that is who these capabilities belong to.
    poll_caps
        .lock()
        .unwrap()
        .insert(addr, (Instant::now(), caps.clone()));
}

/// Merge fresh poll-delivered capability snapshots into the `/status`-pulled
/// registry (#46 Phase 3 step 2).
///
/// This is what removes the inbound-`/status` dependency: a provider behind
/// NAT/CGNAT never appears in the URL list (it has no reachable endpoint), but
/// its own poll feeds the registry, so the scheduler can route to it. A snapshot
/// for a box that also has a dialable `/status` replaces the measured fields
/// but KEEPS the AP-measured latency and the endpoint URL (the endpoint is a
/// routing key for client RTT hints and the diffusion port mapping).
fn apply_poll_caps(
    out: &mut Vec<Provider>,
    poll_caps: &Arc<Mutex<std::collections::HashMap<String, (Instant, Value)>>>,
) {
    let snapshots: Vec<(String, Value)> = {
        let map = poll_caps.lock().unwrap();
        map.iter()
            .filter(|(_, (seen, _))| seen.elapsed().as_secs() <= POLL_CAPS_TTL_SECS)
            .map(|(addr, (_, v))| (addr.clone(), v.clone()))
            .collect()
    };
    for (addr, caps) in snapshots {
        let existing = out
            .iter()
            .position(|p| p.address.eq_ignore_ascii_case(&addr));
        match existing {
            Some(i) => {
                let status_url = out[i].status_url.clone();
                let measured = out[i].latency_ms;
                let latency = if measured > 0 && measured < 9999 {
                    measured
                } else {
                    pull_default_latency_ms()
                };
                out[i] = merge_poll_snapshot(&out[i], &caps, status_url, latency);
                out[i].pulled_seen = Some(Instant::now());
            }
            None => {
                let mut fresh = provider_from_caps(
                    &addr,
                    &caps,
                    format!("pull://{addr}"),
                    pull_default_latency_ms(),
                );
                fresh.pulled_seen = Some(Instant::now());
                out.push(fresh);
            }
        }
    }
}

/// Merge a poll-delivered capability snapshot into an existing registry entry.
///
/// The existing entry's `status_url` is the endpoint the access point actually
/// measured, so **that record is authoritative for which box this is**. The p2p
/// identity (peer id + port) belongs to exactly one node; a snapshot that names
/// a *different* peer id for that endpoint must therefore never overwrite it.
///
/// Without this rule the AP would publish
/// `/ip4/<this record's status_url host>/tcp/30333/p2p/<another node's id>` —
/// which is precisely the mis-paired entry observed on gpncoin.ai/seeds.json
/// (`/ip4/23.241.109.255/…/p2p/12D3KooWAFtUNxc…`, box #1's host carrying the
/// Linux node's peer id). Two boxes sharing one ss58 address (or one of them
/// reporting a stale id) is enough to trigger it.
fn merge_poll_snapshot(
    existing: &Provider,
    caps: &Value,
    status_url: String,
    latency_ms: u64,
) -> Provider {
    let mut merged = provider_from_caps(&existing.address, caps, status_url, latency_ms);
    match existing.peer_id.as_deref() {
        Some(have) if merged.peer_id.as_deref() != Some(have) => {
            log::warn!(
                "providers: {} polled a peer_id ({}) that differs from the one measured for \
                 this endpoint ({}); keeping the measured identity so the directory cannot \
                 pair this host with another node's peer id",
                merged.status_url,
                merged.peer_id.as_deref().unwrap_or("none"),
                have
            );
            merged.peer_id = existing.peer_id.clone();
            merged.p2p_multiaddrs = existing.p2p_multiaddrs.clone();
            merged.p2p_port = existing.p2p_port;
        }
        _ => {
            // Same identity (or none measured yet): never lose information.
            if merged.p2p_multiaddrs.is_empty() {
                merged.p2p_multiaddrs = existing.p2p_multiaddrs.clone();
            }
            if merged.p2p_port.is_none() {
                merged.p2p_port = existing.p2p_port;
            }
        }
    }
    merged
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
    poll_caps: Arc<Mutex<std::collections::HashMap<String, (Instant, Value)>>>,
) {
    let reg = registry.clone();
    let urls = urls.clone();
    std::thread::spawn(move || loop {
        // 1. Merge the static list with the on-chain discovered endpoints.
        let mut all: Vec<String> = urls.iter().cloned().collect();
        // A provider discovered on-chain identifies itself by its storage KEY
        // (the AccountId), so keep url -> account so we can attribute an
        // endpoint whose /status heartbeat omits its own `address`.
        let mut chain_addr: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
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
                Some(v) => {
                    let addr = attribute_address(
                        v.get("address").and_then(|x| x.as_str()).unwrap_or(""),
                        u,
                        &chain_addr,
                    );
                    let latency = v
                        .get("__latency_ms")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(9999);
                    provider_from_caps(&addr, &v, u.clone(), latency)
                }
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
                    capabilities: default_capabilities(),
                    peer_id: None,
                    p2p_multiaddrs: vec![],
                    p2p_port: None,
                    active: 0,
                    capacity: 0,
                    latency_ms: 9999,
                    last_seen: None,
                    pulled_seen: None,
                },
            };
            out.push(p);
        }

        // 3. De-duplicate by provider address (a node listed both statically and
        //    on-chain must not be counted twice). Keep the lowest-latency entry.
        out.sort_by_key(|p| p.latency_ms);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        out.retain(|p| p.address.is_empty() || seen.insert(p.address.clone()));

        // 4. Merge capabilities delivered by providers' own pull polls (#46
        //    Phase 3 step 2). This is the ONLY way a NAT'd/CGNAT provider can
        //    enter the registry: the AP never dials it.
        apply_poll_caps(&mut out, &poll_caps);

        // 4b. Two records publishing the SAME libp2p peer id means at least one
        //     of them is republishing another node's identity — exactly the
        //     2026-10-23 box1 defect, where a provider's heartbeat read its
        //     peer id from the seed node's RPC. The suffix check cannot catch
        //     it (the id matches its own address list); only a cross-record
        //     comparison can. Warn, keyed on the pair, so it is visible in the
        //     AP log instead of silently poisoning the directory.
        warn_duplicate_peer_ids(&out);

        *reg.lock().unwrap() = out;
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// Warn (once per id/host pair) when two registry records claim one peer id.
/// One node = one libp2p key, so this is always a mis-publication.
fn warn_duplicate_peer_ids(providers: &[Provider]) {
    use std::collections::HashMap;
    let mut owners: HashMap<&str, Vec<&str>> = HashMap::new();
    for p in providers {
        if let Some(pid) = p.peer_id.as_deref() {
            let host = p
                .p2p_multiaddrs
                .first()
                .and_then(|m| m.split("/tcp/").next())
                .unwrap_or(&p.status_url);
            owners.entry(pid).or_default().push(host);
        }
    }
    for (pid, mut hosts) in owners {
        hosts.sort_unstable();
        hosts.dedup();
        if hosts.len() < 2 {
            continue;
        }
        let key = format!("{pid}|{}", hosts.join(","));
        if !warn_once(&key) {
            continue;
        }
        log::warn!(
            "providers: peer_id {pid} is published by {} different hosts ({}) — one node has one \
             libp2p identity, so at least one heartbeat is republishing another node's peer id \
             (check its --node-endpoint/GPN_NODE_ENDPOINT); its multiaddrs are undialable",
            hosts.len(),
            hosts.join(", ")
        );
    }
}

/// Resolve a registry entry's `address`: prefer the provider's own heartbeat
/// `address`; when it is empty (an on-chain-discovered endpoint whose `/status`
/// was started without `--address`), fall back to the provider's on-chain key so
/// the entry stays attributable (assignments / `routed_to` / client RTT hints
/// key on `address`).
fn attribute_address(
    hb_addr: &str,
    url: &str,
    chain_addr: &std::collections::HashMap<String, String>,
) -> String {
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

/// The full job-type capability set a node serves *by design*. Used when a
/// provider's `/status` omits `capabilities[]` (older heartbeat): assume it is
/// equipped for everything rather than silently dropping its job types. Note
/// “one GPU, one job” is mutual exclusion (one job at a time) — never a cap on
/// how many *kinds* of job a node may serve.
fn default_capabilities() -> Vec<String> {
    [
        "LLMInference",
        "ImageGeneration",
        "VideoGeneration",
        "MLInference",
        "FineTuning",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn has_model(p: &Provider, model: &str) -> bool {
    if model.is_empty() || model == "auto" {
        !p.models.is_empty()
    } else {
        p.models
            .iter()
            .any(|m| m == model || m.split(':').next() == Some(model))
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

/// `Retry-After` we hand back while the registry is still warming up.
const REGISTRY_WARMING_RETRY_AFTER: &str = "5";

/// The `Retry-After` header for a warming-up registry (tiny_http wants a built
/// `Header`, and these bytes are static so the parse cannot fail).
fn retry_after_header() -> Header {
    Header::from_bytes(&b"Retry-After"[..], REGISTRY_WARMING_RETRY_AFTER.as_bytes())
        .expect("static header")
}

/// Classify a failed model resolution: `(http_status, client_message)`.
///
/// An **empty** registry does not mean "unknown model" — it means nothing has
/// been polled yet. The AP binds its listener before the first `refresh()` pass
/// completes, so for the first few seconds after a restart (plus the on-chain
/// `list_provider_endpoints` RPC) every text request used to be told
/// `unknown model 'llama3.2:1b'; available: []` with **400** — a terminal-looking
/// answer for a transient condition (measured 2026-10-22 11:36: 8.5 s and 41.8 s
/// probes succeeded either side of it). So: empty ⇒ **503** + `Retry-After`;
/// non-empty ⇒ keep the hard **400**, because rejecting a model no provider can
/// serve is what stops orphaned `Pending` jobs saturating `MaxActiveJobs`.
fn model_resolution_status(reg: &[Provider], err: &str) -> (u16, String) {
    if reg.is_empty() {
        (
            503,
            "provider registry is warming up (no providers polled yet); retry shortly".to_string(),
        )
    } else {
        (400, err.to_string())
    }
}

/// Job-relative GPN price for an image/video generation job.
///
/// The model lives in `gpn_agent::pricing` so it shares **one** anchor with the
/// token curve: GPU-seconds (measured: 30 s pipeline load + 0.73 s per
/// megapixel·step on the 4060 anchor) × the anchor card's hourly rate × margin.
/// The buyer's price is therefore GPU-independent, exactly like the token price;
/// the legacy oracle-market formula (`market_hr` = $3/hr) is still reachable via
/// `GPN_PRICING_MODEL=market`. `frames = 1` for an image.
fn image_price_gpn(
    pcfg: &PricingConfig,
    w: f64,
    h: f64,
    steps: f64,
    frames: f64,
    token_price_usd: f64,
    market_hr: f64,
    util: f64,
) -> f64 {
    pcfg.diffusion_price_gpn(w, h, steps, frames, token_price_usd, market_hr, util)
}

// ── unified routing (queue item #70) ─────────────────────────────────────────
//
// Every AI request takes the SAME steps as a chat request: find the first
// available provider that can run it, and if that one cannot *take* the work,
// try the next. Before this, text routing used `pick()` (model-aware) while
// image/video generation used `pick_gen_provider()` (capability-aware), and
// both were single-shot — a wrong, down or incapable first choice failed the
// request instead of moving on.

// What kind of work a request is lives in `gpn_agent::jobkind` (item #76): the AP,
// the submit CLI and the providers must classify a request identically, or a
// generation model gets routed to a chat box and the job sits `Running` until
// the on-chain deadline. `JobKind`, `model_kind` and `admit` are imported above.

/// Everything a request needs from a provider. `Default` = text routing with no
/// hardware floor (the `/v1/chat/completions` case).
#[derive(Clone, Debug, Default)]
struct RouteNeeds {
    /// Requested model; empty/`auto` = any model the provider has.
    model: String,
    min_gflops: u64,
    min_vram_mb: u64,
    min_ram_mb: u64,
    min_tflops: f64,
    /// Cost of a cold pull, used only to rank pullers against ready boxes.
    pull_penalty_ms: u64,
}

/// One ordered routing candidate. `needs_pull` means the box must download the
/// model before it can run the job.
#[derive(Clone, Debug)]
struct RouteCandidate {
    provider: Provider,
    needs_pull: bool,
}

/// Penalty added to a busy provider's score so every idle box outranks it.
const BUSY_PENALTY_MS: u64 = 10_000;

/// How many providers one request may try before giving up (item #70).
const MAX_ROUTE_ATTEMPTS: usize = 3;

/// **The one way any AI request finds providers** (queue item #70).
///
/// Shared by `/v1/chat/completions`, `POST /jobs` (text, image, video, ML and
/// fine-tune), `/v1/images/generations` and `POST /route/{id}`. Pure: no I/O, no
/// globals, so it is unit-testable and the ordering is reproducible.
///
/// Filters (a provider must clear ALL of them):
///   • `last_seen` present — the 5 s `/status` poll refreshes it, and a failed
///     poll rewrites the entry with `last_seen: None`, so this IS the health +
///     freshness test;
///   • `pulled_seen` within [`POLL_CAPS_TTL_SECS`] — delivery is the provider's
///     own pull poll (#46 Phase 3), so a box that never polls cannot be fed;
///   • advertises the capability for this [`JobKind`];
///   • model present in `models[]` (text only; a puller is still eligible);
///   • `p.gflops >= min_gflops` when a floor is requested;
///   • `vram_mb`/`ram_mb` clear `min_vram_mb`/`min_ram_mb` when requested, and
///     the model's weights fit the advertised VRAM;
///   • `min_tflops` floor when requested (fail closed: unknown TFLOPS needs a
///     self-declared discrete card);
///   • free capacity (`active < capacity`) — but when *no* candidate has any,
///     busy boxes are kept as last-resort candidates so the job QUEUES instead
///     of being refused (the long-standing behaviour).
///
/// Ordering: idle before busy, and within a tier by RTT-aware latency (the
/// caller's own measurement wins over the AP's), with `pull_penalty_ms` added
/// to a puller's score so a cold download only wins when it is genuinely faster.
fn route_candidates(
    registry: &[Provider],
    kind: JobKind,
    needs: &RouteNeeds,
    rtt: &RttOverrides,
) -> Vec<RouteCandidate> {
    let cap = kind.capability();
    let has_cap = |p: &Provider| p.capabilities.iter().any(|c| c.eq_ignore_ascii_case(cap));
    let deliverable = |p: &Provider| {
        p.pulled_seen
            .map(|t| t.elapsed().as_secs() <= POLL_CAPS_TTL_SECS)
            .unwrap_or(false)
    };
    let hw_ok = |p: &Provider| {
        (needs.min_vram_mb == 0 || p.vram_mb >= needs.min_vram_mb)
            && (needs.min_ram_mb == 0 || p.ram_mb >= needs.min_ram_mb)
    };
    let gpu_ok = |p: &Provider| {
        if needs.min_tflops <= 0.0 {
            return true;
        }
        if p.tflops > 0.0 {
            return p.tflops >= needs.min_tflops;
        }
        p.gpu_class == "discrete"
    };
    let has_free = |p: &Provider| p.capacity == 0 || p.active < p.capacity;

    let eligible: Vec<&Provider> = registry
        .iter()
        .filter(|p| {
            p.last_seen.is_some()
                && deliverable(p)
                && has_cap(p)
                && p.gflops >= needs.min_gflops
                && hw_ok(p)
                && gpu_ok(p)
                // Model gate (text/ML/fine-tune): the box must already have the
                // model or be able to pull it. Generation ignores the model name.
                && (!kind.gates_on_model()
                    || has_model(p, &needs.model)
                    || p.can_pull)
                && gpn_agent::models::model_fits_vram(&needs.model, p.vram_mb)
        })
        .collect();

    // Free capacity is a filter when anything has it; otherwise queue on the
    // least-loaded boxes rather than turning a busy fleet into a 503.
    let any_free = eligible.iter().any(|p| has_free(p));
    let live: Vec<&Provider> = eligible
        .into_iter()
        .filter(|p| !any_free || has_free(p))
        .collect();

    let eff = |p: &Provider| {
        routing::effective_latency_ms(
            p.latency_ms,
            routing::lookup_override(rtt, &p.address, &p.status_url),
        )
    };

    // Score = busy tier + latency (+ pull cost). Lower is better.
    let mut scored: Vec<(u64, bool, Provider)> = live
        .into_iter()
        .map(|p| {
            let ready = !kind.gates_on_model() || has_model(p, &needs.model);
            // A puller (text only) pays the download penalty; without a model
            // gate there is nothing to pull.
            let pull = kind.gates_on_model() && !ready;
            let score = if p.active == 0 { 0 } else { BUSY_PENALTY_MS }
                + p.active.min(9) * 100
                + eff(p)
                + if pull { needs.pull_penalty_ms } else { 0 };
            (score, pull, (*p).clone())
        })
        .collect();
    // Stable tie-break on address so two otherwise-identical boxes always
    // produce the same order (reproducible tests, no route flapping).
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.address.cmp(&b.2.address)));
    scored
        .into_iter()
        .map(|(_, needs_pull, provider)| RouteCandidate {
            provider,
            needs_pull,
        })
        .collect()
}

/// Outcome of walking the candidate list.
///
/// `Served` carries the whole response value plus the provider record, so this
/// enum is intentionally lopsided: it is a routing result, consumed once per
/// request, never stored in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Failover<T> {
    /// A provider took the work and (for channel jobs) finished it.
    Served {
        value: T,
        provider: Provider,
        /// Whether the SERVING provider must pull the model first.
        needs_pull: bool,
        attempted: usize,
    },
    /// The last attempt may have been accepted — STOP. Retrying could charge
    /// the caller twice, which is worse than an honest 502.
    Ambiguous { reason: String, attempted: usize },
    /// Every candidate refused *before* doing any work (and nothing was paid).
    Exhausted { reasons: Vec<String> },
}

/// Walk `candidates` in order, stopping at the first provider that serves the
/// request (queue item #70).
///
/// `attempt` returns `Err(ChannelFail::NotAttempted)` for refusals that provably
/// moved no value (hub disabled for that provider, connect error, 4xx at accept,
/// provider-signed refund, rejection before the work started) — those log a
/// reason and move to the NEXT candidate. An `Ambiguous` failure (5xx, an
/// unverifiable 200) stops immediately: the provider may hold a pre-signed
/// settle, so a second attempt risks a double charge.
fn failover<T, F>(label: &str, candidates: &[RouteCandidate], mut attempt: F) -> Failover<T>
where
    F: FnMut(&Provider) -> Result<T, gpn_agent::hub_ap::ChannelFail>,
{
    use gpn_agent::hub_ap::ChannelFail;
    let mut reasons: Vec<String> = Vec::new();
    for (i, cand) in candidates.iter().take(MAX_ROUTE_ATTEMPTS).enumerate() {
        match attempt(&cand.provider) {
            Ok(value) => {
                if i > 0 {
                    eprintln!(
                        "[route] {label}: served by candidate #{} ({}) after {} refusal(s)",
                        i + 1,
                        cand.provider.address,
                        i
                    );
                }
                return Failover::Served {
                    value,
                    provider: cand.provider.clone(),
                    needs_pull: cand.needs_pull,
                    attempted: i + 1,
                };
            }
            Err(ChannelFail::NotAttempted(why)) => {
                eprintln!(
                    "[route] {label}: candidate #{} ({}) not attempted: {why} — trying the next provider",
                    i + 1,
                    cand.provider.address
                );
                reasons.push(format!("{}: {why}", cand.provider.address));
            }
            Err(ChannelFail::Ambiguous(why)) => {
                eprintln!(
                    "[route] {label}: candidate #{} ({}) FAILED AMBIGUOUSLY: {why} — stopping (no retry, to avoid a double charge)",
                    i + 1,
                    cand.provider.address
                );
                return Failover::Ambiguous {
                    reason: why,
                    attempted: i + 1,
                };
            }
        }
    }
    Failover::Exhausted { reasons }
}

/// All candidates refused and nothing was paid: 503 whose body lists the reason
/// for EACH candidate, so an operator can see why nobody served the request.
fn exhaustion_json(kind: JobKind, reasons: &[String]) -> Value {
    json!({
        "error": {
            "message": format!(
                "no {} provider could take the request after {} attempt(s)",
                kind.label(),
                reasons.len()
            ),
            "type": "gpn_unavailable",
            "candidates": reasons,
        }
    })
}

/// Estimated render cost in seconds for one image request (`n` sequential
/// renders on one GPU). Shared by the sync-mode gate and the job record.
fn image_estimate_seconds(pcfg: &PricingConfig, w: u32, h: u32, steps: u32, n: u32) -> f64 {
    pcfg.diffusion_seconds(w as f64, h as f64, steps as f64, 1.0) * n.max(1) as f64
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

// ── public fine-tuning (queue item #67) ─────────────────────────────────────
//
// A tune used to be reachable only through the legacy `gpn_api` bin: the modern
// access point could not create a `FineTuning` job at all, so a fine-tune on
// testnet could only be produced by hand. These routes give fine-tuning the same
// public shape as images: submit → `202` + a poll URL, then poll for the tuned
// model name (`tuned-<job>`) the provider registers when it finishes.

/// Hard cap on dataset size: a fine-tune is minutes of GPU on the ONLY GPU this
/// box has, and the dataset travels inside one JSON payload in the off-chain
/// store. 200 pairs is plenty for a LoRA demo and keeps the request bounded.
const MAX_FINETUNE_EXAMPLES: usize = 200;

/// Validate a public fine-tune request.
///
/// Returns `(base_model, dataset_json, lora_r, epochs)` where `dataset_json` is
/// the normalized JSON array string the provider expects. Pure, so the accept /
/// reject matrix is unit-tested without touching a chain or an HTTP socket.
fn parse_finetune_request(v: &Value) -> Result<(String, String, u32, u32), String> {
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if model.is_empty() {
        return Err(
            "missing model: send the base model to fine-tune, e.g. {\"model\": \"llama3.2:1b\"}"
                .into(),
        );
    }
    let raw = v.get("dataset").ok_or_else(|| {
        "missing dataset: send an array of {\"prompt\": ..., \"completion\": ...} pairs".to_string()
    })?;
    // Accept the array itself or a pre-encoded string (some clients serialize
    // form fields as JSON text). Both normalize to the same payload.
    let arr = match raw {
        Value::Array(a) => a.clone(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|x| x.as_array().cloned())
            .ok_or_else(|| {
                "dataset must be a JSON array of {\"prompt\": ..., \"completion\": ...} pairs"
                    .to_string()
            })?,
        _ => {
            return Err(
                "dataset must be a JSON array of {\"prompt\": ..., \"completion\": ...} pairs"
                    .into(),
            )
        }
    };
    if arr.is_empty() {
        return Err(
            "dataset is empty: at least one {\"prompt\", \"completion\"} pair is required".into(),
        );
    }
    if arr.len() > MAX_FINETUNE_EXAMPLES {
        return Err(format!(
            "dataset too large: {} pairs (max {MAX_FINETUNE_EXAMPLES})",
            arr.len()
        ));
    }
    let mut clean: Vec<Value> = Vec::with_capacity(arr.len());
    for (i, ex) in arr.iter().enumerate() {
        let p = ex
            .get("prompt")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        let c = ex
            .get("completion")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if p.is_empty() || c.is_empty() {
            return Err(format!(
                "dataset[{i}] needs a non-empty \"prompt\" and \"completion\""
            ));
        }
        clean.push(json!({"prompt": p, "completion": c}));
    }
    let lora_r = v
        .get("lora_r")
        .and_then(|x| x.as_u64())
        .unwrap_or(16)
        .clamp(1, 128) as u32;
    let epochs = v
        .get("epochs")
        .and_then(|x| x.as_u64())
        .unwrap_or(2)
        .clamp(1, 10) as u32;
    Ok((model, Value::Array(clean).to_string(), lora_r, epochs))
}

/// Public status for a fine-tune: the off-chain record is authoritative while the
/// job runs, the chain is authoritative for terminal states (a provider can die
/// after writing `done` but before `complete_job`, and the expiry sweep then
/// fails the job — reporting `succeeded` there would be a lie).
fn finetune_public_status(offchain: Option<&str>, chain_state: Option<&str>) -> &'static str {
    match chain_state {
        Some("Completed") => return "succeeded",
        Some("Failed") | Some("Cancelled") => return "failed",
        _ => {}
    }
    match offchain {
        Some("done") => "succeeded",
        Some("error") => "failed",
        Some("running") => "running",
        _ => match chain_state {
            Some("Accepted") | Some("Running") | Some("AwaitingVerification") => "running",
            _ => "queued",
        },
    }
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
                eprintln!(
                    "[moderation] refused a prompt in category {}",
                    category.id()
                );
                Some(category)
            }
            Verdict::Allow => None,
        },
    }
}

// `params_b_for` and `roofline_tok_per_s` live in `gpn_agent::pricing` (shared
// with the provider-earnings table and the pure-function test suite).

// ── price feed cache ────────────────────────────────────────────────────────
//
// Both the chat facade and `POST /jobs` need the on-chain GPN price (to convert
// a USD rate into GPN base units) plus, under `GPN_PRICING_MODEL=market`, the
// oracle's GPU-hour price. Fetching that per request would put a chain RPC on
// the fast chat path; instead a background refresher keeps a snapshot warm and
// request handlers read it lock-and-copy.
#[derive(Clone, Copy, Debug)]
struct FeedSnapshot {
    token_price_usd: f64,
    market_hr: f64,
    util: f64,
}

fn feed_snapshot() -> &'static Mutex<FeedSnapshot> {
    static F: OnceLock<Mutex<FeedSnapshot>> = OnceLock::new();
    F.get_or_init(|| {
        Mutex::new(FeedSnapshot {
            // Same fallbacks the request path used when `get_feed()` failed.
            token_price_usd: 0.15,
            market_hr: 3.0,
            util: 0.65,
        })
    })
}

fn feed_now() -> FeedSnapshot {
    *feed_snapshot().lock().unwrap()
}

/// Where the prices currently in force came from (item #68).
///
/// The refresher used to fall back to the compiled defaults **silently**: the
/// only way to tell whether jobs were priced off the on-chain
/// `ProfitabilityOracle::CurrentFeed` or off `$0.15 / $3.00 / 65%` was to read
/// chain state by hand. That is how the feed stayed unposted for weeks without
/// anyone noticing. Every transition is now logged once.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FeedSource {
    Unknown,
    OnChain,
    Fallback,
}

fn log_feed_source(prev: FeedSource, now: FeedSource, f: FeedSnapshot) {
    if prev == now {
        return;
    }
    match now {
        FeedSource::OnChain => eprintln!(
            "[pricing] feed source: ON-CHAIN ProfitabilityOracle::CurrentFeed \
             (token ${:.4}, market ${:.4}/gpu-hr, util {:.1}%)",
            f.token_price_usd,
            f.market_hr,
            f.util * 100.0
        ),
        FeedSource::Fallback => eprintln!(
            "[pricing] feed source: FALLBACK defaults (token ${:.4}, market ${:.4}/gpu-hr, \
             util {:.1}%) — no on-chain oracle feed; run the operator loop so prices are \
             auditable (docs/oracle-feed.md)",
            f.token_price_usd,
            f.market_hr,
            f.util * 100.0
        ),
        FeedSource::Unknown => {}
    }
}

/// Refresh the cached feed every `secs` seconds. A failed read keeps the last
/// good values (never zeroes a price).
///
/// The same tick surfaces any provider whose modelled earning is **below its own
/// declared `owner_cost_usd_hr`**: at the published card it is losing money, and
/// silently hiding that would just relocate the failure to the operator's exit.
fn spawn_feed_refresh(
    client: Arc<ChainClient>,
    rt: Arc<tokio::runtime::Runtime>,
    registry: Arc<Mutex<Vec<Provider>>>,
    pcfg: PricingConfig,
    secs: u64,
) {
    let mut source = FeedSource::Unknown;
    std::thread::spawn(move || loop {
        let mut next_source = FeedSource::Fallback;
        if let Some(f) = rt.block_on(client.get_feed()) {
            let mut g = feed_snapshot().lock().unwrap();
            if f.token_price_usd > 0.0 {
                g.token_price_usd = f.token_price_usd;
            }
            if f.avg_gpu_hour_price_usd > 0.0 {
                g.market_hr = f.avg_gpu_hour_price_usd;
            }
            if f.utilization > 0.0 {
                g.util = f.utilization.clamp(0.05, 1.0);
            }
            // A feed carrying only a token price is still a real feed for the
            // GPN conversion, but a zero market/util value means the operator
            // never posted those fields — report OnChain only when the read
            // actually carried the market anchor.
            next_source = if f.avg_gpu_hour_price_usd > 0.0 {
                FeedSource::OnChain
            } else {
                FeedSource::Fallback
            };
        }
        log_feed_source(source, next_source, feed_now());
        source = next_source;
        if pcfg.model == pricing::PricingModel::Throughput {
            let reg = registry.lock().unwrap().clone();
            for p in reg.iter().filter(|p| p.last_seen.is_some()) {
                // `gflops == 0` means the card has not attested its capability —
                // unmeasured, not slow. Only judge boxes that report real numbers.
                if p.owner_cost_usd_hr <= 0.0 || p.gflops == 0 || p.mem_bw_gbps <= 0.0 {
                    continue;
                }
                let tok_s = roofline_tok_per_s(p.gflops as f64, p.mem_bw_gbps, 1.0);
                let rate = tok_s * 3600.0 * pcfg.k_usd_per_token();
                if rate < p.owner_cost_usd_hr && warn_once(&p.address) {
                    eprintln!(
                        "[pricing] WARNING provider {} earns ${:.4}/hr ({:.1} tok/s on a 1B model at \
                         {} GFLOP/s / {} GB/s) but declares ${:.4}/hr owner cost — it is losing money",
                        p.address,
                        rate,
                        tok_s,
                        p.gflops,
                        p.mem_bw_gbps,
                        p.owner_cost_usd_hr
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_secs(secs.max(5)));
    });
}

/// One warning per provider address per process (the refresher ticks every 30 s).
fn warn_once(addr: &str) -> bool {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap()
        .insert(addr.to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Public rate card served at `GET /pricing`. Derived from the immutable config
/// plus the cached GPN price; contains no secrets and no per-provider identity.
fn pricing_public_json(pcfg: &PricingConfig, f: FeedSnapshot) -> Value {
    let owner_cost = std::env::var("GPN_OWNER_COST_USD_HR")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.05);
    let rows: Vec<Value> = pcfg
        .earnings_table(owner_cost, 1.0)
        .into_iter()
        .map(|r| {
            json!({
                "gpu": r.name,
                "fp32_gflops": r.gflops,
                "mem_bw_gbps": r.mem_bw_gbps,
                "tok_s_1b": (r.tok_s * 10.0).round() / 10.0,
                "usd_per_hour": (r.usd_hr * 10_000.0).round() / 10_000.0,
                "below_owner_cost": r.below_owner_cost
            })
        })
        .collect();
    let out_tok = pcfg.price_out(f.market_hr, pcfg.anchor_tok_s(), f.util);
    let in_tok = out_tok * pcfg.input_rate_frac;
    let mut out = json!({
        "pricing_model": pcfg.model.as_str(),
        "currency": "USD",
        "buyer_price_is_gpu_independent": true,
        "anchor": {
            "description": "4060-class reference card, 1B model, decode is memory-bound",
            "fp32_gflops": pricing::ANCHOR_GFLOPS,
            "mem_bw_gbps": pricing::ANCHOR_MEM_BW_GBPS,
            "params_b": pricing::ANCHOR_PARAMS_B,
            "tok_s": (pcfg.anchor_tok_s() * 10.0).round() / 10.0,
            "usd_per_hour": pcfg.anchor_rate_usd_hr
        },
        "k_usd_per_token": pcfg.k_usd_per_token(),
        "margin_bps": pcfg.margin_bps,
        "input_rate_fraction": pcfg.input_rate_frac,
        "price_per_token": {
            "input": in_tok,
            "output": out_tok
        },
        "price_per_million_tokens": {
            "input": (in_tok * 1e6 * 10_000.0).round() / 10_000.0,
            "output": (out_tok * 1e6 * 10_000.0).round() / 10_000.0
        },
        "provider_earnings_usd_per_hour": rows,
        "owner_cost_usd_per_hour": owner_cost,
        "gpn_price_usd": f.token_price_usd,
        "gpn_per_million_output_tokens":
            (out_tok * 1e6 / f.token_price_usd.max(1e-9) * 100.0).round() / 100.0,
        "note": "Faster GPUs do not raise the buyer's price; they raise the provider's hourly earning. \
                 Provider earnings are pre-margin and before the 5% marketplace fee."
    });
    // ── image / video pricing (same anchor, different work unit) ───────────
    // Diffusion is priced on GPU-seconds (pixels × steps), derived from the same
    // 4060 anchor as the token curve: `secs/3600 × anchor_rate × (1+margin)`.
    // The buyer's price is GPU-independent; a faster card renders in fewer
    // seconds, so it earns the same per image but more per hour.
    let img_jobs: Vec<Value> = [(512u32, 512u32, 8u32), (1024, 1024, 20)]
        .iter()
        .map(|(w, h, steps)| {
            let secs = pcfg.diffusion_seconds(*w as f64, *h as f64, *steps as f64, 1.0);
            let usd = pcfg.diffusion_price_usd(
                *w as f64,
                *h as f64,
                *steps as f64,
                1.0,
                f.market_hr,
                f.util,
            );
            json!({
                "job": format!("{w}x{h}@{steps}steps"),
                "width": w,
                "height": h,
                "steps": steps,
                "estimated_seconds_on_anchor": (secs * 100.0).round() / 100.0,
                "price_usd": (usd * 1e6).round() / 1e6,
                "price_gpn": (usd / f.token_price_usd.max(1e-9) * 1e6).round() / 1e6
            })
        })
        .collect();
    let img_providers: Vec<Value> = pcfg
        .earnings_table(owner_cost, 1.0)
        .into_iter()
        .map(|r| {
            let secs = pcfg.diffusion_provider_seconds(512.0, 512.0, 8.0, 1.0, r.usd_hr);
            json!({
                "gpu": r.name,
                "usd_per_hour": (r.usd_hr * 10_000.0).round() / 10_000.0,
                "seconds_per_512x512_8step": (secs * 10.0).round() / 10.0,
                "images_per_hour": if secs > 0.0 { (3600.0 / secs * 10.0).round() / 10.0 } else { 0.0 },
                "usd_per_image_512x512_8step":
                    (pcfg.diffusion_provider_earn_usd(512.0, 512.0, 8.0, 1.0, r.usd_hr) * 1e6).round() / 1e6
            })
        })
        .collect();
    out["image_pricing"] = json!({
        "unit": "gpu_seconds",
        "buyer_price_is_gpu_independent": true,
        "formula": "seconds = base + (MPx x steps x frames) x sec_per_mpx_step ; usd = seconds/3600 x anchor_usd_per_hour x (1+margin)",
        "calibration": {
            "measured_on": "RTX 4060 8GB (the anchor card), sd-turbo via gpn_diffuse.py, one process per render",
            "base_seconds": pricing::DIFF_BASE_SECS,
            "sec_per_megapixel_step": pricing::DIFF_SEC_PER_MPX_STEP,
            "measured": [
                {"job": "512x512@8", "seconds": 28.88},
                {"job": "512x512@20", "seconds": 36.54},
                {"job": "768x768@12", "seconds": 35.68},
                {"job": "1024x1024@20", "seconds": 44.72}
            ]
        },
        "reference_jobs": img_jobs,
        "provider_earnings_512x512_8step": img_providers,
        "note": "Same image is the same price whichever card renders it. Provider GPU-seconds and images/hour come from the card's curve rate, not from a per-GPU price."
    });
    out["video_pricing"] = json!({
        "status": "blocked",
        "reason": "LTX-Video's T5-XXL text encoder does not fit an 8 GB card; no video job has completed on this fleet.",
        "base_seconds": pricing::DIFF_VIDEO_BASE_SECS,
        "sec_per_megapixel_step": pricing::DIFF_SEC_PER_MPX_STEP * pricing::DIFF_VIDEO_COMPUTE_FACTOR,
        "estimated_seconds_256x256_16f_30steps":
            (pcfg.diffusion_seconds(256.0, 256.0, 30.0, 16.0) * 10.0).round() / 10.0,
        "price_if_requested_usd":
            (pcfg.diffusion_price_usd(256.0, 256.0, 30.0, 16.0, f.market_hr, f.util) * 1e6).round() / 1e6,
        "note": "Priced if requested, but labelled blocked — the constants are estimates, not measurements."
    });
    if pcfg.model == pricing::PricingModel::Market {
        out["market"] = json!({
            "avg_gpu_hour_price_usd": f.market_hr,
            "utilization": f.util,
            "rental_fraction": pcfg.rental_frac,
            "legacy_price_per_million_output":
                pcfg.legacy_price_per_token_out(f.market_hr, pcfg.anchor_tok_s(), f.util) * 1e6
        });
    }
    out
}

/// Legacy single-shot selector. Thin wrapper over [`route_candidates`] so the
/// chat, generation and native-job paths all share ONE filter/ordering rule
/// (queue item #70). Returns `(provider, needs_pull)` for the best candidate.
///
/// Kept (rather than deleted) because it is the most direct expression of "text
/// routing" and the existing routing tests are written against it.
#[allow(dead_code)]
fn pick(
    registry: &[Provider],
    min_gflops: u64,
    min_vram_mb: u64,
    min_ram_mb: u64,
    min_tflops: f64,
    model: &str,
    pull_penalty_ms: u64,
    rtt: &RttOverrides,
) -> Option<(Provider, bool)> {
    let needs = RouteNeeds {
        model: model.to_string(),
        min_gflops,
        min_vram_mb,
        min_ram_mb,
        min_tflops,
        pull_penalty_ms,
    };
    route_candidates(registry, JobKind::Text, &needs, rtt)
        .into_iter()
        .next()
        .map(|c| (c.provider, c.needs_pull))
}

/// One provider an image request may be rendered on, in routing order (item
/// #70): the first that accepts the render serves it, and a provider that
/// refuses before doing any work is skipped for the next one.
#[derive(Clone, Debug)]
struct ImageRoute {
    provider_addr: String,
    status_url: String,
    /// Diffusion channel endpoint (port 8095 by default); `None` = derive as
    /// `status_url + 1` (the LLM provider's own convention).
    channel_endpoint: Option<String>,
}

/// Everything a background render needs. Cloned into the worker thread so the
/// HTTP handler returns immediately.
#[derive(Clone)]
struct ImageCtx {
    hub: Arc<gpn_agent::hub_ap::HubCtx>,
    rt: Arc<tokio::runtime::Runtime>,
    client: Arc<ChainClient>,
    /// Ordered candidates (max [`MAX_ROUTE_ATTEMPTS`] are tried).
    routes: Vec<ImageRoute>,
}

/// Resolve the diffusion channel endpoint for a provider.
///
/// Precedence: an explicit `GPN_IMAGE_CHANNEL_URL` (whole endpoint), then
/// `<host of status_url>:<GPN_IMAGE_CHANNEL_PORT>` (default 8095), then `None`
/// (meaning "derive `status_url + 1`", which is right when LLM and diffusion
/// share a process — not the case here).
fn image_channel_endpoint(status_url: &str) -> Option<String> {
    if let Ok(u) = std::env::var("GPN_IMAGE_CHANNEL_URL") {
        if !u.trim().is_empty() {
            return Some(u);
        }
    }
    if std::env::var("GPN_IMAGE_CHANNEL_DERIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return None;
    }
    let port: u16 = std::env::var("GPN_IMAGE_CHANNEL_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8095);
    let u = status_url.trim_end_matches('/');
    let u = u.strip_suffix("/status").unwrap_or(u);
    let (scheme, rest) = u.split_once("://")?;
    let hostport = rest.split('/').next()?;
    let (host, _) = hostport.rsplit_once(':')?;
    Some(format!("{scheme}://{host}:{port}/channel/job"))
}

/// Fetch one finished artifact from the provider and store it locally.
///
/// Everything is verified: the sha256 the provider signed into its result must
/// match the bytes, and the bytes must start with the PNG signature. A provider
/// that answers with garbage therefore fails the job instead of getting a
/// corrupt "image" published under a public URL.
fn fetch_and_store_artifact(
    route: &ImageRoute,
    rec_id: &str,
    index: u32,
    channel_id: u64,
    job_id: u64,
    meta: &Value,
) -> Result<ImageArtifact, String> {
    let endpoint = route
        .channel_endpoint
        .clone()
        .or_else(|| image_channel_endpoint(&route.status_url))
        .ok_or_else(|| "no channel endpoint for the artifact".to_string())?;
    let base = endpoint
        .strip_suffix("/channel/job")
        .ok_or_else(|| "channel endpoint is not a /channel/job URL".to_string())?;
    let token = gpn_agent::image_channel::artifact_token(channel_id, job_id);
    let url = format!("{base}/channel/artifact/{job_id}?t={token}");
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?
        .get(&url)
        .send()
        .map_err(|e| format!("artifact fetch failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("artifact fetch -> HTTP {}", resp.status().as_u16()));
    }
    const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
    let bytes = resp
        .bytes()
        .map_err(|e| format!("artifact body: {e}"))?
        .to_vec();
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("artifact size {} is out of bounds", bytes.len()));
    }
    if !image_jobs::is_png(&bytes) {
        return Err("provider returned bytes that are not a PNG".to_string());
    }
    let want = meta
        .get("sha256")
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    let got = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        hex::encode(h.finalize())
    };
    if !want.is_empty() && !want.eq_ignore_ascii_case(&got) {
        return Err(format!(
            "artifact sha256 mismatch: provider said {want}, bytes hash to {got}"
        ));
    }
    image_jobs::put_artifact(rec_id, index, &bytes).map_err(|e| format!("store artifact: {e}"))?;
    Ok(ImageArtifact {
        index,
        media_type: meta
            .get("media_type")
            .and_then(|x| x.as_str())
            .unwrap_or("image/png")
            .to_string(),
        sha256: got,
        bytes: bytes.len() as u64,
        width: meta.get("width").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        height: meta.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
    })
}

/// Why an image record did not produce bytes.
///
/// The distinction is what the caller is told: `Unavailable` means the render
/// service could not be reached or had no capacity — a **503 with a retry
/// hint**, not a phantom "failed render".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageFailure {
    /// Nothing is wrong with the request; the service is down/busy. No charge.
    Unavailable,
    /// The request itself could not be rendered (refused, bad artifact, ...).
    Failed,
}

/// Run one image record to completion over the hub channel.
///
/// There is deliberately **no on-chain fallback**. Images used to fall back to
/// a loopback `POST /jobs` on the same access point, which (a) ignored the
/// `ok`/`error` of that response — so a 503 "no compatible provider" surfaced
/// as `"on-chain render did not produce an artifact (status: )"`, a phantom
/// failure that looked like a broken render — and (b) could never help: the
/// on-chain job is claimed by the *same* diffusion provider, on the *same*
/// single GPU, so a channel failure caused by staleness or capacity was
/// guaranteed to fail there too, after up to 420 s of blocking wait. The
/// honest answer is a 503 that says the image service is unavailable.
fn run_image_record(ctx: &ImageCtx, rec: &ImageJob, index: u32, units: u128) -> ImageFailure {
    let gen = GenParams {
        kind: "image".to_string(),
        width: rec.width,
        height: rec.height,
        steps: rec.steps,
        seed: rec.seed,
        frames: 1,
        fps: 1,
        n: 1,
    };
    // Item #70: walk the candidate list. A provider that refuses BEFORE doing any
    // work (`NotAttempted` — no channel endpoint, connect error, 4xx at accept,
    // provider-signed refund) is skipped for the next one; the request only fails
    // once every candidate has refused. An `Ambiguous` failure stops the walk: the
    // provider may hold the AP's pre-signed settle, so a second render could
    // charge the caller twice.
    let mut refusals: Vec<String> = Vec::new();
    let limit = ctx.routes.len().min(MAX_ROUTE_ATTEMPTS);
    for (attempt, route) in ctx.routes.iter().take(MAX_ROUTE_ATTEMPTS).enumerate() {
        match gpn_agent::hub_ap::try_channel_gen_job(
            &ctx.hub,
            &ctx.rt,
            Some(&ctx.client),
            &route.provider_addr,
            &route.status_url,
            route.channel_endpoint.clone(),
            &rec.model,
            &rec.prompt,
            "ImageGeneration",
            gen.clone(),
            units.max(1),
        ) {
            Ok(out) => {
                let parsed: Value = serde_json::from_str(&out.text).unwrap_or(json!({}));
                let empty = vec![];
                let metas = parsed
                    .get("artifacts")
                    .and_then(|a| a.as_array())
                    .unwrap_or(&empty);
                let mut data = Vec::new();
                for (i, meta) in metas.iter().enumerate() {
                    // The provider numbers the artifacts of THIS job from 0; the
                    // caller's slot is `index` (n sequential jobs for n images).
                    let slot = index + i as u32;
                    match fetch_and_store_artifact(
                        route,
                        &rec.id,
                        slot,
                        out.channel_id,
                        out.job_id,
                        meta,
                    ) {
                        Ok(a) => data.push(a),
                        Err(e) => {
                            eprintln!("[images] {}: artifact {slot} rejected: {e}", rec.id);
                            image_jobs::update(&rec.id, |j| {
                                j.status = "failed".into();
                                j.payment = "channel".into();
                                j.channel_id = Some(out.channel_id);
                                j.job_id = Some(out.job_id);
                                j.error = Some(format!(
                                    "render completed but the artifact was unusable: {e}"
                                ));
                            });
                            return ImageFailure::Failed;
                        }
                    }
                }
                if data.is_empty() {
                    image_jobs::update(&rec.id, |j| {
                        j.status = "failed".into();
                        j.payment = "channel".into();
                        j.error = Some("provider returned no artifacts".into());
                    });
                    return ImageFailure::Failed;
                }
                println!(
                    "[images] {} settled on channel {} (job {}) — {} image(s) on {}",
                    rec.id,
                    out.channel_id,
                    out.job_id,
                    data.len(),
                    route.provider_addr
                );
                image_jobs::update(&rec.id, |j| {
                    j.payment = "channel".into();
                    j.channel_id = Some(out.channel_id);
                    j.job_id = Some(out.job_id);
                    for a in data {
                        j.data.retain(|x| x.index != a.index);
                        j.data.push(a);
                    }
                    j.data.sort_by_key(|a| a.index);
                    if j.data.len() as u32 >= j.n {
                        j.status = "completed".into();
                        j.error = None;
                    }
                });
                return ImageFailure::Failed;
            }
            Err(e) if e.fallback_ok() => {
                // Nothing was charged: no value moved off-chain, and images have
                // no on-chain path (see the doc comment above). Try the next
                // candidate (item #70) before telling the caller anything.
                let refused = e.reason().contains("refunded");
                eprintln!(
                    "[images] {}: candidate #{} ({}) refused the render ({}: {}){}",
                    rec.id,
                    attempt + 1,
                    route.provider_addr,
                    if refused {
                        "provider-signed refund"
                    } else {
                        "not attempted"
                    },
                    e.reason(),
                    if attempt + 1 < limit {
                        " — trying the next provider"
                    } else {
                        ""
                    }
                );
                refusals.push(format!("{}: {}", route.provider_addr, e.reason()));
            }
            Err(e) => {
                // The provider may hold the AP's pre-signed settle: the assignment
                // may have been accepted, so this is not a plain "unavailable".
                eprintln!(
                    "[images] {}: ambiguous channel failure on {}: {}",
                    rec.id,
                    route.provider_addr,
                    e.reason()
                );
                image_jobs::update(&rec.id, |j| {
                    j.status = "failed".into();
                    j.payment = "pending".into();
                    j.error = Some(format!(
                        "channel settlement failed ambiguously: {}; not retrying to avoid a double charge",
                        e.reason()
                    ));
                });
                return ImageFailure::Unavailable;
            }
        }
    }
    // Every candidate refused and nothing was paid.
    let detail = if refusals.is_empty() {
        "no provider was routable".to_string()
    } else {
        refusals.join("; ")
    };
    eprintln!(
        "[images] {}: no candidate could take the render ({} attempt(s)): {}",
        rec.id,
        refusals.len(),
        detail
    );
    image_jobs::update(&rec.id, |j| {
        j.status = "failed".into();
        j.payment = "not_charged".into();
        j.error = Some(format!(
            "image generation is temporarily unavailable: every candidate refused ({detail})"
        ));
    });
    ImageFailure::Unavailable
}

/// Read one stored image, if it exists.
fn image_bytes(id: &str, index: u32) -> Option<Vec<u8>> {
    if !image_jobs::valid_id(id) {
        return None;
    }
    std::fs::read(image_jobs::artifact_path(id, index)).ok()
}

/// Send one stored image with its real content type.
fn respond_image(mut req: tiny_http::Request, id: &str, index: u32) -> bool {
    let Some(bytes) = image_bytes(id, index) else {
        return false;
    };
    let ct = Header::from_bytes(&b"Content-Type"[..], &b"image/png"[..]).unwrap();
    let cache = Header::from_bytes(&b"Cache-Control"[..], &b"public, max-age=86400"[..]).unwrap();
    let _ = req.respond(
        Response::from_data(bytes)
            .with_header(ct)
            .with_header(cache),
    );
    true
}

/// Client-side RTT hints from a request body: `{"rtt": {"<addr-or-url>": <ms>}}`.
///
/// The client measures its OWN round-trip to the provider endpoints (from
/// `GET /providers`) and/or the access point (via `GET /ping`) and passes them
/// here so routing reflects the **user→provider** path, not just AP→provider.
/// Absent/malformed hints are ignored → routing falls back to the AP-measured
/// latency (identical to the old behaviour).
fn rtt_overrides(v: &Value) -> RttOverrides {
    v.get("rtt")
        .map(routing::parse_rtt_overrides)
        .unwrap_or_default()
}

/// Rewrite a wildcard bind address (`0.0.0.0:8092`, `[::]:8092`) to its loopback
/// equivalent (`127.0.0.1:8092`, `[::1]:8092`) for use as an advertised callback,
/// so a co-located provider reaches the AP without any public-DNS lookup.
fn loopback_if_wildcard(listen: &str) -> String {
    match listen {
        "0.0.0.0" => "127.0.0.1".to_string(),
        "[::]" | "::" => "[::1]".to_string(),
        _ => {
            if let Some(rest) = listen.strip_prefix("0.0.0.0:") {
                format!("127.0.0.1:{rest}")
            } else if let Some(rest) = listen.strip_prefix("[::]:") {
                format!("[::1]:{rest}")
            } else {
                listen.to_string()
            }
        }
    }
}

/// What kind of work does an ALREADY-CREATED job (`POST /route/{id}`) represent?
///
/// The stored payload kind is authoritative — it is what the provider will read
/// — with the model name as the fallback and text as the default (item #76).
/// Routing this path as text is what made a client-created generation job
/// unroutable: its diffusion model is not a chat tag, so the request was refused
/// as "unknown model" (created, never routed) or handed to a box that could not
/// run it.
fn route_kind(stored_payload_kind: Option<&str>, model: &str) -> JobKind {
    stored_payload_kind
        .and_then(JobKind::from_payload_kind)
        .or_else(|| model_kind(model))
        .unwrap_or(JobKind::Text)
}

/// Write the lease row for a job handed to `provider` (#46 Phase 3 step 2).///
/// This IS the delivery: the access point no longer pushes to a provider's
/// heartbeat. A provider learns about the job from its own `GET /assignments`
/// long-poll within ~250 ms, so a NAT'd/CGNAT box with no inbound port is a
/// first-class provider. The record carries the callback URL + per-job token
/// the provider echoes on `POST /result/{id}`.
///
/// (History: this used to be followed by a `POST /assign` HTTP push; the push
/// path and the heartbeat's `/assign` endpoint were both deleted in Phase 3.)
fn write_assignment(
    store: &str,
    job_id: u64,
    provider: &str,
    model: &str,
    pull: bool,
    callback_url: &str,
    callback_token: &str,
) -> std::io::Result<()> {
    // Cross-process locked read-modify-write: the AP, heartbeat + providers all
    // share `assignments.json`, so an unlocked RMW here could drop another
    // process's just-written assignment (same lost-update class as `LlmStore`).
    let rec = json!({"provider": provider, "model": model, "pull": pull, "callback": callback_url, "callback_token": callback_token});
    gpn_agent::llm::merge_json_object(std::path::Path::new(store), &job_id.to_string(), rec)
}

/// Hand a just-created job to a provider — or refund it (item #76).
///
/// The lease row IS the delivery (#46 Phase 3: the provider learns about the job
/// from its own `GET /assignments` long-poll), so a lease that never landed
/// means the job can never be routed: it would sit `Pending` until its on-chain
/// deadline while the caller was told `ok:true`. That is the "created but never
/// routed" half of item #76 (e.g. gen job #86). So the write is CHECKED, and a
/// failure fails the job on-chain (escrow refunded immediately) and returns the
/// reason to the caller.
fn dispatch_lease(
    store: &str,
    rt: &tokio::runtime::Runtime,
    client: &ChainClient,
    job_id: u64,
    provider: &str,
    model: &str,
    pull: bool,
    callback_url: &str,
    callback_token: &str,
) -> Result<(), String> {
    match write_assignment(
        store,
        job_id,
        provider,
        model,
        pull,
        callback_url,
        callback_token,
    ) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Nothing can ever run this job: refund now (the requestor may fail a
            // Pending job — finding #12) instead of letting it expire.
            let refunded = rt.block_on(client.fail_job_fast(job_id, 0)).is_ok();
            eprintln!(
                "[dispatch] job #{job_id}: could not write the lease for {provider} ({e}) — \
                 not routed ({}); the caller must be told",
                if refunded {
                    "escrow refunded"
                } else {
                    "refund pending; on-chain expiry will reclaim it"
                }
            );
            Err(format!(
                "could not hand the job to a provider (lease write failed: {e}); {}",
                if refunded {
                    "the escrow was refunded and no job was dispatched"
                } else {
                    "no job was dispatched; the on-chain expiry will refund it"
                }
            ))
        }
    }
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

/// Read a raw query-string value (no decoding) from a request URL.
fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let (_, q) = url.split_once('?')?;
    q.split('&').find_map(|kv| match kv.split_once('=') {
        Some((k, v)) if k == key => Some(v),
        _ => None,
    })
}

/// Percent-decode a query value (`%2F`, `%3A`, `+`). Providers send hex/ss58,
/// so this only has to undo what an HTTP client may have escaped.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Does an assignment record's `provider` denote the caller? The AP stores the
/// ss58 form, but a provider may present its hex account id (or vice versa),
/// so compare canonicalised addresses when either side is hex.
fn provider_matches(record: &str, asked: &str) -> bool {
    if record.eq_ignore_ascii_case(asked) {
        return true;
    }
    let canon = |s: &str| {
        if s.starts_with("0x") || s.starts_with("0X") {
            hex_to_ss58(s)
        } else {
            Some(s.to_string())
        }
    };
    match (canon(record), canon(asked)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Max leases returned per `/assignments` response (the provider-side cap is
/// the same; see `gpn_agent::assign_pull::MAX_LEASES`).
const MAX_PULL_LEASES: usize = 20;

/// Build the lease list for one provider: every assignment record for it with a
/// job id above `since`, joined with the AP's own off-chain job payload
/// (prompt/model), which the provider needs in order to RUN the job.
///
/// Ordered by job id ascending and capped, so a provider that has been away
/// catches up over a few polls instead of receiving an unbounded backlog.
fn collect_leases(store: &str, provider: &str, since: u64, limit: usize) -> Vec<Value> {
    let Ok(s) = std::fs::read_to_string(store) else {
        return vec![];
    };
    let Ok(v) = serde_json::from_str::<Value>(&s) else {
        return vec![];
    };
    let Some(map) = v.as_object() else {
        return vec![];
    };
    let mut ids: Vec<u64> = map
        .iter()
        .filter_map(|(k, rec)| {
            let id = k.parse::<u64>().ok()?;
            if id <= since {
                return None;
            }
            let p = rec.get("provider").and_then(|x| x.as_str()).unwrap_or("");
            if p.is_empty() || !provider_matches(p, provider) {
                return None;
            }
            Some(id)
        })
        .collect();
    ids.sort_unstable();
    ids.truncate(limit);
    // Item #91: this response IS the delivery — stamp it so the janitor knows a
    // claim may be in flight before it decides the job is abandoned.
    let now = Instant::now();
    for id in &ids {
        note_lease_delivered(*id, now);
    }
    ids.into_iter()
        .map(|id| {
            let rec = map.get(&id.to_string()).cloned().unwrap_or(json!({}));
            let mut lease = json!({
                "job_id": id,
                "provider": provider,
                "model": rec.get("model").and_then(|x| x.as_str()).unwrap_or(""),
                "pull": rec.get("pull").and_then(|x| x.as_bool()).unwrap_or(false),
                "callback": rec.get("callback").and_then(|x| x.as_str()).unwrap_or(""),
                "callback_token": rec.get("callback_token").and_then(|x| x.as_str()).unwrap_or(""),
                "lease_ms": 60_000,
            });
            if let Some(job) = gpn_agent::llm::get_job(id) {
                if let Ok(jv) = serde_json::to_value(&job) {
                    lease["job"] = jv;
                }
            }
            lease
        })
        .collect()
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
    // When <listen> is a wildcard bind (0.0.0.0/::), advertise the **loopback**
    // address: a co-located provider reaches it without any public-DNS lookup, so
    // an AAAA-laggy host resolver cannot stall the `/result` return
    // (see docs/run-a-node.md, "host-DNS /result defect"). Operators exposing a
    // remote-reachable AP set GPN_PUBLIC_URL/GPN_AP_CALLBACK_URL explicitly.
    let callback_url = opt(&args, "--callback-url")
        .or_else(|| std::env::var("GPN_AP_CALLBACK_URL").ok())
        .or_else(|| std::env::var("GPN_PUBLIC_URL").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("http://{}", loopback_if_wildcard(&listen)));
    // Bearer token for non-loopback callers. Precedence: `GPN_AP_TOKEN` in the
    // environment > `GPN_AP_TOKEN=` in the env file (~/.gpn/accesspoint.env) >
    // generated on first boot and persisted to that file (mode 0600). The
    // generated value is printed exactly once — see src/ap_token.rs. An
    // explicitly supplied token is never overwritten.
    let ap_auth = gpn_agent::ap_token::resolve();
    let ap_token = ap_auth.token.clone();
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
        || std::env::var("GPN_ALLOW_HOT_WALLET")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
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

    // ── pricing (throughput-calibrated by default; market = legacy) ────────
    // Built once: every request reads the same immutable card. The guardrail runs
    // here so a mis-set anchor (an extra zero, say) stops the AP at boot instead
    // of publishing a price nobody intended.
    let pcfg = PricingConfig::from_env((margin * 10_000.0).round() as u32, rental_frac);
    // Validate the price the AP will actually publish: the throughput card, or the
    // legacy market price at the feed's fallback conditions.
    let published_price_pm = match pcfg.model {
        pricing::PricingModel::Throughput => pcfg.price_per_million_out(),
        pricing::PricingModel::Market => pcfg.price_per_million_out_for(
            pcfg.market_hr_fallback,
            pcfg.anchor_tok_s(),
            pcfg.util_fallback,
        ),
    };
    let published = pcfg
        .check_price_usd_per_m(published_price_pm)
        .map_err(|e| format!("pricing refused: {e} (set GPN_PRICING_UNSAFE=1 to override)"))?;
    println!(
        "[pricing] model={} published=${:.4}/1M output anchor=${:.4}/hr anchor_tok_s={:.1} k={:.4e} margin={}bps in_frac={} -> input ${:.4}/1M gpn/tok@$0.15={}",
        pcfg.model.as_str(),
        published,
        pcfg.anchor_rate_usd_hr,
        pcfg.anchor_tok_s(),
        pcfg.k_usd_per_token(),
        pcfg.margin_bps,
        pcfg.input_rate_frac,
        published * pcfg.input_rate_frac,
        pcfg.gpn_per_token(published / 1e6, 0.15),
    );

    // Build the chain client (synchronous runtime for tiny_http handler).
    let rt = Arc::new(tokio::runtime::Runtime::new()?);
    let client = Arc::new(
        rt.block_on(ChainClient::new(&endpoint))?
            .with_signer(&signer_uri),
    );
    spawn_janitor(client.clone(), rt.clone());
    // Image records expire (24 h by default); sweep at boot and hourly. A box
    // that restarts rarely would otherwise accumulate PNGs forever.
    {
        let (r, f) = image_jobs::gc();
        println!("[images] gc at boot: {r} expired record(s), {f} artifact file(s) removed");
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(3600));
            let (r, f) = image_jobs::gc();
            if r > 0 || f > 0 {
                println!("[images] gc: {r} expired record(s), {f} artifact file(s) removed");
            }
        });
    }
    // Item #2: off-chain escrow (payment channels). One channel per provider,
    // opened once; per-job claim+payment are signed messages (zero txs). Any
    // failure falls back to the on-chain job path below.
    let hub = gpn_agent::hub_ap::HubCtx::from_env(&signer_uri);
    gpn_agent::hub_ap::spawn_settler(hub.clone(), rt.clone(), client.clone());
    // Settlement backpressure watch (see `ChainClient::pending_extrinsics`): with
    // on-chain settle as the default every chat is 2–3 AP extrinsics, so a pool
    // that keeps growing is the first sign that settlement has become the
    // bottleneck. Warn only — never shed load on a guess.
    {
        let rt = rt.clone();
        let client = client.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            let warn_at: usize = std::env::var("GPN_PENDING_WARN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(12);
            if let Some(n) = rt.block_on(client.pending_extrinsics()) {
                if n >= warn_at {
                    eprintln!(
                        "[settle] WARNING: {n} extrinsics pending in the pool (threshold {warn_at}) — \
                         on-chain settlement is falling behind; consider GPN_ESCROW_MODE=channel for volume"
                    );
                }
            }
        });
    }

    let registry = Arc::new(Mutex::new(Vec::<Provider>::new()));
    // #46 Phase 3 step 2: capabilities delivered by providers' own pull polls.
    // Kept beside the registry (not in it) because it must survive the 5 s
    // `/status` rebuild, and it is keyed by canonical address.
    let poll_caps: Arc<Mutex<std::collections::HashMap<String, (Instant, Value)>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    // Warm the GPN-price snapshot (chat//jobs convert USD → GPN base units) and
    // keep an eye on providers earning below their own declared cost.
    spawn_feed_refresh(
        client.clone(),
        rt.clone(),
        registry.clone(),
        pcfg.clone(),
        30,
    );
    refresh(
        &registry,
        &Arc::new(urls.clone()),
        client.clone(),
        rt.clone(),
        poll_caps.clone(),
    );
    println!("  on-chain discovery: ENABLED (ComputeMarketplace::ProviderEndpoints)");

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    println!("GPN access point (scheduler) listening on http://{listen}");
    println!("  chain: {endpoint}");
    println!("  providers: {} ({})", urls.len(), urls.join(", "));
    println!("  assign store: {assign_store}");
    println!(
        "  endpoints: /health /ping /providers /jobs /job/{{id}} /mode | POST /jobs /v1/finetunes /route/{{id}} | GET /v1/finetunes/{{id}}"
    );
    println!(
        "  abuse: denylist={} ({} bans) reports={}",
        denylist_path.display(),
        denylist.lock().unwrap().accounts.len(),
        reports_path.display()
    );
    println!(
        "  mode: {} ({})",
        if allow_hot_wallet {
            "TESTNET / free-AI"
        } else {
            "MAINNET / client-signed"
        },
        if allow_hot_wallet {
            "access-point hot wallet pays"
        } else {
            "callers must sign with their OWN wallet"
        }
    );
    gpn_agent::ap_token::announce(&ap_auth);

    let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    // Handle each request on its OWN thread. Previously the loop handled requests
    // sequentially, so a single blocking handler (e.g. POST /jobs doing a chain
    // RPC, or the OpenAI facade waiting for a job to finish) would wedge EVERY
    // other endpoint — including GET /health. Threading keeps the AP responsive.
    for mut req in server.incoming_requests() {
        let rt = rt.clone();
        let client = client.clone();
        let registry = registry.clone();
        let poll_caps = poll_caps.clone();
        let assign_store = assign_store.clone();
        let callback_url = callback_url.clone();
        let hdr = hdr.clone();
        let ap_token = ap_token.clone();
        let listen = listen.clone();
        let denylist = denylist.clone();
        let denylist_path = denylist_path.clone();
        let reports_path = reports_path.clone();
        let hub = hub.clone();
        let pcfg = pcfg.clone();
        std::thread::spawn(move || {
            let url = req.url().to_string();
            let method = req.method().clone();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let path: Vec<&str> = url
                .trim_start_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();

            // Auth gate (non-loopback only).
            if !ap_token.is_empty() {
                let is_loopback = req
                    .remote_addr()
                    .map(|a| a.ip().is_loopback())
                    .unwrap_or(false);
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
                // GET /assignments — the provider-PULL transport (#46 Phase 1).
                // A provider asks for ITS OWN queued assignments instead of waiting
                // for a push to its (possibly un-reachable) heartbeat port, so a
                // NAT'd / CGNAT home node works with no port forward at all.
                // Bounded long-poll: each request is served on its own thread, so
                // holding it up to `wait` ms (max 25 s) costs nothing elsewhere;
                // it returns the moment there is work. A healthy idle provider
                // therefore makes one request per 25 s.
                (Method::Get, [first, ..]) if first.split('?').next() == Some("assignments") => {
                    let is_loopback = req
                        .remote_addr()
                        .map(|a| a.ip().is_loopback())
                        .unwrap_or(false);
                    // Providers hold no user token: bind the endpoint to the same
                    // shared AP↔provider secret the provider's poll presents
                    // (loopback is
                    // exempt so local tests/monitors keep working).
                    let want = std::env::var("GPN_ASSIGN_TOKEN").unwrap_or_default();
                    if !want.is_empty() && !is_loopback {
                        let got = req
                            .headers()
                            .iter()
                            .find(|h| h.field.equiv("X-Gpn-Assign-Token"))
                            .map(|h| h.value.as_str().trim().to_string())
                            .unwrap_or_default();
                        if !ct_eq(&want, &got) {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({"error":"invalid assign token"}).to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(401),
                            );
                            return;
                        }
                    }
                    let provider = query_param(&url, "provider")
                        .map(pct_decode)
                        .unwrap_or_default();
                    if provider.is_empty() {
                        json!({"error": "missing provider"})
                    } else {
                        // #46 Phase 3 step 2: the poll doubles as our heartbeat.
                        // The provider is authenticated (loopback or the shared
                        // token) and sends its own capabilities, so the AP learns
                        // them WITHOUT dialling its `:8093` — the reason a NAT'd
                        // provider could never be routed to before.
                        if let Some(raw) = query_param(&url, "caps") {
                            let decoded = pct_decode(raw);
                            match serde_json::from_str::<Value>(&decoded) {
                                Ok(caps) if caps.is_object() => {
                                    ingest_caps(&poll_caps, &provider, &caps);
                                }
                                Ok(_) => eprintln!(
                                    "[assign] ignoring non-object caps from {}",
                                    pct_decode(&provider)
                                ),
                                Err(e) => eprintln!(
                                    "[assign] ignoring malformed caps from {}: {e}",
                                    pct_decode(&provider)
                                ),
                            }
                        }
                        let since: u64 = query_param(&url, "since")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        let wait_ms: u64 = query_param(&url, "wait")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0)
                            .min(25_000);
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
                        loop {
                            let leases =
                                collect_leases(&assign_store, &provider, since, MAX_PULL_LEASES);
                            if !leases.is_empty() || std::time::Instant::now() >= deadline {
                                let seq = leases
                                    .iter()
                                    .filter_map(|l| l.get("job_id").and_then(|x| x.as_u64()))
                                    .max()
                                    .unwrap_or(since);
                                break json!({"provider": provider, "seq": seq, "leases": leases});
                            }
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                    }
                }
                // GET /pricing — the public rate card. Deliberately no secrets and
                // no chain RPC: it is derived entirely from the immutable config.
                // Cacheable for 60 s so the website can proxy it cheaply.
                (Method::Get, ["pricing"]) => {
                    let f = feed_now();
                    let body = pricing_public_json(&pcfg, f);
                    let cache =
                        Header::from_bytes(&b"Cache-Control"[..], &b"public, max-age=60"[..])
                            .unwrap();
                    let _ = req.respond(
                        Response::from_string(body.to_string())
                            .with_header(hdr.clone())
                            .with_header(cache)
                            .with_status_code(200),
                    );
                    return;
                }
                (Method::Get, ["health"]) => {
                    json!({"status":"ok","providers": registry.lock().unwrap().len(),
                        // Settlement policy, so an operator can verify the env
                        // actually took effect without reading the logs.
                        "settlement": {
                            "mode": hub.policy.mode.as_str(),
                            "channel_enabled": hub.enabled,
                            "settle_min_gpn_base_units": hub.policy.min_gpn.to_string(),
                            "settle_min_gpn": hub.policy.min_gpn as f64
                                / gpn_agent::settle_policy::GPN_UNITS as f64,
                            "channel_ttl_blocks": hub.ttl,
                            "channel_settle_secs": hub.settle_secs,
                            "channel_deposit_base_units": hub.deposit.to_string(),
                            "channel_deposit_gpn": hub.deposit as f64
                                / gpn_agent::settle_policy::GPN_UNITS as f64,
                            "channel_close_at_base_units": hub.close_at.to_string(),
                            "channel_close_at_gpn": hub.close_at as f64
                                / gpn_agent::settle_policy::GPN_UNITS as f64,
                        }
                    })
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
                    "capabilities": p.capabilities,
                    "peer_id": p.peer_id, "multiaddrs": p.p2p_multiaddrs,
                    "p2p_port": p.p2p_port,
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
                    let kind = v
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("account")
                        .to_string();
                    let target = v
                        .get("target")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let category = v
                        .get("category")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                    let reason = v
                        .get("reason")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
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
                            Err(e) => {
                                json!({"ok": false, "error": format!("could not record report: {e}")})
                            }
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
                    let action = v
                        .get("action")
                        .and_then(|x| x.as_str())
                        .unwrap_or("ban")
                        .to_string();
                    let kind = v
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("account")
                        .to_string();
                    let value = v
                        .get("value")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
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
                            Err(e) => {
                                json!({"ok": false, "error": format!("could not persist denylist: {e}")})
                            }
                        }
                    }
                }
                // ── OpenAI-compatible facade ─────────────────────────────────────
                // Item 4: the public catalogue is a shop window. Fine-tune
                // artifacts (`tuned-<job>`), toy models (`stories260K`, <0.3B)
                // and anything unvetted are served internally if asked for by
                // name, but they are not listed here. GPN_PUBLIC_MODELS (CSV)
                // replaces the allowlist when an operator wants to publish more.
                (Method::Get, ["v1", "models"]) => {
                    let reg = registry.lock().unwrap();
                    let ids = gpn_agent::models::filter_public_models(
                        reg.iter().flat_map(|p| p.models.clone()),
                    );
                    let mut ids = ids;
                    ids.sort();
                    ids.dedup();
                    if ids.is_empty() {
                        // Never leave an empty catalogue: clients call
                        // /v1/models before anything else and treat [] as an
                        // outage. Advertise the baseline only when no provider
                        // is online but a provider COULD serve it.
                        ids.push("llama3.2:1b".into());
                    }
                    json!({"object":"list", "data": ids.iter().map(|m| json!({
                    "id": m, "object": "model", "owned_by": "gpn"
                })).collect::<Vec<_>>()})
                }
                (Method::Post, ["v1", "chat", "completions"]) => {
                    let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    let model = v
                        .get("model")
                        .and_then(|x| x.as_str())
                        .unwrap_or("auto")
                        .to_string();
                    let stream = v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false);
                    // Item #63: honour the caller's output allowance instead of a
                    // hard-coded 512 (which also capped the model, not just the bill).
                    let chat_max_tokens = chat_max_tokens(&v);
                    // Item #37: resolve the prompt from OpenAI `messages[]`/`system` **or** a
                    // top-level `prompt`/`input` (llama.cpp / Ollama-style clients pointed at
                    // this facade send those), and refuse a body with no usable prompt BEFORE
                    // any create — otherwise the flat prompt degrades to the bare `"assistant:"`
                    // suffix, a nonsense job is created, routed and paid, and the caller gets
                    // HTTP 200 with an off-topic answer (the item-#24 orphan class, one layer up).
                    let prompt = match resolve_job_prompt(&v) {
                        Some(p) => p,
                        None => {
                            let r = Response::from_string(json!({
                                "error": {"message": "missing prompt: send messages[] (OpenAI) or prompt/input", "type": "gpn_error"}
                            }).to_string())
                            .with_header(hdr.clone())
                            .with_status_code(400);
                            let _ = req.respond(r);
                            return;
                        }
                    };
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
                            // Empty registry = starting up, not "bad model name".
                            let (code, msg) = model_resolution_status(&reg, &e);
                            let kind = if code == 503 {
                                "gpn_unavailable"
                            } else {
                                "gpn_error"
                            };
                            let mut r = Response::from_string(
                                json!({"error": {"message": msg, "type": kind}}).to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(code);
                            if code == 503 {
                                r = r.with_header(retry_after_header());
                            }
                            let _ = req.respond(r);
                            return;
                        }
                    };
                    // One shared candidate pipeline for every AI request (item
                    // #70); the first entry is what the on-chain/passthrough path
                    // uses, the whole list feeds the channel failover below.
                    let needs = RouteNeeds {
                        model: model.clone(),
                        pull_penalty_ms,
                        ..Default::default()
                    };
                    let candidates =
                        route_candidates(&reg, JobKind::Text, &needs, &rtt_overrides(&v));
                    let (provider, needs_pull) = match candidates.first() {
                        Some(c) => (c.provider.clone(), c.needs_pull),
                        None => {
                            let r = Response::from_string(json!({
                            "error": {"message": "no compatible provider online", "type": "gpn_error"}
                        }).to_string()).with_header(hdr.clone()).with_status_code(503);
                            let _ = req.respond(r);
                            return;
                        }
                    };
                    // ── payment policy ─────────────────────────────────────
                    // The *client* may name a mode (`channel` / `settle` /
                    // `onchain` / `passthrough`); otherwise the AP's value-based
                    // policy decides once the price is known (below). Channels
                    // exist between this AP and one provider, so batching is the
                    // AP operator's call, not the end user's.
                    let explicit_mode = v
                        .get("mode")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_ascii_lowercase());
                    let mut text = String::new();
                    let mut usage: Option<(u32, u32)> = None;
                    let mut channel_meta: Option<(u64, u64, u128, u128, u128)> = None;
                    // ── rate card for this request ─────────────────────────
                    // Throughput model: ONE global buyer price per token, converted
                    // from USD at the cached GPN price. Market model (legacy): the
                    // old fleet-average market rental. Input is billed at
                    // `GPN_INPUT_RATE_FRAC` × the output rate, and escrow covers
                    // `prompt_tokens×frac + max_tokens` output-equivalents.
                    let feed = feed_now();
                    let prompt_tokens = gpn_agent::pricing::estimate_prompt_tokens(&prompt);
                    let usd_per_token_out = match pcfg.model {
                        pricing::PricingModel::Throughput => pcfg.price_per_token_out(),
                        pricing::PricingModel::Market => {
                            let params_b = params_b_for(&model);
                            let reg = registry.lock().unwrap().clone();
                            let live: Vec<&Provider> =
                                reg.iter().filter(|p| p.last_seen.is_some()).collect();
                            let tok_per_s = if live.is_empty() {
                                roofline_tok_per_s(165.0, 250.0, params_b)
                            } else {
                                live.iter()
                                    .map(|p| {
                                        roofline_tok_per_s(p.gflops as f64, p.mem_bw_gbps, params_b)
                                    })
                                    .sum::<f64>()
                                    / live.len() as f64
                            };
                            pcfg.legacy_price_per_token_out(feed.market_hr, tok_per_s, feed.util)
                        }
                    };
                    let chat_price_per_token =
                        pcfg.gpn_per_token(usd_per_token_out, feed.token_price_usd);
                    let escrow_units = pcfg.escrow_token_units(prompt_tokens, chat_max_tokens);
                    // Single-rate pallet approximation for the on-chain path; the
                    // channel path carries the ceiling as an exact amount.
                    let (onchain_rate, onchain_cap) = pcfg.onchain_rate_and_cap(
                        chat_price_per_token,
                        prompt_tokens,
                        chat_max_tokens,
                    );
                    // ── value-based routing ────────────────────────────────────
                    // Job value = the escrow ceiling the request would lock: the
                    // thing we are about to be paid for. (The final charge is
                    // unknown until the model finishes, so the quote is the only
                    // honest input at decision time.)
                    let price = chat_price_per_token.saturating_mul(escrow_units as u128);
                    let (route, why) = hub.policy.decide(explicit_mode.as_deref(), price);
                    if routing_debug() {
                        eprintln!(
                            "[routing] {} (value={} base units, threshold={}, mode={}, channel={}) → {}",
                            why.as_str(),
                            price,
                            hub.policy.min_gpn,
                            hub.policy.mode.as_str(),
                            if hub.enabled { "yes" } else { "no" },
                            route.as_str()
                        );
                    }
                    let passthrough = route == gpn_agent::settle_policy::Route::Passthrough;
                    // ── off-chain escrow (channel) fast path ───────────────
                    // The AP signs a channel state update, the provider runs the
                    // job and returns a signed result; zero extrinsics and zero
                    // block waits. Falls through to the on-chain path ONLY when
                    // nothing could have been paid (`NotAttempted`); an
                    // ambiguous failure (assignment possibly accepted) must not
                    // retry on-chain or the caller would pay twice.
                    if route == gpn_agent::settle_policy::Route::Channel {
                        // Item #63: hand the provider the per-token rates so it
                        // settles at `min(actual usage, price)` and the unused
                        // ceiling is never charged. `price` stays the hard cap.
                        let billing = gpn_agent::hub_channel::BillingRates {
                            in_rate: (chat_price_per_token as f64
                                * pcfg.input_rate_frac.clamp(0.0, 1.0))
                            .round() as u128,
                            out_rate: chat_price_per_token,
                        };
                        let _t0 = std::time::Instant::now();
                        let outcome = failover("chat", &candidates, |cand| {
                            gpn_agent::hub_ap::try_channel_job_billed_ex(
                                &hub,
                                &rt,
                                Some(&client),
                                &cand.address,
                                &cand.status_url,
                                &model,
                                &prompt,
                                chat_max_tokens,
                                price,
                                Some(billing),
                            )
                        });
                        match outcome {
                            Failover::Served {
                                value: out,
                                attempted,
                                ..
                            } => {
                                println!(
                                    "[chat] channel settle: job {} on channel {} in {} ms (attempt {})",
                                    out.job_id,
                                    out.channel_id,
                                    _t0.elapsed().as_millis(),
                                    attempted
                                );
                                text = out.text;
                                usage = out.usage;
                                channel_meta = Some((
                                    out.channel_id,
                                    out.job_id,
                                    out.balance_provider,
                                    out.charge,
                                    price,
                                ));
                            }
                            Failover::Exhausted { reasons } => {
                                // Every candidate refused before doing any work,
                                // so nothing was paid. Same fallback as before:
                                // the on-chain escrow path (which routes to the
                                // first candidate) is still a valid attempt.
                                eprintln!(
                                    "[chat] every channel candidate refused ({}); falling back to on-chain escrow: {}",
                                    reasons.len(),
                                    reasons.join("; ")
                                );
                            }
                            Failover::Ambiguous { reason, attempted } => {
                                let r = Response::from_string(
                                    json!({
                                        "error": {
                                            "message": format!("channel settlement failed ambiguously after {attempted} attempt(s): {reason}; not retrying on-chain to avoid a double charge"),
                                            "type": "gpn_error"
                                        }
                                    })
                                    .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(502);
                                let _ = req.respond(r);
                                return;
                            }
                        }
                    }
                    let channel_done = channel_meta.is_some();
                    // Fast AP path (GPN_FAST_AP, default ON): report the predicted id as soon
                    // as the pool accepts (no block wait). With the flag off we still submit
                    // through the same stream (one nonce authority) but wait for inclusion and
                    // return the id the chain actually assigned.
                    let fast_ap = fast_ap_enabled();
                    // Preserve the create failure reason instead of discarding it.
                    let mut create_err: Option<String> = None;
                    let (created, id) = if passthrough || channel_done {
                        (false, channel_meta.map(|c| c.1).unwrap_or(0u64))
                    } else {
                        // create + route (hot wallet pays). Id comes from a process-local
                        // allocator so concurrent requests never predict the same job id.
                        // `_create_guard` serializes the create so exactly one is in flight.
                        let _create_guard = CREATE_LOCK.lock().unwrap();
                        // NextJobId is the id the create will take. An UNREADABLE counter must
                        // never be treated as 0: the pre-fix silent 0 allocated job id 0,
                        // reported `next: 0` on a chain with ~6000 jobs, and mislabelled the
                        // caller's job (change-log 2026-10-20 15:1x). Refuse instead.
                        let next_before = match rt.block_on(client.try_get_next_job_id()) {
                            Some(n) => n,
                            None => {
                                let r = Response::from_string(
                                    json!({"error": {"message": "chain unavailable: cannot read NextJobId", "type": "gpn_error"}})
                                        .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(503);
                                let _ = req.respond(r);
                                return;
                            }
                        };
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
                        // Serialized by `_create_guard`: exactly ONE create is in flight at a
                        // time, so the process-local prediction IS the real on-chain id (the AP
                        // is the sole creator; the chain assigns ids in submit order).
                        let outcome = submit_create(&rt, &client, next_before, fast_ap, || {
                            rt.block_on(client.create_job_fast(
                                8192u32,
                                75u16,
                                1u8,
                                0u32,
                                150u64,
                                "LLMInference",
                                "Spot",
                                onchain_rate.saturating_mul(onchain_cap as u128),
                                onchain_rate,
                                onchain_cap,
                                1000,
                                workload_hash,
                            ))
                        });
                        if let Some(e) = outcome.err {
                            create_err = Some(e);
                        }
                        (outcome.created, outcome.id)
                    };
                    if channel_done {
                        // Answer, usage and the off-chain job id were produced by
                        // the channel path above — nothing left to settle here.
                    } else if passthrough {
                        // Direct provider call, no chain. Derive the off-chain infer
                        // endpoint from the provider's status_url (:8093 → :8094).
                        let infer_url = format!(
                            "{}/infer",
                            provider.status_url.replacen(":8093", ":8094", 1)
                        );
                        let body = json!({"model": model, "prompt": prompt, "temperature": 0.7, "max_tokens": chat_max_tokens});
                        text = match reqwest::blocking::Client::new()
                            .post(&infer_url)
                            .json(&body)
                            .send()
                            .and_then(|r| r.json::<serde_json::Value>())
                        {
                            Ok(j) if j.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => j
                                .get("text")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            Ok(j) => format!(
                                "[gpn error] {}",
                                j.get("error")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("inference failed")
                            ),
                            Err(e) => format!("[gpn error] {e}"),
                        };
                    } else if created {
                        record_created(id, chat_max_tokens as u64);
                        let job =
                            gpn_agent::llm::LlmJob::new_llm(&model, &prompt, 0.7, chat_max_tokens);
                        let store = gpn_agent::llm::LlmStore::new();
                        if !persist_job_payload(&store, id, &job) {
                            // Same rule as POST /jobs: never advertise a job whose payload
                            // never landed (the provider would skip it and it would sit
                            // until expiry). Refund, skip the lease, tell the caller.
                            let refunded = rt.block_on(client.fail_job_fast(id, 0)).is_ok();
                            eprintln!(
                                "[store] chat job #{id}: payload not persisted — not dispatched ({})",
                                if refunded { "escrow refunded" } else { "refund pending; on-chain expiry will reclaim it" }
                            );
                            text = format!(
                                "[gpn error] could not persist the job payload; the job was not dispatched (job #{id})"
                            );
                        } else {
                            let callback_token = new_callback_token(id);
                            let dispatched = dispatch_lease(
                                &assign_store,
                                &rt,
                                &client,
                                id,
                                &provider.address,
                                &model,
                                needs_pull,
                                &callback_url,
                                &callback_token,
                            );
                            // #46 Phase 3: delivery is the provider's pull long-poll;
                            // the lease row above is the only hand-off.
                            match dispatched {
                                // Item #76: never report a job as dispatched when the
                                // lease never landed — say what happened instead, and
                                // do not sit in the completion wait for work that was
                                // never handed over.
                                Err(e) => text = format!("[gpn error] {e} (job #{id})"),
                                // block until the provider finishes (OpenAI calls are synchronous)
                                Ok(()) => {
                                    for _ in 0..1200 {
                                        if let Some(j) = gpn_agent::llm::get_job(id) {
                                            if j.status == "done" {
                                                text = j.response.clone().unwrap_or_default();
                                                if let (Some(p), Some(c)) =
                                                    (j.prompt_tokens, j.completion_tokens)
                                                {
                                                    usage = Some((p, c));
                                                }
                                                break;
                                            }
                                            if j.status == "error" {
                                                text = format!(
                                                    "[gpn error] {}",
                                                    j.error.clone().unwrap_or_default()
                                                );
                                                break;
                                            }
                                        }
                                        std::thread::sleep(Duration::from_millis(300));
                                    }
                                }
                            }
                        }
                    } else {
                        text = match &create_err {
                            Some(e) => format!("[gpn error] could not create job: {e}"),
                            None => "[gpn error] could not create job".to_string(),
                        };
                    }
                    if stream {
                        let idc = format!("chatcmpl-gpn-{id}");
                        let created = now_secs();
                        let chunk1 = json!({"id":idc,"object":"chat.completion.chunk","created":created,"model":model,
                        "choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":serde_json::Value::Null}]});
                        let chunk2 = json!({"id":idc,"object":"chat.completion.chunk","created":created,"model":model,
                        "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
                        let sse = format!("data: {chunk1}\n\ndata: {chunk2}\n\ndata: [DONE]\n\n");
                        let ct =
                            Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..])
                                .unwrap();
                        let cc =
                            Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..]).unwrap();
                        let r = Response::from_string(sse).with_header(ct).with_header(cc);
                        let _ = req.respond(r);
                        return;
                    }
                    json!({
                        "id": format!("chatcmpl-gpn-{id}"), "object": "chat.completion", "created": now_secs(),
                        "model": model,
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                        "usage": usage_json(usage),
                        // How this request was paid. `channel` = off-chain signed
                        // settlement (no tx); `onchain` = per-request escrow cycle;
                        // `passthrough` = unbilled. Clients may pin `"mode"`.
                        "gpn": match channel_meta {
                            Some((channel_id, job_id, balance, charge, ceiling)) => json!({
                                "payment": "channel",
                                "channel_id": channel_id,
                                "job_id": job_id.to_string(),
                                "channel_balance_provider": balance.to_string(),
                                // Item #63: what this job actually cost, which is
                                // `<= channel_charge_ceiling` when the provider
                                // reports usage.
                                "channel_charge": charge.to_string(),
                                "channel_charge_ceiling": ceiling.to_string(),
                            }),
                            None if passthrough => json!({"payment": "passthrough"}),
                            None => json!({"payment": "onchain", "job_id": id.to_string()}),
                        }
                    })
                }
                // ── public image generation (OpenAI-compatible) ─────────────────
                // `POST /v1/images/generations` answers immediately with 202 and
                // a poll URL. A render is 29–45 s today and grows with size and
                // steps, so holding the connection would collide with the
                // Worker's 110 s upstream limit; `"sync": true` is allowed only
                // when the estimate fits a 60 s budget.
                (Method::Post, ["v1", "images", "generations"]) => {
                    if let Some(ip) = req.remote_addr().map(|a| a.ip().to_string()) {
                        if denylist.lock().unwrap().is_ip_banned(&ip) {
                            let _ = req.respond(
                                Response::from_string(json!({
                                    "error": {"message": "service refused (operator denylist)", "type": "gpn_policy_violation"}
                                }).to_string())
                                .with_header(hdr.clone())
                                .with_status_code(403),
                            );
                            return;
                        }
                    }
                    if !allow_hot_wallet {
                        let _ = req.respond(
                            Response::from_string(json!({
                                "error": {"message": "public generation is disabled on this access point (mainnet)", "type": "gpn_error"}
                            }).to_string())
                            .with_header(hdr.clone())
                            .with_status_code(403),
                        );
                        return;
                    }
                    let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    let prompt = match v.get("prompt").and_then(|x| x.as_str()) {
                        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
                        _ => {
                            let _ = req.respond(
                                Response::from_string(json!({
                                    "error": {"message": "missing prompt: send {\"prompt\": \"...\"}", "type": "gpn_bad_request"}
                                }).to_string())
                                .with_header(hdr.clone())
                                .with_status_code(400),
                            );
                            return;
                        }
                    };
                    if let Some(cat) = moderation_block(&prompt) {
                        let _ = req.respond(
                            Response::from_string(json!({
                                "error": {"message": cat.reason(), "type": "gpn_policy_violation", "category": cat.id()}
                            }).to_string())
                            .with_header(hdr.clone())
                            .with_status_code(451),
                        );
                        return;
                    }
                    let model = v
                        .get("model")
                        .and_then(|x| x.as_str())
                        .unwrap_or("sd-turbo")
                        .to_string();
                    let size = v
                        .get("size")
                        .and_then(|x| x.as_str())
                        .unwrap_or("512x512")
                        .to_string();
                    let (width, height) = match image_jobs::parse_size(&size) {
                        Ok(wh) => wh,
                        Err(e) => {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({
                                        "error": {"message": e, "type": "gpn_bad_request"}
                                    })
                                    .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(400),
                            );
                            return;
                        }
                    };
                    let steps = v
                        .get("steps")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(8)
                        .clamp(1, 50) as u32;
                    let n = v.get("n").and_then(|x| x.as_u64()).unwrap_or(1).clamp(1, 4) as u32;
                    let seed = v.get("seed").and_then(|x| x.as_i64()).unwrap_or(0);
                    let b64 = v
                        .get("response_format")
                        .and_then(|x| x.as_str())
                        .map(|f| f.eq_ignore_ascii_case("b64_json"))
                        .unwrap_or(false);
                    let sync = v.get("sync").and_then(|x| x.as_bool()).unwrap_or(false);
                    let est = image_estimate_seconds(&pcfg, width, height, steps, n);
                    if sync {
                        if let Some(msg) = image_jobs::sync_refusal(est) {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({
                                        "error": {
                                            "message": msg,
                                            "type": "gpn_sync_too_slow",
                                            "estimated_seconds": (est * 100.0).round() / 100.0
                                        }
                                    })
                                    .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(400),
                            );
                            return;
                        }
                    }
                    // Route to a box that advertises ImageGeneration, using the
                    // SAME candidate pipeline as a chat request (item #70).
                    let reg = registry.lock().unwrap().clone();
                    let needs = RouteNeeds {
                        model: model.clone(),
                        ..Default::default()
                    };
                    let candidates =
                        route_candidates(&reg, JobKind::Image, &needs, &rtt_overrides(&v));
                    if candidates.is_empty() {
                        let empty = reg.iter().all(|p| p.last_seen.is_none());
                        let mut r = Response::from_string(json!({
                            "error": {
                                "message": if empty {
                                    "provider registry is warming up (no providers polled yet); retry shortly".to_string()
                                } else {
                                    "no provider with the ImageGeneration capability is online".to_string()
                                },
                                "type": "gpn_unavailable"
                            }
                        }).to_string())
                        .with_header(hdr.clone())
                        .with_status_code(503);
                        if empty {
                            r = r.with_header(retry_after_header());
                        }
                        let _ = req.respond(r);
                        return;
                    }
                    // The render itself only happens over the hub channel. Probe
                    // each candidate's render endpoint BEFORE accepting the
                    // request and queue on the first box that answers: a caller
                    // gets an immediate, honest 503 (listing why every candidate
                    // was skipped) instead of a `202` that silently dies 30 s later.
                    let mut skipped: Vec<String> = Vec::new();
                    let mut routes: Vec<ImageRoute> = Vec::new();
                    for cand in candidates.iter().take(MAX_ROUTE_ATTEMPTS) {
                        let endpoint = image_channel_endpoint(&cand.provider.status_url);
                        let reachable = endpoint
                            .as_deref()
                            .map(gpn_agent::hub_ap::probe_channel_endpoint)
                            .unwrap_or(false);
                        if reachable {
                            routes.push(ImageRoute {
                                provider_addr: cand.provider.address.clone(),
                                status_url: cand.provider.status_url.clone(),
                                channel_endpoint: endpoint,
                            });
                        } else {
                            let ep = endpoint
                                .clone()
                                .unwrap_or_else(|| "<unresolved>".to_string());
                            eprintln!(
                                "[images] candidate {} does not serve the channel render \
                                 endpoint ({ep}) — trying the next provider",
                                cand.provider.address
                            );
                            skipped.push(format!(
                                "{}: no channel render endpoint at {ep}",
                                cand.provider.address
                            ));
                        }
                    }
                    if routes.is_empty() {
                        let msg = format!(
                            "image service unavailable: no diffusion provider serves the channel \
                             render endpoint. No image was queued and nothing was charged; retry \
                             when a diffusion provider is online. Candidates: {}",
                            skipped.join("; ")
                        );
                        eprintln!("[images] refusing generation request: {msg}");
                        let _ = req.respond(
                            Response::from_string(
                                json!({
                                    "error": {
                                        "message": msg,
                                        "type": "gpn_unavailable",
                                        "candidates": skipped
                                    }
                                })
                                .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_header(retry_after_header())
                            .with_status_code(503),
                        );
                        return;
                    }
                    let feed = feed_now();
                    let price_gpn = image_price_gpn(
                        &pcfg,
                        width as f64,
                        height as f64,
                        steps as f64,
                        1.0,
                        feed.token_price_usd,
                        feed.market_hr,
                        feed.util,
                    ) * n as f64;
                    let id = image_jobs::new_id(&prompt);
                    let rec = ImageJob::new(
                        id.clone(),
                        &prompt,
                        &model,
                        width,
                        height,
                        steps,
                        seed,
                        n,
                        est,
                        price_gpn,
                    );
                    if let Err(e) = image_jobs::save(&rec) {
                        let _ = req.respond(
                            Response::from_string(json!({
                                "error": {"message": format!("cannot persist the job: {e}"), "type": "gpn_error"}
                            }).to_string())
                            .with_header(hdr.clone())
                            .with_status_code(500),
                        );
                        return;
                    }
                    let ctx = ImageCtx {
                        hub: hub.clone(),
                        rt: rt.clone(),
                        client: client.clone(),
                        routes,
                    };
                    // Per-image price: `n` renders are `n` sequential channel
                    // jobs (one GPU, one job at a time), each priced exactly
                    // like a single image.
                    let per_image_units =
                        (price_gpn / n as f64 * pricing::BASE_UNITS_PER_GPN) as u128;
                    if sync {
                        let started = Instant::now();
                        let mut failure = ImageFailure::Failed;
                        for i in 0..n {
                            failure = run_image_record(&ctx, &rec, i, per_image_units.max(1));
                            if image_jobs::get(&id)
                                .map(|j| j.status == "failed")
                                .unwrap_or(false)
                            {
                                break;
                            }
                        }
                        let done = image_jobs::get(&id).unwrap_or(rec);
                        println!(
                            "[images] {} sync request finished in {} ms (status {})",
                            id,
                            started.elapsed().as_millis(),
                            done.status
                        );
                        let code = if done.status == "completed" {
                            200
                        } else if failure == ImageFailure::Unavailable {
                            // Nothing was charged — say the service is down.
                            503
                        } else {
                            502
                        };
                        let mut r = Response::from_string(done.public_json(b64).to_string())
                            .with_header(hdr.clone())
                            .with_status_code(code);
                        if code == 503 {
                            r = r.with_header(retry_after_header());
                        }
                        let _ = req.respond(r);
                        return;
                    }
                    let rec_worker = rec.clone();
                    std::thread::spawn(move || {
                        for i in 0..n {
                            let _ = run_image_record(&ctx, &rec_worker, i, per_image_units.max(1));
                            if image_jobs::get(&id)
                                .map(|j| j.status == "failed")
                                .unwrap_or(false)
                            {
                                break;
                            }
                        }
                    });
                    let mut pending = rec.pending_shape();
                    pending["payment_hint"] = json!("channel");
                    pending["n"] = json!(n);
                    pending["gpn"]["price_total_gpn"] = json!(price_gpn);
                    let _ = req.respond(
                        Response::from_string(pending.to_string())
                            .with_header(hdr.clone())
                            .with_status_code(202),
                    );
                    return;
                }
                (Method::Get, ["v1", "images", "generations", id]) => {
                    let want_b64 = url.contains("format=b64_json") || url.contains("b64=true");
                    match image_jobs::get(id) {
                        Some(j) => {
                            // A render that never ran (service unavailable) is a
                            // 503, not a silent 200 "failed": the caller must be
                            // able to tell "your image failed" from "we are down".
                            let unavailable = j.status == "failed" && j.payment == "not_charged";
                            let mut r = Response::from_string(j.public_json(want_b64).to_string())
                                .with_header(hdr.clone());
                            if unavailable {
                                r = r.with_header(retry_after_header()).with_status_code(503);
                            }
                            let _ = req.respond(r);
                        }
                        None => {
                            let _ = req.respond(
                                Response::from_string(json!({
                                    "error": {"message": "unknown or expired image job id", "type": "gpn_not_found"}
                                }).to_string())
                                .with_header(hdr.clone())
                                .with_status_code(404),
                            );
                        }
                    }
                    return;
                }
                // The finished bytes. `GET /v1/images/{id}` is the URL every
                // completed job advertises; `/{id}/{n}` addresses the 2nd+ image
                // of an `n>1` request.
                (Method::Get, ["v1", "images", id]) => {
                    if image_bytes(id, 0).is_none() {
                        let _ = req.respond(
                        Response::from_string(
                            json!({"error": {"message": "no such image (unknown id, not finished, or expired)", "type": "gpn_not_found"}})
                                .to_string(),
                        )
                        .with_header(hdr.clone())
                        .with_status_code(404),
                        );
                        return;
                    }
                    respond_image(req, id, 0);
                    return;
                }
                (Method::Get, ["v1", "images", id, idx]) => {
                    let i: u32 = idx.parse().unwrap_or(0);
                    if image_bytes(id, i).is_none() {
                        let _ = req.respond(
                        Response::from_string(
                            json!({"error": {"message": "no such image", "type": "gpn_not_found"}})
                                .to_string(),
                        )
                        .with_header(hdr.clone())
                        .with_status_code(404),
                        );
                        return;
                    }
                    respond_image(req, id, i);
                    return;
                }
                // ── public fine-tuning (queue item #67): submit ──────────────────
                // `POST /v1/finetunes` answers 202 + a poll URL, because a LoRA
                // tune is minutes of GPU (the Worker upstream limit is 110 s).
                (Method::Post, ["v1", "finetunes"]) => {
                    if let Some(ip) = req.remote_addr().map(|a| a.ip().to_string()) {
                        if denylist.lock().unwrap().is_ip_banned(&ip) {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({"error": {"message": "service refused (operator denylist)", "type": "gpn_policy_violation"}})
                                        .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(403),
                            );
                            return;
                        }
                    }
                    if !allow_hot_wallet {
                        let _ = req.respond(
                            Response::from_string(
                                json!({"error": {"message": "public fine-tuning is disabled on this access point (mainnet)", "type": "gpn_error"}})
                                    .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(403),
                        );
                        return;
                    }
                    let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    let (model, dataset_json, lora_r, epochs) = match parse_finetune_request(&v) {
                        Ok(x) => x,
                        Err(e) => {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({"error": {"message": e, "type": "gpn_bad_request"}})
                                        .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(400),
                            );
                            return;
                        }
                    };
                    // Same guardrail as every other job entry point: the dataset
                    // text is user content, so it is moderated BEFORE any escrow.
                    if let Some(cat) = moderation_block(&dataset_json) {
                        let _ = req.respond(
                            Response::from_string(
                                json!({"error": {"message": cat.reason(), "type": "gpn_policy_violation", "category": cat.id()}})
                                    .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(451),
                        );
                        return;
                    }
                    // A fine-tune must land on a box that advertises the FineTuning
                    // capability and can serve the base model (item #70's one pipeline).
                    let reg = registry.lock().unwrap().clone();
                    let needs = RouteNeeds {
                        model: model.clone(),
                        ..Default::default()
                    };
                    let candidates =
                        route_candidates(&reg, JobKind::Finetune, &needs, &rtt_overrides(&v));
                    let provider = match candidates.first() {
                        Some(c) => c.provider.clone(),
                        None => {
                            let empty = reg.iter().all(|p| p.last_seen.is_none());
                            let mut r = Response::from_string(
                                json!({"error": {
                                    "message": if empty {
                                        "provider registry is warming up (no providers polled yet); retry shortly".to_string()
                                    } else {
                                        format!("no provider with the FineTuning capability can serve model '{model}' (or pull it)")
                                    },
                                    "type": "gpn_unavailable"
                                }})
                                .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(503);
                            if empty {
                                r = r.with_header(retry_after_header());
                            }
                            let _ = req.respond(r);
                            return;
                        }
                    };
                    // Flat job price (a tune is GPU-minutes, not tokens). The
                    // on-chain single-rate fields stay 0: the executor reports 0
                    // tokens, so the escrow is returned after completion.
                    let price_gpn = v
                        .get("price_gpn")
                        .and_then(|x| x.as_f64())
                        .filter(|p| *p > 0.0)
                        .unwrap_or(50.0);
                    let next_before = match rt.block_on(client.try_get_next_job_id()) {
                        Some(n) => n,
                        None => {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({"error": {"message": "chain unavailable: cannot read NextJobId — refusing to create a job with an unknown id", "type": "gpn_error"}})
                                        .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(503),
                            );
                            return;
                        }
                    };
                    // Opaque workload hash (never the dataset — privacy).
                    let workload_hash = {
                        use sha2::{Digest, Sha256};
                        let mut h = Sha256::new();
                        h.update(dataset_json.as_bytes());
                        h.update(std::process::id().to_le_bytes());
                        h.update(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_nanos())
                                .unwrap_or(0)
                                .to_le_bytes(),
                        );
                        let mut o = [0u8; 32];
                        o.copy_from_slice(&h.finalize());
                        o[0] |= 1;
                        o
                    };
                    let escrow = (price_gpn * 1e12) as u128;
                    let fast_ap = fast_ap_enabled();
                    let _create_guard = CREATE_LOCK.lock().unwrap();
                    let outcome = submit_create(&rt, &client, next_before, fast_ap, || {
                        rt.block_on(client.create_job_fast(
                            0,
                            75,
                            1,
                            0,
                            150,
                            "FineTuning",
                            "Spot",
                            escrow,
                            0,
                            0,
                            1000,
                            workload_hash,
                        ))
                    });
                    drop(_create_guard);
                    if !outcome.created {
                        let msg = outcome
                            .err
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "could not create job".to_string());
                        let _ = req.respond(
                            Response::from_string(
                                json!({"error": {"message": msg, "type": "gpn_error"}}).to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(502),
                        );
                        return;
                    }
                    let id = outcome.id;
                    let job =
                        gpn_agent::llm::LlmJob::new_finetune(&model, &dataset_json, lora_r, epochs);
                    let store = gpn_agent::llm::LlmStore::new();
                    if !persist_job_payload(&store, id, &job) {
                        // Same rule as every other create: never advertise an
                        // unrunnable job — refund and say so.
                        let refunded = rt.block_on(client.fail_job_fast(id, 0)).is_ok();
                        eprintln!(
                            "[store] finetune #{id}: payload not persisted — not dispatched ({})",
                            if refunded {
                                "escrow refunded"
                            } else {
                                "refund pending; on-chain expiry will reclaim it"
                            }
                        );
                        let _ = req.respond(
                            Response::from_string(
                                json!({
                                    "error": {"message": "could not persist the job payload to the off-chain store; the job was not dispatched", "type": "gpn_error"},
                                    "job_id": id,
                                    "refunded": refunded
                                })
                                .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(500),
                        );
                        return;
                    }
                    record_created(id, 0);
                    let callback_token = new_callback_token(id);
                    // Item #76: a lease that never landed means the tune can never be
                    // routed — refund it and say so instead of returning 202.
                    if let Err(e) = dispatch_lease(
                        &assign_store,
                        &rt,
                        &client,
                        id,
                        &provider.address,
                        &model,
                        false,
                        &callback_url,
                        &callback_token,
                    ) {
                        let _ = req.respond(
                            Response::from_string(
                                json!({
                                    "error": {"message": e, "type": "gpn_error"},
                                    "job_id": id
                                })
                                .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(500),
                        );
                        return;
                    }
                    let _ = req.respond(
                        Response::from_string(
                            json!({
                                "id": id.to_string(),
                                "object": "finetune",
                                "status": "queued",
                                "created": now_secs(),
                                "model": model,
                                "tuned_model": format!("tuned-{id}"),
                                "lora_r": lora_r,
                                "epochs": epochs,
                                "examples": serde_json::from_str::<Value>(&dataset_json)
                                    .ok()
                                    .and_then(|x| x.as_array().map(|a| a.len()))
                                    .unwrap_or(0),
                                "routed_to": provider.address,
                                "price_gpn": price_gpn,
                                "poll_url": format!("/v1/finetunes/{id}")
                            })
                            .to_string(),
                        )
                        .with_header(hdr.clone())
                        .with_status_code(202),
                    );
                    return;
                }
                // ── public fine-tuning (queue item #67): poll ───────────────────
                (Method::Get, ["v1", "finetunes", id]) => {
                    let idn: u64 = match id.parse() {
                        Ok(n) => n,
                        Err(_) => {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({"error": {"message": "invalid fine-tune id", "type": "gpn_bad_request"}})
                                        .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(400),
                            );
                            return;
                        }
                    };
                    let off = gpn_agent::llm::get_job(idn).filter(|j| j.kind == "finetune");
                    let chain = rt.block_on(client.get_job_state(idn));
                    let chain_state = chain.as_ref().map(|(s, _, _, _)| s.clone());
                    if off.is_none() && chain.is_none() {
                        if create_is_pending(idn) {
                            let _ = req.respond(
                                Response::from_string(
                                    json!({"id": idn.to_string(), "object": "finetune", "status": "queued", "pending_inclusion": true})
                                        .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(202),
                            );
                            return;
                        }
                        let _ = req.respond(
                            Response::from_string(
                                json!({"error": {"message": "unknown fine-tune id", "type": "gpn_not_found"}})
                                    .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(404),
                        );
                        return;
                    }
                    let status = finetune_public_status(
                        off.as_ref().map(|j| j.status.as_str()),
                        chain_state.as_deref(),
                    );
                    let provider = chain.as_ref().and_then(|(_, _, _, p)| p.clone());
                    let mut out = json!({
                        "id": idn.to_string(),
                        "object": "finetune",
                        "status": status,
                        "model": off.as_ref().map(|j| j.model.clone()).unwrap_or_default(),
                        "chain_state": chain_state,
                        "provider": provider
                    });
                    if let Some(j) = off {
                        out["lora_r"] = json!(j.lora_r);
                        out["epochs"] = json!(j.epochs);
                        out["result_hash"] = json!(j.result_hash);
                        out["artifact_path"] = json!(j.artifact_path);
                        out["artifact_media_type"] = json!(j.artifact_media_type);
                        out["error"] = json!(j.error);
                    }
                    if status == "succeeded" {
                        // What the provider registers on completion
                        // (`spawn_servemodel`), so a caller can immediately use it
                        // as any other model in `/v1/chat/completions`.
                        out["tuned_model"] = json!(format!("tuned-{idn}"));
                    }
                    let _ = req
                        .respond(Response::from_string(out.to_string()).with_header(hdr.clone()));
                    return;
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
                    let model = v
                        .get("model")
                        .and_then(|x| x.as_str())
                        .unwrap_or("auto")
                        .to_string();
                    let wait = v.get("wait").and_then(|x| x.as_bool()).unwrap_or(false);
                    let wait_secs: u64 = v.get("wait_secs").and_then(|x| x.as_u64()).unwrap_or(180);
                    // per-token billing (optional): rate × max_tokens = escrow ceiling.
                    let ppt_in: u128 = v
                        .get("price_per_token")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0) as u128;
                    let mtok: u32 =
                        v.get("max_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    // Oracle + fleet cost economics → GPN per token.
                    //   market  = oracle avg_gpu_hour_price (per-class market rental)
                    //   target  = clamp(rental_frac × market, owner_cost, market)
                    //   tok/s   = fleet-average measured throughput
                    //   $/token = target_hr / (tok/s × 3600 × util) × (1 + margin)
                    //   gpn/tok = $/token ÷ token_price_usd
                    let feed = feed_now();
                    let (token_price_usd, market_hr, util) =
                        (feed.token_price_usd, feed.market_hr, feed.util);
                    let params_b = params_b_for(&model);
                    let (tok_per_s, _owner_hr) = {
                        let reg = registry.lock().unwrap().clone();
                        let live: Vec<&Provider> =
                            reg.iter().filter(|p| p.last_seen.is_some()).collect();
                        if live.is_empty() {
                            (roofline_tok_per_s(165.0, 250.0, params_b), 0.03_f64)
                        } else {
                            let t = live
                                .iter()
                                .map(|p| {
                                    roofline_tok_per_s(p.gflops as f64, p.mem_bw_gbps, params_b)
                                })
                                .sum::<f64>()
                                / live.len() as f64;
                            let o = live
                                .iter()
                                .map(|p| p.owner_cost_usd_hr)
                                .fold(f64::MAX, f64::min);
                            (t.max(1.0), o.max(0.001))
                        }
                    };
                    // Throughput model (default): one global buyer price, converted to
                    // GPN at the cached token price. Market model: the legacy
                    // fleet-average rental anchored on the oracle.
                    let usd_per_token = match pcfg.model {
                        pricing::PricingModel::Throughput => pcfg.price_per_token_out(),
                        pricing::PricingModel::Market => {
                            pcfg.legacy_price_per_token_out(market_hr, tok_per_s, util)
                        }
                    };
                    let oracle_ppt: u128 = pcfg.gpn_per_token(usd_per_token, token_price_usd);
                    let ppt: u128 = if ppt_in > 0 { ppt_in } else { oracle_ppt };
                    let mtok_eff: u32 = if mtok > 0 { mtok } else { 256 };
                    // `text_rate`/`text_cap` (the on-chain single-rate approximation of
                    // two-rate billing) are computed after `prompt` is resolved, below.
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
                    // Item #76: ONE classification, decided BEFORE any escrow. An
                    // unknown `job_type` is a 4xx (it used to be silently downgraded
                    // to text), and a generation model wins over the `LLMInference`
                    // default, so a caller who omits `job_type` can no longer create a
                    // *text* job out of an image/video model — which is what sent a
                    // diffusion job to the LLM box, where it was skipped forever and
                    // sat `Running` until the on-chain deadline.
                    let kind = match admit(v.get("job_type").and_then(|x| x.as_str()), &model) {
                        Ok(k) => k,
                        Err(e) => {
                            eprintln!("[jobs] refused: {}", e.message());
                            let _ = req.respond(
                                Response::from_string(
                                    json!({"ok": false, "error": e.message()}).to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(400),
                            );
                            return;
                        }
                    };
                    // The name written on chain is the kind's own, so chain state, the
                    // off-chain payload and the routing decision can never disagree.
                    let job_type = kind.onchain_name();
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

                    // Input metering: the on-chain pallet stores ONE rate, so publish a
                    // blended rate over `prompt+max` that reproduces two-rate billing at
                    // full completion (see `PricingConfig::onchain_rate_and_cap`). A
                    // caller-supplied `price_per_token` keeps its old semantics.
                    let (text_rate, text_cap) = if ppt_in > 0 {
                        (ppt, mtok_eff)
                    } else {
                        pcfg.onchain_rate_and_cap(
                            ppt,
                            prompt
                                .as_deref()
                                .map(gpn_agent::pricing::estimate_prompt_tokens)
                                .unwrap_or(1),
                            mtok_eff,
                        )
                    };

                    // choose the best provider (item #70: ONE shared pipeline for
                    // every job type). Generation jobs (image/video) are served by
                    // the diffusion provider and ML/fine-tune jobs by a box that
                    // advertises that capability — the LLM model registry doesn't
                    // list their models, so DON'T validate them here. Reject
                    // unknown models only for LLM/text jobs (prevents orphaned
                    // Pending jobs).
                    let jt_lc = job_type.to_ascii_lowercase();
                    let is_gen = kind.is_generation();
                    let reg = registry.lock().unwrap().clone();
                    let model = if kind.is_generation() {
                        model
                    } else {
                        match resolve_requested_model(&reg, &model) {
                            Ok(m) => m,
                            Err(e) => {
                                // Same startup race as the chat facade: 503 while no
                                // provider has polled yet, 400 once the registry is real.
                                let (code, msg) = model_resolution_status(&reg, &e);
                                let mut r = Response::from_string(
                                    json!({"ok":false,"error":msg}).to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(code);
                                if code == 503 {
                                    r = r.with_header(retry_after_header());
                                }
                                let _ = req.respond(r);
                                return;
                            }
                        }
                    };
                    // One shared candidate pipeline for every job type (item
                    // #70): text gates on the model, generation/ML/fine-tune gate
                    // on the advertised capability (`gpn_agent::jobkind::admit`).
                    let needs = RouteNeeds {
                        model: model.clone(),
                        min_gflops,
                        min_vram_mb,
                        min_ram_mb,
                        min_tflops,
                        pull_penalty_ms,
                    };
                    let candidates = route_candidates(&reg, kind, &needs, &rtt_overrides(&v));
                    let (provider, needs_pull) = match candidates.first() {
                        Some(c) => (c.provider.clone(), c.needs_pull),
                        None => {
                            let r = Response::from_string(
                                json!({"ok":false,"error":"no compatible provider available"})
                                    .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(503);
                            let _ = req.respond(r);
                            return;
                        }
                    };

                    // ── item #2: off-chain escrow (channel) fast path ────────
                    // Route the job to the provider with signed messages only —
                    // no `create_job`/`claim`/`complete`. Falls through to the
                    // on-chain path on ANY failure (safe fallback), so enabling
                    // the mode cannot make a job unservable.
                    if !is_gen {
                        let prompt_s = prompt.clone().unwrap_or_default();
                        let price = ppt.saturating_mul(mtok_eff as u128).max(1);
                        // Item #63: the caller's own rate card, so a provider
                        // settles at actual usage instead of the ceiling. Only
                        // meaningful when the caller actually priced per token.
                        let billing = (ppt > 0).then_some(gpn_agent::hub_channel::BillingRates {
                            in_rate: ppt,
                            out_rate: ppt,
                        });
                        match failover("jobs", &candidates, |cand| {
                            gpn_agent::hub_ap::try_channel_job_billed_ex(
                                &hub,
                                &rt,
                                Some(&client),
                                &cand.address,
                                &cand.status_url,
                                &model,
                                &prompt_s,
                                mtok_eff,
                                price,
                                billing,
                            )
                        }) {
                            Failover::Served {
                                value: out,
                                provider: served,
                                needs_pull: served_pull,
                                attempted,
                            } => {
                                let mut resp = json!({
                                    "ok": true,
                                    "escrow": "channel",
                                    "channel_id": out.channel_id,
                                    "job_id": out.job_id,
                                    "routed_to": served.address,
                                    "model": model,
                                    "pull": served_pull,
                                    "status_url": served.status_url,
                                    "latency_ms": served.latency_ms,
                                    "load": format!("{}/{}", served.active, served.capacity),
                                    "attempts": attempted,
                                    "channel_balance_provider": out.balance_provider.to_string(),
                                    "channel_charge": out.charge.to_string(),
                                    "channel_charge_ceiling": price.to_string(),
                                    "answer": {
                                        "status": "done",
                                        "response": out.text,
                                        "off_chain": true,
                                    },
                                });
                                resp["usage"] = usage_json(out.usage);
                                let r = Response::from_string(resp.to_string())
                                    .with_header(hdr.clone());
                                let _ = req.respond(r);
                                return;
                            }
                            Failover::Exhausted { reasons } => {
                                // Nothing was paid by any candidate: the on-chain
                                // escrow path (first candidate) is still valid.
                                eprintln!(
                                    "[jobs] every channel candidate refused ({}); falling back to on-chain escrow: {}",
                                    reasons.len(),
                                    reasons.join("; ")
                                );
                            }
                            Failover::Ambiguous { reason, attempted } => {
                                // The assignment may have been accepted: paying
                                // on-chain too would charge the caller twice.
                                let r = Response::from_string(
                                    json!({
                                        "ok": false,
                                        "error": format!("channel settlement failed ambiguously after {attempted} attempt(s): {reason}; not retrying on-chain to avoid a double charge")
                                    })
                                    .to_string(),
                                )
                                .with_header(hdr.clone())
                                .with_status_code(502);
                                let _ = req.respond(r);
                                return;
                            }
                        }
                    }

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
                    let next_before = match rt.block_on(client.try_get_next_job_id()) {
                        Some(n) => n,
                        None => {
                            // Never create against an unknown counter (see the
                            // /v1/chat/completions guard): a silent 0 hands out id 0.
                            let r = Response::from_string(
                                json!({"ok":false,"error":"chain unavailable: cannot read NextJobId — refusing to create a job with an unknown id"})
                                    .to_string(),
                            )
                            .with_header(hdr.clone())
                            .with_status_code(503);
                            let _ = req.respond(r);
                            return;
                        }
                    };
                    // Job-relative pricing for generation jobs (pixels × steps), grounded
                    // in the oracle market GPU price; text jobs price per-token (ppt).
                    let price_gpn = if is_gen {
                        match v.get("price_gpn").and_then(|x| x.as_f64()) {
                            Some(p) if p > 0.0 => p, // explicit override
                            _ => image_price_gpn(
                                &pcfg,
                                g("width", 512) as f64,
                                g("height", 512) as f64,
                                g("steps", 8) as f64,
                                // A video job's denoise cost scales with frames;
                                // an image is one frame. `min_vram_mb`/`min_tflops`
                                // routing already sends video to big-VRAM boxes.
                                if jt_lc.contains("video") {
                                    g("frames", 16).max(1) as f64
                                } else {
                                    1.0
                                },
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
                    } else if text_rate > 0 && text_cap > 0 {
                        (
                            text_rate.saturating_mul(text_cap as u128),
                            text_rate,
                            text_cap,
                        )
                    } else {
                        ((price_gpn * 1e12) as u128, 0u128, 0u32)
                    };
                    // Price preview (`"dry_run": true`): quote the created job exactly as
                    // it would be escrowed — price, escrow and the model behind it —
                    // and return WITHOUT creating anything on-chain and WITHOUT
                    // routing a render. This is the only way to inspect a generation
                    // price from the public path (a real image job costs a GPU minute).
                    if v.get("dry_run").and_then(|x| x.as_bool()).unwrap_or(false) {
                        let mut q = json!({
                            "ok": true,
                            "dry_run": true,
                            "job_type": job_type,
                            "is_generation": is_gen,
                            "escrow_base_units": escrow.to_string(),
                            "price_gpn": price_gpn,
                            "token_price_usd": token_price_usd,
                            "price_usd": price_gpn * token_price_usd,
                            "pricing_model": pcfg.model.as_str()
                        });
                        if is_gen {
                            let frames = if jt_lc.contains("video") {
                                g("frames", 16).max(1) as f64
                            } else {
                                1.0
                            };
                            let secs = pcfg.diffusion_seconds(
                                g("width", 512) as f64,
                                g("height", 512) as f64,
                                g("steps", 8) as f64,
                                frames,
                            );
                            q["image"] = json!({
                                "width": g("width", 512),
                                "height": g("height", 512),
                                "steps": g("steps", 8),
                                "frames": frames as u32,
                                "estimated_seconds_on_anchor": (secs * 100.0).round() / 100.0,
                                "calibration": {
                                    "base_seconds": pricing::DIFF_BASE_SECS,
                                    "sec_per_megapixel_step": if frames > 1.0 {
                                        pricing::DIFF_SEC_PER_MPX_STEP * pricing::DIFF_VIDEO_COMPUTE_FACTOR
                                    } else {
                                        pricing::DIFF_SEC_PER_MPX_STEP
                                    }
                                },
                                "buyer_price_is_gpu_independent": true
                            });
                        }
                        let _ = req
                            .respond(Response::from_string(q.to_string()).with_header(hdr.clone()));
                        return;
                    }
                    // ONE nonce authority: create through the shared fast stream (the same one
                    // the chat path + janitor use) instead of a stop-and-wait `submit_signed` that
                    // grabbed a nonce straight from chain state — the two allocators used to fight
                    // over the hot wallet's nonces ("Transaction is outdated" / "Priority is too
                    // low"), dropping creates outright. With `GPN_FAST_AP` on we also drop the
                    // ~1-block (~15 s, observed up to 27 s) create stall from `POST /jobs`.
                    let fast_ap = fast_ap_enabled();
                    let _create_guard = CREATE_LOCK.lock().unwrap();
                    let outcome = submit_create(&rt, &client, next_before, fast_ap, || {
                        rt.block_on(client.create_job_fast(
                            min_vram_mb as u32,
                            g("min_compute_capability", 75) as u16,
                            g("min_gpu_count", 1) as u8,
                            g("min_cuda_cores", 0) as u32,
                            min_gflops,
                            job_type,
                            "Spot",
                            escrow,
                            job_ppt,
                            job_mtok,
                            1000,
                            workload_hash,
                        ))
                    });
                    drop(_create_guard);
                    match outcome.created {
                        true => {
                            let id = outcome.id;
                            // store the off-chain payload (prompt/model/params) if present.
                            // Generation jobs (image/video) MUST be stored with a gen
                            // kind, otherwise the LLM provider claims them and 404s on
                            // a diffusion model; the diffusion provider only claims
                            // jobs where `kind` is image/video.
                            if let Some(p) = prompt.as_ref() {
                                // Item #76: the payload kind comes from the SAME
                                // classification that chose the provider (`kind`), not
                                // from a fourth private sniff of the model name. A
                                // payload whose kind disagrees with the box it was
                                // routed to is a job nobody can run.
                                let job = match kind {
                                    JobKind::Image | JobKind::Video => {
                                        let neg = v.get("negative_prompt").and_then(|x| x.as_str());
                                        gpn_agent::llm::LlmJob::new_gen(
                                            kind.payload_kind(),
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
                                    }
                                    JobKind::Ml => {
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
                                    }
                                    // `POST /jobs` has no fine-tune body shape (that
                                    // lives on `/v1/finetunes`); a FineTuning kind is
                                    // still admitted above and recorded as text here.
                                    JobKind::Text | JobKind::Finetune => {
                                        gpn_agent::llm::LlmJob::new_llm(&model, p, 0.7, mtok_eff)
                                    }
                                };
                                let store = gpn_agent::llm::LlmStore::new();
                                if !persist_job_payload(&store, id, &job) {
                                    // No payload ⇒ nobody can ever run this job. Refund the
                                    // escrow now (requestor may fail a Pending job, finding
                                    // #12) and refuse to advertise it, instead of returning a
                                    // fake `ok:true` for a job that can only expire (#41).
                                    let refunded = rt.block_on(client.fail_job_fast(id, 0)).is_ok();
                                    eprintln!(
                                        "[store] job #{id}: payload not persisted — not dispatched ({})",
                                        if refunded { "escrow refunded" } else { "refund pending; on-chain expiry will reclaim it" }
                                    );
                                    let r = Response::from_string(
                                        json!({
                                            "ok": false,
                                            "job_id": id,
                                            "refunded": refunded,
                                            "error": "could not persist the job payload to the off-chain store; the job was not dispatched"
                                        })
                                        .to_string(),
                                    )
                                    .with_header(hdr.clone())
                                    .with_status_code(500);
                                    let _ = req.respond(r);
                                    return;
                                }
                            }
                            // dispatch: record which provider (by address) should claim it,
                            // plus the model + whether it must pull the model first.
                            record_created(id, mtok_eff as u64);
                            let callback_token = new_callback_token(id);
                            // Item #76: the lease row is the delivery. If it cannot be
                            // written the job can never be routed, so refund it and
                            // answer with the truth rather than `ok:true`.
                            if let Err(e) = dispatch_lease(
                                &assign_store,
                                &rt,
                                &client,
                                id,
                                &provider.address,
                                &model,
                                needs_pull,
                                &callback_url,
                                &callback_token,
                            ) {
                                let _ = req.respond(
                                    Response::from_string(
                                        json!({"ok": false, "job_id": id, "error": e}).to_string(),
                                    )
                                    .with_header(hdr.clone())
                                    .with_status_code(500),
                                );
                                return;
                            }
                            // #46 Phase 3: delivery is the provider's pull long-poll;
                            // the lease row above is the only hand-off.
                            // optional inline wait: poll the off-chain store and return the
                            // answer in THIS response (no separate /result call).
                            let mut answer: Option<serde_json::Value> = None;
                            if wait {
                                let deadline = std::time::Instant::now()
                                    + Duration::from_secs(wait_secs.min(600));
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
                        false => match outcome.err {
                            Some(e) => json!({"ok":false,"error":e.to_string()}),
                            None => json!({"ok":false,"error":"could not create job"}),
                        },
                    }
                }
                (Method::Get, ["jobs"]) => {
                    // `next: 0` on a chain with history is the fingerprint of an RPC
                    // outage (the counter is never reset). Say so explicitly instead of
                    // rendering an empty marketplace as fact.
                    match rt.block_on(client.try_get_next_job_id()) {
                        Some(next) => {
                            let start = next.saturating_sub(JOBS_LIST_WINDOW);
                            let mut jobs = vec![];
                            for id in start..next {
                                if let Some((state, _p, _r, prov)) =
                                    rt.block_on(client.get_job_state(id))
                                {
                                    jobs.push(json!({"id":id,"state":state,"provider":prov}));
                                }
                            }
                            json!({"jobs":jobs, "from":start, "next":next})
                        }
                        None => json!({
                            "jobs": [],
                            "from": 0,
                            "next": 0,
                            "chain_unavailable": true,
                            "error": "cannot read ComputeMarketplace::NextJobId (node/RPC unavailable)"
                        }),
                    }
                }
                (Method::Get, ["job", id]) => {
                    let idn: u64 = id.parse().unwrap_or(0);
                    match rt.block_on(client.get_job_state(idn)) {
                        Some((state, price, req_, prov)) => {
                            clear_create_pending(idn);
                            json!({"id":idn,"state":state,"price_base":price.to_string(),"requestor":req_,"provider":prov})
                        }
                        // The create is in the pool but not yet in a block: report the
                        // real state instead of a confusing 404 (the caller got this id
                        // from POST /jobs seconds ago).
                        None if create_is_pending(idn) => json!({
                            "id": idn,
                            "state": "Pending",
                            "pending_inclusion": true,
                            "note": "create submitted; awaiting block inclusion"
                        }),
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
                    let is_loopback = req
                        .remote_addr()
                        .map(|a| a.ip().is_loopback())
                        .unwrap_or(false);
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
                                    Response::from_string(
                                        json!({"error":"invalid callback token"}).to_string(),
                                    )
                                    .with_header(hdr.clone())
                                    .with_status_code(401),
                                );
                                return;
                            }
                        }
                    }
                    let status = v.get("status").and_then(|x| x.as_str()).unwrap_or("done");
                    let store = gpn_agent::llm::LlmStore::new();
                    // Item #53: the provider's real token counts, carried into the AP's
                    // own record so a REMOTE provider's usage is as truthful as a
                    // local one's. Absent ⇒ left unset (0 in the OpenAI `usage`
                    // block), never invented. Recorded in the SAME write that marks
                    // the job done — a separate write is a race against this AP's own
                    // chat waiter, which reads on `status == "done"`.
                    let pt = v.get("prompt_tokens").and_then(|x| x.as_u64());
                    let ctk = v.get("completion_tokens").and_then(|x| x.as_u64());
                    let usage = match (pt, ctk) {
                        (None, None) => None,
                        (p, c) => Some((p.unwrap_or(0) as u32, c.unwrap_or(0) as u32)),
                    };
                    let res = if status == "error" {
                        store.set_error(
                            idn,
                            v.get("error")
                                .and_then(|x| x.as_str())
                                .unwrap_or("remote inference error"),
                        )
                    } else if let Some(ap) = v.get("artifact_path").and_then(|x| x.as_str()) {
                        store.complete_artifact(
                            idn,
                            ap,
                            v.get("artifact_media_type")
                                .and_then(|x| x.as_str())
                                .unwrap_or("application/octet-stream"),
                            v.get("result_hash").and_then(|x| x.as_str()).unwrap_or(""),
                        )
                    } else {
                        store.complete_with_usage(
                            idn,
                            v.get("response").and_then(|x| x.as_str()).unwrap_or(""),
                            v.get("result_hash").and_then(|x| x.as_str()).unwrap_or(""),
                            v.get("salt").and_then(|x| x.as_str()),
                            usage,
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
                                let _ = req.respond(
                                    Response::from_data(bytes).with_header(ct).with_header(cd),
                                );
                            }
                            Err(e) => {
                                let _ = req.respond(
                                    Response::from_string(
                                        json!({"error": e.to_string()}).to_string(),
                                    )
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
                            let ct =
                                Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                                    .unwrap_or_else(|_| hdr.clone());
                            let cd = Header::from_bytes(
                                &b"Content-Disposition"[..],
                                &b"attachment; filename=\"gpn-testnet-spec.json\""[..],
                            )
                            .unwrap_or_else(|_| hdr.clone());
                            let _ = req.respond(
                                Response::from_data(bytes).with_header(ct).with_header(cd),
                            );
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
                            req_acct
                                .as_deref()
                                .map(|a| d.is_account_banned(a))
                                .unwrap_or(false)
                                || prov_acct
                                    .as_deref()
                                    .map(|a| d.is_provider_banned(a))
                                    .unwrap_or(false)
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
                        // Item #76: route by the job's OWN kind. `/route/{id}` used to
                        // assume text, so a generation job created by a client was
                        // validated against the LLM catalogue (refused as "unknown
                        // model" — created, never routed) or handed to a chat box that
                        // could not run it. The stored payload is the authority; the
                        // caller's `job` body and the model name are the fallbacks.
                        let stored_kind =
                            gpn_agent::llm::get_job(idn).map(|j| j.kind).or_else(|| {
                                route_body
                                    .get("job")
                                    .and_then(|j| j.get("kind"))
                                    .and_then(|k| k.as_str())
                                    .map(str::to_string)
                            });
                        let kind = route_kind(stored_kind.as_deref(), &qmodel);
                        // A generation model is not an Ollama tag: resolving it against
                        // the chat catalogue is what made these jobs unroutable.
                        let resolved = if kind.is_generation() {
                            Ok(qmodel)
                        } else {
                            resolve_requested_model(&reg, &qmodel)
                        };
                        match resolved {
                            Err(e) => {
                                // Route path keeps its `ok:false` shape but must not
                                // claim "unknown model" when nobody has polled yet.
                                let (_, msg) = model_resolution_status(&reg, &e);
                                json!({"ok":false,"error":msg})
                            }
                            Ok(qmodel) => {
                                // Same shared candidate pipeline as every other AI
                                // request (item #70).
                                let needs = RouteNeeds {
                                    model: qmodel.clone(),
                                    pull_penalty_ms,
                                    ..Default::default()
                                };
                                let cands = route_candidates(
                                    &reg,
                                    kind,
                                    &needs,
                                    &rtt_overrides(&route_body),
                                );
                                match cands.first() {
                                    Some(c) => {
                                        let p = c.provider.clone();
                                        let pull = c.needs_pull;
                                        // Client-signed path: the prompt lives only in the
                                        // caller's body. If they included the off-chain job
                                        // (item #14) persist + push it, so a REMOTE provider
                                        // can run it; otherwise a remote provider will claim
                                        // but find no local prompt.
                                        let job_payload = route_body.get("job").and_then(|jv| {
                                            serde_json::from_value::<gpn_agent::llm::LlmJob>(
                                                jv.clone(),
                                            )
                                            .ok()
                                        });
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
                                        let mut payload_ok = true;
                                        if let Some(j) = &job_payload {
                                            let store = gpn_agent::llm::LlmStore::new();
                                            payload_ok = persist_job_payload(&store, idn, j);
                                        }
                                        if !payload_ok {
                                            // Advertise nothing: a lease for a job whose payload
                                            // never landed can only be skipped by the provider
                                            // (#41) and left to expire. The client keeps its own
                                            // on-chain job and may retry the route call.
                                            eprintln!(
                                            "[store] route job #{idn}: payload not persisted — not advertised"
                                        );
                                            json!({"ok": false, "job_id": idn,
                                            "error": "could not persist the job payload to the off-chain store; the job was not advertised"})
                                        } else {
                                            let callback_token = new_callback_token(idn);
                                            // Item #76: this path is where a client-created
                                            // job used to be "created but never routed" — the
                                            // lease failure was discarded, so the caller saw
                                            // `ok:true` for a job no provider would ever learn
                                            // about. Refund + report instead.
                                            match dispatch_lease(
                                                &assign_store,
                                                &rt,
                                                &client,
                                                idn,
                                                &p.address,
                                                &qmodel,
                                                pull,
                                                &callback_url,
                                                &callback_token,
                                            ) {
                                                Err(e) => json!({"ok": false, "job_id": idn,
                                                    "error": e}),
                                                // #46 Phase 3: no push — the provider's pull
                                                // long-poll delivers the lease written above.
                                                Ok(()) => {
                                                    json!({"ok":true,"job_id":idn,"routed_to":p.address,
                                       "model":qmodel,"pull":pull,
                                       "latency_ms":p.latency_ms,"load":format!("{}/{}",p.active,p.capacity)})
                                                }
                                            }
                                        }
                                    }
                                    None => {
                                        json!({"ok":false,"error":"no compatible provider available"})
                                    }
                                }
                            }
                        }
                    }
                }
                _ => json!({"error":"not found"}),
            };

            let code = if resp.get("error").map(|v| !v.is_null()).unwrap_or(false) {
                404
            } else {
                200
            };
            let _ = req.respond(
                Response::from_string(resp.to_string())
                    .with_header(hdr.clone())
                    .with_status_code(code),
            );
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Item #53: a completed chat job reports the backend's real token counts in
    /// the OpenAI `usage` block instead of the hard-coded zeros every client saw
    /// before, and an unreported count stays 0 rather than being invented.
    #[test]
    fn chat_usage_reports_backend_tokens_and_totals_them() {
        let reported = usage_json(Some((17, 42)));
        assert_eq!(reported["prompt_tokens"], json!(17));
        assert_eq!(reported["completion_tokens"], json!(42));
        assert_eq!(reported["total_tokens"], json!(59));

        // No backend numbers (artifact jobs, older provider builds) => zeros,
        // not a guess, and not a panic on overflow-prone input.
        let unknown = usage_json(None);
        assert_eq!(unknown["prompt_tokens"], json!(0));
        assert_eq!(unknown["completion_tokens"], json!(0));
        assert_eq!(unknown["total_tokens"], json!(0));

        let huge = usage_json(Some((u32::MAX, u32::MAX)));
        assert_eq!(
            huge["total_tokens"],
            json!(u32::MAX),
            "saturates, never wraps"
        );
    }

    /// Peer-id publication: a heartbeat that publishes a peer id + public p2p
    /// multiaddr is relayed, but every string is validated first — the AP is a
    /// public directory and must not become a relay for arbitrary text.
    #[test]
    fn p2p_fields_are_whitelisted_before_relay() {
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        let good = format!("/ip4/203.0.113.7/tcp/30333/p2p/{pid}");
        let v = json!({
            "peer_id": pid,
            "p2p_port": 30333,
            "multiaddrs": [
                good,
                // duplicate — collapsed
                good,
                // somebody else's peer id
                "/ip4/203.0.113.9/tcp/30333/p2p/12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw",
                // no peer id -> not dialable
                "/ip4/203.0.113.9/tcp/30333",
                // not a multiaddr at all
                "http://203.0.113.9:8093/status",
                // unknown protocol
                "/ip4/203.0.113.9/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo/http/evil",
            ],
        });
        let (peer_id, mas, port) = p2p_fields(&v);
        assert_eq!(peer_id.as_deref(), Some(pid));
        assert_eq!(mas, vec![good.clone()]);
        assert_eq!(port, Some(30333));

        let p = provider_from_caps("5Abc", &v, "http://203.0.113.7:8093/status".into(), 5);
        assert_eq!(p.peer_id.as_deref(), Some(pid));
        assert_eq!(p.p2p_multiaddrs, vec![good]);
        assert_eq!(p.p2p_port, Some(30333));
    }

    #[test]
    fn p2p_fields_are_dropped_without_a_valid_peer_id() {
        let v = json!({
            "peer_id": "../../etc/passwd",
            "p2p_port": 30333,
            "multiaddrs": ["/ip4/203.0.113.7/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"],
        });
        let (peer_id, mas, port) = p2p_fields(&v);
        assert!(peer_id.is_none());
        assert!(
            mas.is_empty(),
            "an unverified id cannot vouch for an address"
        );
        assert!(
            port.is_none(),
            "a p2p port is only meaningful with the peer id that owns it — publishing it 
             alone lets the directory pair this port with another node's id"
        );
    }

    /// The directory bug of 2026-10-20: box #1's status_url host ended up
    /// published with the Linux node's peer id. A poll snapshot carrying a
    /// different peer id for an already-measured endpoint must not replace the
    /// measured identity.
    fn provider_with(peer_id: Option<&str>, port: Option<u16>) -> Provider {
        let mut p = provider_from_caps(
            "5Abc",
            &json!({ "gflops": 10 }),
            "http://23.241.109.255:8093/status".into(),
            7,
        );
        p.peer_id = peer_id.map(|s| s.to_string());
        p.p2p_port = port;
        p.p2p_multiaddrs = peer_id
            .map(|id| vec![format!("/ip4/23.241.109.255/tcp/30333/p2p/{id}")])
            .unwrap_or_default();
        p
    }

    #[test]
    fn merge_never_pair_another_nodes_peer_id_with_this_endpoint() {
        let linux = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        // The measured (first) record belongs to a box with no published id yet.
        let existing = provider_with(None, None);
        let caps = json!({ "peer_id": linux, "p2p_port": 30333, "gflops": 99 });
        let merged = merge_poll_snapshot(&existing, &caps, existing.status_url.clone(), 11);
        // With nothing measured, the snapshot is the only identity — adopt it.
        assert_eq!(merged.peer_id.as_deref(), Some(linux));
        assert_eq!(merged.p2p_port, Some(30333));
        assert_eq!(merged.status_url, existing.status_url);
        assert_eq!(merged.gflops, 99, "measured fields are refreshed");
        assert_eq!(merged.latency_ms, 11);
    }

    #[test]
    fn merge_keeps_the_measured_identity_when_the_snapshot_disagrees() {
        let own = "12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw";
        let other = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        let existing = provider_with(Some(own), Some(30333));
        let caps = json!({ "peer_id": other, "p2p_port": 40444, "gflops": 5 });
        let merged = merge_poll_snapshot(&existing, &caps, existing.status_url.clone(), 3);
        assert_eq!(
            merged.peer_id.as_deref(),
            Some(own),
            "this endpoint's own id must win"
        );
        assert_eq!(merged.p2p_port, Some(30333));
        assert_eq!(merged.p2p_multiaddrs, existing.p2p_multiaddrs);
        // Everything else still comes from the fresh snapshot.
        assert_eq!(merged.gflops, 5);
    }

    #[test]
    fn merge_is_idempotent_for_the_same_identity() {
        let own = "12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw";
        let existing = provider_with(Some(own), Some(30333));
        let caps = json!({ "peer_id": own, "p2p_port": 30333, "gflops": 1 });
        let merged = merge_poll_snapshot(&existing, &caps, existing.status_url.clone(), 2);
        assert_eq!(merged.peer_id.as_deref(), Some(own));
        assert_eq!(merged.p2p_port, Some(30333));
        assert_eq!(merged.p2p_multiaddrs, existing.p2p_multiaddrs);
    }

    #[test]
    fn merge_keeps_a_measured_identity_the_snapshot_omits() {
        let own = "12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw";
        let existing = provider_with(Some(own), Some(30333));
        let caps = json!({ "gflops": 2 });
        let merged = merge_poll_snapshot(&existing, &caps, existing.status_url.clone(), 1);
        assert_eq!(merged.peer_id.as_deref(), Some(own));
        assert_eq!(merged.p2p_port, Some(30333));
        assert_eq!(merged.p2p_multiaddrs, existing.p2p_multiaddrs);
    }

    #[test]
    fn p2p_port_falls_back_to_the_multiaddr() {
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        let v = json!({
            "peer_id": pid,
            "multiaddrs": [format!("/ip4/203.0.113.7/tcp/30399/p2p/{pid}")],
        });
        let (_, _, port) = p2p_fields(&v);
        assert_eq!(port, Some(30399));
    }

    #[test]
    fn multiaddr_validation_rejects_junk() {
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        assert!(clean_p2p_multiaddr(&format!("/ip4/1.2.3.4/tcp/30333/p2p/{pid}")).is_some());
        assert!(clean_p2p_multiaddr(&format!("/dns4/node.example/tcp/30333/p2p/{pid}")).is_some());
        assert!(clean_p2p_multiaddr("/ip4/1.2.3.4/tcp/99999/p2p/12D3KooWAFt").is_none());
        assert!(clean_p2p_multiaddr("/ip4/1.2.3.4/tcp/30333").is_none());
        assert!(clean_p2p_multiaddr("/ip4/1.2.3.4/tcp/30333/p2p/bad id").is_none());
        assert!(clean_p2p_multiaddr("ip4/1.2.3.4/tcp/30333/p2p/12D3KooWAFt").is_none());
        assert!(clean_p2p_multiaddr("/ip4//tcp/30333/p2p/12D3KooWAFt").is_none());
    }

    /// Item #41 residual: a job's off-chain payload write must be VERIFIED before
    /// the job is advertised. Every old call site did `let _ = put(..)`, so a
    /// failed write produced an on-chain job with escrow locked and no payload —
    /// unrunnable by design, and only reclaimed at its deadline while the caller
    /// waited out a timeout.
    #[test]
    fn payload_write_is_verified_before_a_job_is_advertised() {
        let dir = std::env::temp_dir().join(format!(
            "gpn-payload-verify-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let job = gpn_agent::llm::LlmJob::new_llm("m", "keep the caller's prompt", 0.7, 16);

        // A writable store persists the payload and reads it back.
        let good = gpn_agent::llm::LlmStore::with_path(dir.join("store.json"));
        assert!(persist_job_payload(&good, 7, &job));
        assert_eq!(
            good.get(7).map(|j| j.prompt),
            Some("keep the caller's prompt".to_string())
        );

        // An unwritable target is reported (this is the case the old code ate).
        let blocked = dir.join("blocked.json");
        std::fs::create_dir_all(&blocked).unwrap();
        let bad = gpn_agent::llm::LlmStore::with_path(blocked);
        assert!(!persist_job_payload(&bad, 8, &job));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #46 Phase 3 step 2: a provider the AP can never dial (NAT/CGNAT) enters
    /// the registry from its own pull poll, and a snapshot for a box the AP
    /// CAN dial updates the measured fields without losing its endpoint or the
    /// AP-measured latency. Stale snapshots retire on their own.
    #[test]
    fn pull_poll_caps_register_a_provider_the_ap_cannot_dial() {
        let map: Arc<Mutex<std::collections::HashMap<String, (Instant, Value)>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let hex = "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";
        let ss58 = hex_to_ss58(hex).unwrap();
        let caps = json!({
            "address": "0xIGNORED", "gflops": 2500, "models": ["llama3.2:1b"],
            "can_pull": true, "vram_mb": 24000, "ram_mb": 32000,
            "tflops": 40.0, "gpu_class": "discrete", "capacity": 1, "active": 0,
            "capabilities": ["LLMInference"]
        });
        ingest_caps(&map, hex, &caps);

        // A NAT'd provider: no /status entry exists at all.
        let mut out: Vec<Provider> = vec![];
        apply_poll_caps(&mut out, &map);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].address, ss58,
            "keyed by canonical ss58, not the caps body"
        );
        assert_eq!(out[0].gflops, 2500);
        assert_eq!(out[0].vram_mb, 24000);
        assert!(out[0].can_pull);
        assert_eq!(out[0].capabilities, vec!["LLMInference".to_string()]);
        assert!(
            out[0].last_seen.is_some(),
            "a fresh poll is a live heartbeat"
        );
        assert_eq!(out[0].status_url, format!("pull://{ss58}"));

        // The same box also reachable over /status: keep the endpoint + the
        // AP-measured latency, refresh everything else.
        let mut out2 = vec![prov(&ss58, "http://box:8093/status", 17, "old-model")];
        apply_poll_caps(&mut out2, &map);
        assert_eq!(out2.len(), 1, "merged, not duplicated");
        assert_eq!(out2[0].status_url, "http://box:8093/status");
        assert_eq!(out2[0].latency_ms, 17);
        assert_eq!(out2[0].models, vec!["llama3.2:1b".to_string()]);

        // A snapshot that stopped arriving expires and cannot keep a dead box
        // routable forever.
        {
            let mut m = map.lock().unwrap();
            m.insert(
                ss58.clone(),
                (
                    Instant::now() - std::time::Duration::from_secs(POLL_CAPS_TTL_SECS + 1),
                    caps.clone(),
                ),
            );
        }
        let mut out3: Vec<Provider> = vec![];
        apply_poll_caps(&mut out3, &map);
        assert!(out3.is_empty(), "stale caps are dropped");
    }

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
            capabilities: default_capabilities(),
            peer_id: None,
            p2p_multiaddrs: vec![],
            p2p_port: None,
            active: 0,
            capacity: 4,
            latency_ms,
            last_seen: Some(Instant::now()),
            // Routing tests describe *deliverable* providers, so the delivery
            // gate (item #55) stays inert for them.
            pulled_seen: Some(Instant::now()),
        }
    }

    #[test]
    fn pick_gates_on_whether_the_model_fits_the_provider_vram() {
        // Item 3: a 7B model needs ~5.4 GB with headroom. The faster 4 GB box
        // must not be chosen even though it claims the model, or the caller
        // waits for an OOM/CPU fallback.
        let mut small = prov("small", "http://s", 5, "qwen2.5:7b");
        small.vram_mb = 4096;
        let mut big = prov("big", "http://b", 50, "qwen2.5:7b");
        big.vram_mb = 8188;
        let reg = vec![small.clone(), big];
        let (p, _) = pick(&reg, 0, 0, 0, 0.0, "qwen2.5:7b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p.address, "big", "the 4GB box cannot hold a 7B model");
        // A model that fits both: the faster box wins again.
        let mut small2 = small.clone();
        small2.models = vec!["llama3.2:1b".into()];
        let mut big2 = reg[1].clone();
        big2.models = vec!["llama3.2:1b".into()];
        let reg2 = vec![small2, big2];
        let (p2, _) = pick(&reg2, 0, 0, 0, 0.0, "llama3.2:1b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p2.address, "small");
        // Unknown VRAM (0) does not gate: an unprobed box keeps working.
        let mut unknown = prov("unknown", "http://u", 1, "qwen2.5:7b");
        unknown.vram_mb = 0;
        let reg3 = vec![unknown];
        assert!(pick(&reg3, 0, 0, 0, 0.0, "qwen2.5:7b", 0, &RttOverrides::new()).is_some());
    }

    #[test]
    fn the_public_catalogue_hides_fine_tunes_and_toys() {
        // Item 4: /v1/models goes through the same filter.
        let published =
            gpn_agent::models::filter_public_models(["tuned-42", "stories260K", "llama3.2:1b"]);
        assert_eq!(published, vec!["llama3.2:1b".to_string()]);
    }

    #[test]
    fn a_provider_that_never_polls_is_not_routable() {
        // item #55: the push (`POST /assign`) is gone, so a provider that only
        // answers `/status` can never be handed a lease — routing to it makes the
        // job sit Pending until the janitor cancels it (live: #6237). The gate is
        // fail-closed and prefers the provider we can actually deliver to.
        let mut push_only = prov("box", "http://box/status", 1, "llama3.2:1b");
        push_only.pulled_seen = None;
        let polling = prov("local", "http://local/status", 90, "llama3.2:1b");

        // Even though the push-only box is 90x "faster", it is not a candidate.
        let reg = vec![push_only.clone(), polling.clone()];
        let (p, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p.address, "local");

        // A poll older than the caps TTL is not evidence of a live poller either.
        let mut stale = prov("stale", "http://stale/status", 1, "llama3.2:1b");
        stale.pulled_seen = Some(Instant::now() - Duration::from_secs(POLL_CAPS_TTL_SECS + 1));
        let reg = vec![stale, polling.clone()];
        let (p, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p.address, "local");

        // Nothing polling at all ⇒ no candidate, so the AP fails closed (503)
        // instead of black-holing a created, escrowed job.
        let reg = vec![push_only];
        assert!(pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 0, &RttOverrides::new()).is_none());
    }

    #[test]
    fn create_confirm_window_honours_env_and_floors_out_absurd_values() {
        // The pre-fix confirm window was a hardcoded 2 s — shorter than one GPN block, so a
        // create that landed normally looked "lost" and the id allocator was rewound onto an
        // id that tx was about to take (the +N label drift). The window must be tunable and
        // must never collapse back to a sub-block value.
        std::env::set_var("GPN_CREATE_CONFIRM_MS", "12000");
        assert_eq!(create_confirm_ms(), 12_000);
        std::env::set_var("GPN_CREATE_CONFIRM_MS", "50");
        assert_eq!(
            create_confirm_ms(),
            45_000,
            "absurdly small values fall back to the default"
        );
        std::env::set_var("GPN_CREATE_CONFIRM_MS", "nonsense");
        assert_eq!(create_confirm_ms(), 45_000);
        std::env::remove_var("GPN_CREATE_CONFIRM_MS");
        assert_eq!(create_confirm_ms(), 45_000);
    }

    #[test]
    fn pending_create_is_reported_until_the_ttl_expires() {
        // POST /jobs returns the predicted id before inclusion, so /job/{id} must say
        // "pending inclusion" instead of "not found" for a genuinely in-flight create
        // — and must stop doing so once the window passes (a create that never landed
        // must not masquerade as pending forever).
        assert!(pending_create_is_live(Duration::from_secs(0)));
        assert!(pending_create_is_live(PENDING_CREATE_TTL));
        assert!(!pending_create_is_live(
            PENDING_CREATE_TTL + Duration::from_secs(1)
        ));

        let id = 900_001;
        assert!(!create_is_pending(id), "unknown ids stay not-found");
        mark_create_pending(id);
        assert!(
            create_is_pending(id),
            "a submitted create reads as pending, not lost"
        );
        clear_create_pending(id);
        assert!(
            !create_is_pending(id),
            "once the chain knows the id, the marker is gone"
        );
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
        chain.insert(
            "http://win:8093/status".to_string(),
            "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty".to_string(),
        );
        // Heartbeat omits address -> use the on-chain key.
        assert_eq!(
            attribute_address("", "http://win:8093/status", &chain),
            "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty"
        );
        // Heartbeat reports its own address -> trust it.
        assert_eq!(
            attribute_address("5GrwvaE", "http://win:8093/status", &chain),
            "5GrwvaE"
        );
        // Unknown endpoint + no heartbeat address -> empty (unchanged behaviour).
        assert_eq!(
            attribute_address("", "http://other:8093/status", &chain),
            ""
        );
    }

    #[test]
    fn ap_view_picks_nearest_to_the_access_point_by_default() {
        // A is 5ms from the AP, B is 50ms. No client hints -> A wins.
        let reg = vec![
            prov("A", "http://a:8093", 5, "llama3.2:1b"),
            prov("B", "http://b:8093", 50, "llama3.2:1b"),
        ];
        let (p, _) = pick(
            &reg,
            0,
            0,
            0,
            0.0,
            "llama3.2:1b",
            120_000,
            &RttOverrides::new(),
        )
        .unwrap();
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
        assert_eq!(
            p.address, "B",
            "client-measured RTT must override AP-measured latency"
        );
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
        assert_eq!(
            moderation_block(blocked).map(|c| c.id()),
            Some("weapons_mass_casualty")
        );
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
    fn absent_chain_record_keeps_a_young_job_tracked() {
        // A fast create is pool-ack only: `Jobs[id]` does not exist yet, which is
        // indistinguishable from "gone". Dropping the entry loses `max_tokens` and
        // the adopt scan re-adds it as 0 (window collapses to the 120 s floor) —
        // so a large in-flight job could be cancelled by our own janitor.
        let mt = 4000; // stale_secs(4000) = 60 + 200*2 = 460
        assert_eq!(stale_secs(mt), 460);
        assert!(survives_absent_read(0, mt));
        assert!(survives_absent_read(36, mt)); // observed create->inclusion latency
        assert!(survives_absent_read(460, mt));
        assert!(survives_absent_read(580, mt)); // stale window + grace
                                                // Past the grace it is treated as gone, so a create that never lands
                                                // still ages out of the tracking list (no slot leak).
        assert!(!survives_absent_read(581, mt));
        // Same shape at the floor and the cap (no overflow, still bounded).
        assert!(survives_absent_read(239, 0));
        assert!(!survives_absent_read(241, 0));
        assert!(survives_absent_read(1019, u64::MAX));
        assert!(!survives_absent_read(1021, u64::MAX));
    }

    #[test]
    fn delivered_results_are_never_janitor_cancelled() {
        // The cancel gate is `Pending && aged past the token window`; a delivered
        // result must veto it regardless of age (the chain lags behind reality
        // while our accept/start txs queue behind slow blocks).
        for status in ["done", "error"] {
            assert!(
                job_has_result(status),
                "{status} means work already happened"
            );
        }
        for status in ["queued", "running", ""] {
            assert!(
                !job_has_result(status),
                "{status} must still be cancellable"
            );
        }
        // Composite decision: the #5534 case exactly.
        let mt = 512; // stale_secs(512) = 60 + 25*2 = 120 (the floor)
        assert_eq!(stale_secs(mt), 120);
        // Delivered but still Pending on-chain and past the window -> DO NOT cancel.
        assert!(!should_cancel_stale("Pending", 361, 120, true, false));
        // Not delivered -> the age heuristic still reclaims a stranded Pending job.
        assert!(should_cancel_stale("Pending", 361, 120, false, false));
        // …but only past its own token-scaled window.
        assert!(!should_cancel_stale("Pending", 119, 120, false, false));
        // Work already started on-chain is never requestor-cancelled.
        for s in ["Accepted", "Running", "AwaitingVerification"] {
            assert!(!should_cancel_stale(s, 10_000, 120, false, false));
        }
        // Item #91: a lease delivered moments ago is a claim in flight — the
        // janitor must wait for it even though the job is old and unusable-looking.
        assert!(!should_cancel_stale("Pending", 361, 120, false, true));
    }

    /// Item #91: delivery arms a bounded veto, and the veto lapses so a dead
    /// provider is still reclaimed by the fast path. Also pins the stamping
    /// itself: without it `claim_in_flight` can never be true in production.
    #[test]
    fn a_delivered_lease_suspends_the_stale_cancel() {
        // Boundary: the veto covers one accept + one start inclusion.
        assert!(claim_in_flight(Some(0)));
        assert!(claim_in_flight(Some(CLAIM_IN_FLIGHT_SECS - 1)));
        assert!(!claim_in_flight(Some(CLAIM_IN_FLIGHT_SECS)));
        assert!(
            CLAIM_IN_FLIGHT_SECS >= 120,
            "one accept + one start at the observed 60-120 s/inclusion"
        );
        // No stamp at all (never delivered, e.g. an adopted orphan) is not a claim.
        assert!(!claim_in_flight(None));

        // The map half: a stamped id vetoes, an unstamped one does not, and the
        // stamp ages out with the clock rather than lingering forever.
        let id = 9_900_091u64;
        let now = Instant::now();
        assert!(!claim_in_flight_now(id, now));
        note_lease_delivered(id, now);
        assert!(claim_in_flight_now(id, now));
        assert!(claim_in_flight_now(
            id,
            now + Duration::from_secs(CLAIM_IN_FLIGHT_SECS - 1)
        ));
        assert!(!claim_in_flight_now(
            id,
            now + Duration::from_secs(CLAIM_IN_FLIGHT_SECS)
        ));
        // Retention: a stamp older than the retention window is pruned on the
        // next delivery, so the map cannot grow with the job history.
        let later = now + Duration::from_secs(LEASE_STAMP_RETENTION_SECS + 1);
        note_lease_delivered(9_900_092u64, later);
        assert!(lease_delivered().lock().unwrap().get(&id).is_none());
    }

    #[test]
    fn long_job_kinds_get_a_stale_floor_the_token_window_would_cut_short() {
        // Observed 2026-10-23: fine-tune #6528 (max_tokens 0) was requestor-cancelled
        // by the AP janitor 145 s after creation while the provider was actively
        // tuning it; its `start` then failed JobNotPending.
        assert_eq!(stale_secs(0), 120, "LLM floor is unchanged");
        assert!(
            stale_secs_for_kind("finetune", 0) >= 1800,
            "a fine-tune needs minutes before it can leave Pending"
        );
        assert!(stale_secs_for_kind("image", 0) >= 900);
        assert!(stale_secs_for_kind("video", 0) >= 900);
        assert!(stale_secs_for_kind("ml", 0) >= 300);
        // LLM/chat keeps the token-scaled window exactly.
        assert_eq!(stale_secs_for_kind("llm", 4000), stale_secs(4000));
        assert_eq!(stale_secs_for_kind("", 512), stale_secs(512));
        // The 12:33:14 self-cancel would not have fired at 145 s for a fine-tune …
        assert!(!should_cancel_stale(
            "Pending",
            145,
            stale_secs_for_kind("finetune", 0),
            false,
            false
        ));
        // … while a genuinely stranded fine-tune is still reclaimed past the floor.
        assert!(should_cancel_stale(
            "Pending",
            1801,
            stale_secs_for_kind("finetune", 0),
            false,
            false
        ));
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
        assert_eq!(
            resolve_job_prompt(&json!({"prompt": "hi"})).as_deref(),
            Some("hi")
        );
        assert_eq!(
            resolve_job_prompt(&json!({"input": "vec"})).as_deref(),
            Some("vec")
        );
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
    fn chat_facade_keeps_the_callers_prompt_and_rejects_an_empty_one() {
        // Item #37: the OpenAI facade used to flatten ONLY `messages[]`, so a
        // `prompt`-only body (llama.cpp / Ollama-style client) collapsed to the
        // bare "assistant:" suffix and a nonsense job was created, routed and
        // paid. Both shapes must now carry the caller's text, and a body with
        // neither must be refused instead of producing the empty-prompt job.
        assert_eq!(
            resolve_job_prompt(&json!({"model": "llama3.2:1b", "prompt": "capital of France?"}))
                .as_deref(),
            Some("capital of France?")
        );
        let m = resolve_job_prompt(&json!({
            "model": "llama3.2:1b",
            "messages": [{"role": "user", "content": "capital of France?"}]
        }))
        .expect("OpenAI body must yield a prompt");
        assert!(m.contains("capital of France?"));
        assert!(m.ends_with("assistant:"));
        // The degenerate body that used to become a paid, unservable job.
        let empty = resolve_job_prompt(&json!({"model": "llama3.2:1b"}));
        assert_eq!(empty, None, "no prompt => 400, no create, no escrow");
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
        let reg = vec![
            mk("small", "http://s", 5, 8188),
            mk("big", "http://b", 50, 24576),
        ];
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
        let (p, _) = pick(
            &reg2,
            0,
            0,
            32000,
            0.0,
            "llama3.2:1b",
            0,
            &RttOverrides::new(),
        )
        .unwrap();
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
        assert_eq!(
            p.address, "dgpu",
            "3 TFLOPS job must skip the integrated GPU"
        );
        // No floor -> the faster integrated node wins (gate inert).
        let (p2, _) = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 0, &RttOverrides::new()).unwrap();
        assert_eq!(p2.address, "apu");

        // A known-but-too-low tflops is excluded even if it claims "discrete"...
        let weak = mk("weak", "http://w", 1, "discrete", 2.0);
        assert!(pick(
            std::slice::from_ref(&weak),
            0,
            0,
            0,
            3.0,
            "llama3.2:1b",
            0,
            &RttOverrides::new()
        )
        .is_none());
        // ...and an *unknown* tflops (0) is trusted ONLY when self-declared discrete.
        let unknown_disc = mk("ud", "http://ud", 1, "discrete", 0.0);
        assert!(pick(
            &[unknown_disc],
            0,
            0,
            0,
            3.0,
            "llama3.2:1b",
            0,
            &RttOverrides::new()
        )
        .is_some());
        let unknown_integ = mk("ui", "http://ui", 1, "integrated", 0.0);
        assert!(pick(
            &[unknown_integ],
            0,
            0,
            0,
            3.0,
            "llama3.2:1b",
            0,
            &RttOverrides::new()
        )
        .is_none());
    }

    #[test]
    fn pick_gate_recognises_amd_discrete_and_igpu() {
        // item #27: the same gate must work for AMD. Build a provider profile
        // straight from an AMD model name via the shared detection helpers.
        let mk_amd = |addr: &str, url: &str, lat: u64, name: &str| {
            let mut p = prov(addr, url, lat, "llama3.2:1b");
            p.gpu_class = gpn_agent::gpu::classify_gpu_name(name).to_string();
            p.tflops = gpn_agent::gpu::estimate_tflops(name);
            p
        };
        // An AMD RX 7900 XTX is a discrete dGPU → admitted by the class flag.
        let dgpu = mk_amd("amd-d", "http://d", 50, "AMD Radeon RX 7900 XTX");
        assert_eq!(dgpu.gpu_class, "discrete");
        assert!(pick(
            &[dgpu],
            0,
            0,
            0,
            3.0,
            "llama3.2:1b",
            0,
            &RttOverrides::new()
        )
        .is_some());
        // An AMD APU (Radeon Graphics) is integrated + sub-floor → excluded.
        let igpu = mk_amd("amd-i", "http://i", 1, "AMD Radeon(TM) Graphics");
        assert_eq!(igpu.gpu_class, "integrated");
        assert!(pick(
            &[igpu],
            0,
            0,
            0,
            3.0,
            "llama3.2:1b",
            0,
            &RttOverrides::new()
        )
        .is_none());
    }

    #[test]
    fn callback_default_uses_loopback_for_wildcard_binds() {
        // A wildcard listen (0.0.0.0/::) is not a routable destination; the
        // advertised callback should be loopback so a co-located provider
        // reaches the AP with no public-DNS lookup (host-DNS /result defect).
        assert_eq!(loopback_if_wildcard("0.0.0.0:8092"), "127.0.0.1:8092");
        assert_eq!(loopback_if_wildcard("[::]:8092"), "[::1]:8092");
        assert_eq!(loopback_if_wildcard("0.0.0.0"), "127.0.0.1");
        // Non-wildcard binds are preserved verbatim.
        assert_eq!(loopback_if_wildcard("10.0.0.5:8092"), "10.0.0.5:8092");
    }

    #[test]
    fn write_assignment_records_the_callback_for_result_return() {
        // Item #14: the assignment record must carry the AP callback URL so a
        // remote provider knows where to POST the finished result.
        let p = std::env::temp_dir().join(format!("gpn-assign-test-{}.json", std::process::id()));
        let ps = p.to_string_lossy().to_string();
        write_assignment(
            &ps,
            42,
            "5Bob",
            "qwen2.5:7b",
            true,
            "http://gpn-testnet.duckdns.org:8092",
            "tok-abc",
        )
        .unwrap();
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
    fn assignment_job_payload_roundtrips_as_llmjob() {
        // #46 Phase 3: the payload travels in the pull lease (never a push), so
        // this wire shape is what `collect_leases` embeds and the provider's
        // `assign_pull::apply_lease` deserializes back into its own store.
        let job = gpn_agent::llm::LlmJob::new_llm("qwen2.5:7b", "hello world", 0.7, 256);
        let v = serde_json::to_value(&job).unwrap();
        let back: gpn_agent::llm::LlmJob = serde_json::from_value(v).unwrap();
        assert_eq!(back.kind, "llm");
        assert_eq!(back.model, "qwen2.5:7b");
        assert_eq!(back.prompt, "hello world");
        assert_eq!(back.status, "queued");
    }

    #[test]
    fn an_empty_registry_is_warming_up_not_an_unknown_model() {
        // The AP binds :8092 before its first refresh pass, so a request in that
        // window sees an empty registry. That must read as "retry shortly" (503),
        // never as "your model name is wrong" (400) — the 2026-10-22 11:36 probe.
        let empty: Vec<Provider> = Vec::new();
        let err = "unknown model 'llama3.2:1b'; available: []";
        let (code, msg) = model_resolution_status(&empty, err);
        assert_eq!(code, 503);
        assert!(msg.contains("warming up"), "{msg}");
        assert!(!msg.contains("unknown model"), "{msg}");

        // With a live provider the rejection stays a hard 400 and keeps the reason:
        // an unservable model would otherwise orphan a Pending job forever.
        let reg = vec![prov(
            "5Me",
            "http://127.0.0.1:8093/status",
            5,
            "llama3.2:1b",
        )];
        let (code, msg) = model_resolution_status(&reg, err);
        assert_eq!(code, 400);
        assert_eq!(msg, err);
    }

    #[test]
    fn pull_query_parsing_handles_escapes_and_missing_keys() {
        // #46 pull transport: the provider asks `/assignments?provider=..&since=..&wait=..`.
        let url = "/assignments?provider=0xab%2Fcd&since=17&wait=25000&x=1";
        assert_eq!(query_param(url, "provider"), Some("0xab%2Fcd"));
        assert_eq!(pct_decode("0xab%2Fcd"), "0xab/cd");
        assert_eq!(pct_decode("a+b"), "a b");
        assert_eq!(
            query_param(url, "since").and_then(|s| s.parse::<u64>().ok()),
            Some(17)
        );
        assert_eq!(query_param(url, "missing"), None);
        // A URL with no query must not panic or invent values.
        assert_eq!(query_param("/assignments", "provider"), None);
        // Malformed escapes pass through untouched rather than eating bytes.
        assert_eq!(pct_decode("100%"), "100%");
    }

    #[test]
    fn provider_identity_matches_across_hex_and_ss58() {
        // The AP records the ss58 form; a provider may present its hex account id.
        let hex = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";
        let ss58 = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";
        assert!(provider_matches(ss58, hex));
        assert!(provider_matches(hex, ss58));
        assert!(provider_matches(ss58, ss58));
        assert!(!provider_matches(
            ss58,
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"
        ));
        assert!(!provider_matches("", ss58));
    }

    #[test]
    fn collect_leases_stamps_delivery_so_the_janitor_sees_a_claim_in_flight() {
        // Item #91: the poll response IS the delivery; if it is not stamped the
        // janitor can cancel the very job the provider is accepting.
        let dir = std::env::temp_dir().join(format!("gpn-lease-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("assignments.json");
        std::fs::write(
            &store,
            r#"{"7001":{"provider":"0xabc","model":"m","pull":true},"7002":{"provider":"0xdef","model":"m","pull":true}}"#,
        )
        .unwrap();
        let leases = collect_leases(store.to_str().unwrap(), "0xabc", 0, 20);
        assert_eq!(leases.len(), 1, "only 7001 belongs to this provider");
        let now = Instant::now();
        assert!(
            claim_in_flight_now(7001, now),
            "a lease returned to a provider must count as a claim in flight"
        );
        assert!(
            !claim_in_flight_now(7002, now),
            "another provider's lease was not delivered to anyone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_leases_filters_by_provider_and_cursor() {
        let dir = std::env::temp_dir().join(format!("gpn-lease-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = dir.join("assignments.json");
        let body = serde_json::json!({
            "10": {"provider": "5Me", "model": "m", "pull": false,
                    "callback": "http://ap", "callback_token": "t10"},
            "11": {"provider": "5Other", "model": "m", "pull": false,
                    "callback": "http://ap", "callback_token": "t11"},
            "12": {"provider": "5Me", "model": "m2", "pull": true,
                    "callback": "http://ap", "callback_token": "t12"}
        });
        std::fs::write(&store, body.to_string()).unwrap();
        let path = store.to_str().unwrap().to_string();

        // Only our own assignments, above the cursor, ascending.
        let got = collect_leases(&path, "5Me", 10, MAX_PULL_LEASES);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["job_id"], 12);
        assert_eq!(got[0]["callback_token"], "t12");
        assert_eq!(got[0]["pull"], true);

        // Cursor 0 returns both of ours; someone else's never leaks in.
        let all = collect_leases(&path, "5Me", 0, MAX_PULL_LEASES);
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|l| l["provider"] == "5Me"));
        assert!(all.iter().all(|l| l["job_id"] != 11));

        // The cap is honoured (a long-absent provider catches up over polls).
        assert_eq!(collect_leases(&path, "5Me", 0, 1).len(), 1);
        // A missing store is empty, never fatal.
        assert!(collect_leases("/nonexistent/assignments.json", "5Me", 0, 20).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── unified routing + failover (queue item #70) ───────────────────────────

    fn prov_caps(addr: &str, url: &str, latency_ms: u64, model: &str, caps: &[&str]) -> Provider {
        let mut p = prov(addr, url, latency_ms, model);
        p.capabilities = caps.iter().map(|c| c.to_string()).collect();
        p
    }

    /// A text request and an image request go through the SAME function, and it
    /// filters on capability per kind: an LLM box is not an image candidate and a
    /// diffusion box is not a text candidate.
    #[test]
    fn every_job_kind_shares_one_candidate_pipeline() {
        let llm = prov_caps("llm", "http://llm", 10, "llama3.2:1b", &["LLMInference"]);
        let diff = prov_caps("diff", "http://diff", 10, "", &["ImageGeneration"]);
        let video = prov_caps("vid", "http://vid", 10, "", &["VideoGeneration"]);
        let ml = prov_caps("ml", "http://ml", 10, "resnet50", &["MLInference"]);
        let ft = prov_caps("ft", "http://ft", 10, "llama3.2:1b", &["FineTuning"]);
        let reg = vec![llm, diff, video, ml, ft];

        let text = route_candidates(
            &reg,
            JobKind::Text,
            &RouteNeeds {
                model: "llama3.2:1b".into(),
                ..Default::default()
            },
            &RttOverrides::new(),
        );
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].provider.address, "llm");
        assert!(!text[0].needs_pull);

        for (kind, want, model) in [
            (JobKind::Image, "diff", "sd-turbo"),
            (JobKind::Video, "vid", "ltx-video"),
            (JobKind::Ml, "ml", "resnet50"),
            (JobKind::Finetune, "ft", "llama3.2:1b"),
        ] {
            let needs = RouteNeeds {
                // Generation ignores the model (it is not in `models[]`); ML DOES
                // match on it, exactly like text. Fine-tune also ignores it — its
                // base model is an HF id, not an Ollama tag — see the assertion
                // below.
                model: if kind.is_generation() {
                    "sd-turbo".into()
                } else {
                    model.into()
                },
                ..Default::default()
            };
            let got = route_candidates(&reg, kind, &needs, &RttOverrides::new());
            assert_eq!(got.len(), 1, "{kind:?} must match exactly one box");
            assert_eq!(
                got[0].provider.address, want,
                "{kind:?} routed to the wrong kind"
            );
            assert!(!got[0].needs_pull, "generation never needs an Ollama pull");
        }
        // An ML job for a model nobody serves is NOT routable (no orphaned
        // Pending job), even though every box advertises MLInference.
        let missing = RouteNeeds {
            model: "resnet152".into(),
            ..Default::default()
        };
        assert!(route_candidates(&reg, JobKind::Ml, &missing, &RttOverrides::new()).is_empty());

        // A fine-tune's base model is a HuggingFace id (`HuggingFaceTB/…`), which
        // no box lists in `models[]`; the FineTuning capability alone must make it
        // routable (item #67).
        let hf = RouteNeeds {
            model: "HuggingFaceTB/SmolLM2-135M-Instruct".into(),
            ..Default::default()
        };
        let ft_only = route_candidates(&reg, JobKind::Finetune, &hf, &RttOverrides::new());
        assert_eq!(ft_only.len(), 1);
        assert_eq!(ft_only[0].provider.address, "ft");
    }

    /// `job_type` + model admission (item #76): the native `POST /jobs` path must
    /// pick the same kind the chat/image facades do, and it must classify the
    /// model rather than trust a defaulted `job_type`.
    #[test]
    fn job_type_and_model_classify_to_the_same_kinds() {
        assert_eq!(
            admit(Some("LLMInference"), "llama3.2:1b"),
            Ok(JobKind::Text)
        );
        assert_eq!(
            admit(Some("ImageGeneration"), "sd-turbo"),
            Ok(JobKind::Image)
        );
        // The reported #76 shape: `job_type` left at the LLM default.
        assert_eq!(admit(Some("LLMInference"), "sd-turbo"), Ok(JobKind::Image));
        assert_eq!(
            admit(Some("VideoGeneration"), "ltx-video"),
            Ok(JobKind::Video)
        );
        assert_eq!(admit(Some("LLMInference"), "ltx-video"), Ok(JobKind::Video));
        assert_eq!(admit(Some("MLInference"), "resnet50"), Ok(JobKind::Ml));
        assert_eq!(
            admit(Some("FineTuning"), "llama3.2:1b"),
            Ok(JobKind::Finetune)
        );
        // An unknown type is refused, not downgraded to text (before any escrow).
        assert!(admit(Some("ImageGen"), "sd-turbo").is_err());
        assert!(admit(Some("Custom"), "llama3.2:1b").is_err());
    }

    /// `POST /route/{id}` classifies an existing job from its stored payload, so a
    /// client-created image/video job is never validated against the chat catalogue
    /// or handed to an LLM box (item #76).
    #[test]
    fn an_existing_job_is_routed_by_its_payload_kind() {
        assert_eq!(route_kind(Some("image"), "sd-turbo"), JobKind::Image);
        assert_eq!(route_kind(Some("video"), "ltx-video"), JobKind::Video);
        assert_eq!(route_kind(Some("ml"), "resnet50"), JobKind::Ml);
        assert_eq!(
            route_kind(Some("finetune"), "HuggingFaceTB/SmolLM2-135M"),
            JobKind::Finetune
        );
        assert_eq!(route_kind(Some("llm"), "llama3.2:1b"), JobKind::Text);
        // No stored payload (a remote client kept it): the model name decides.
        assert_eq!(route_kind(None, "sdxl-turbo"), JobKind::Image);
        assert_eq!(route_kind(None, "ltx-video"), JobKind::Video);
        assert_eq!(route_kind(None, "llama3.2:1b"), JobKind::Text);
        // A stale/unknown payload kind must not hide a generation model.
        assert_eq!(route_kind(Some("banana"), "flux-dev"), JobKind::Image);
    }

    // ── public fine-tuning (queue item #67) ──────────────────────────────────

    #[test]
    fn a_public_finetune_request_is_validated_before_any_escrow() {
        let ok = json!({
            "model": "llama3.2:1b",
            "dataset": [
                {"prompt": "2+2?", "completion": "4"},
                {"prompt": "hi", "completion": "hello"}
            ],
            "lora_r": 32,
            "epochs": 3
        });
        let (model, dataset, lora_r, epochs) = parse_finetune_request(&ok).unwrap();
        assert_eq!(model, "llama3.2:1b");
        assert_eq!((lora_r, epochs), (32, 3));
        let parsed: Value = serde_json::from_str(&dataset).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[1]["completion"], "hello");

        // A pre-encoded JSON string is normalized to the same array payload.
        let as_text = json!({"model": "m", "dataset": "[{\"prompt\":\"a\",\"completion\":\"b\"}]"});
        let (_, d2, _, _) = parse_finetune_request(&as_text).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&d2)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // Hyperparameters are clamped into a sane band, never trusted raw.
        let clamped = json!({"model": "m", "dataset": [{"prompt": "a", "completion": "b"}], "lora_r": 9999, "epochs": 0});
        let (_, _, r, e) = parse_finetune_request(&clamped).unwrap();
        assert_eq!((r, e), (128, 1));

        // Rejections: no base model, no dataset, empty dataset, a missing half
        // of a pair, and an oversized dataset (a tune is minutes of GPU).
        assert!(
            parse_finetune_request(&json!({"dataset": [{"prompt": "a", "completion": "b"}]}))
                .is_err()
        );
        assert!(parse_finetune_request(&json!({"model": "m"})).is_err());
        assert!(parse_finetune_request(&json!({"model": "m", "dataset": []})).is_err());
        assert!(
            parse_finetune_request(&json!({"model": "m", "dataset": [{"prompt": "a"}]})).is_err()
        );
        let big: Vec<Value> = (0..MAX_FINETUNE_EXAMPLES + 1)
            .map(|i| json!({"prompt": format!("p{i}"), "completion": "c"}))
            .collect();
        assert!(parse_finetune_request(&json!({"model": "m", "dataset": big})).is_err());
    }

    /// A provider can die after writing `done` but before `complete_job`; the
    /// expiry sweep then fails the job. The chain must win for terminal states,
    /// so a poll never reports `succeeded` for a job that was refunded.
    #[test]
    fn finetune_status_prefers_the_chain_for_terminal_states() {
        assert_eq!(finetune_public_status(None, Some("Pending")), "queued");
        assert_eq!(
            finetune_public_status(Some("queued"), Some("Accepted")),
            "running"
        );
        assert_eq!(
            finetune_public_status(Some("running"), Some("Running")),
            "running"
        );
        assert_eq!(
            finetune_public_status(Some("done"), Some("AwaitingVerification")),
            "succeeded"
        );
        assert_eq!(
            finetune_public_status(Some("done"), Some("Completed")),
            "succeeded"
        );
        assert_eq!(
            finetune_public_status(Some("done"), Some("Failed")),
            "failed"
        );
        assert_eq!(
            finetune_public_status(Some("error"), Some("Cancelled")),
            "failed"
        );
        assert_eq!(
            finetune_public_status(Some("error"), Some("Running")),
            "failed"
        );
        assert_eq!(finetune_public_status(None, None), "queued");
    }

    /// Ordering: idle before busy, then RTT-aware latency (the client's own
    /// measurement wins over the AP-measured value).
    #[test]
    fn candidates_are_ordered_idle_first_then_by_client_rtt() {
        let mut a = prov_caps("a", "http://a", 10, "m", &["LLMInference"]);
        let mut b = prov_caps("b", "http://b", 50, "m", &["LLMInference"]);
        let busy = {
            let mut p = prov_caps("busy", "http://busy", 1, "m", &["LLMInference"]);
            p.active = 1;
            p
        };
        a.models = vec!["m".into()];
        b.models = vec!["m".into()];
        let reg = vec![busy, b.clone(), a.clone()];
        let needs = RouteNeeds {
            model: "m".into(),
            ..Default::default()
        };
        let got = route_candidates(&reg, JobKind::Text, &needs, &RttOverrides::new());
        assert_eq!(
            got.iter()
                .map(|c| c.provider.address.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "busy"],
            "idle boxes first (by latency), the busy one last"
        );
        // The client measured a fast path to `b`: it now outranks `a`.
        let mut rtt = RttOverrides::new();
        rtt.insert("b".to_string(), 1);
        let got2 = route_candidates(&reg, JobKind::Text, &needs, &rtt);
        assert_eq!(got2[0].provider.address, "b");
    }

    /// A cold puller is ranked against a ready box by latency + pull cost: on a
    /// slow link the ready box wins, on a fast one the puller can win.
    #[test]
    fn a_puller_only_wins_when_the_download_still_finishes_sooner() {
        let ready = prov_caps("ready", "http://ready", 100, "m", &["LLMInference"]);
        let mut puller = prov_caps("puller", "http://puller", 5, "other", &["LLMInference"]);
        puller.can_pull = true;
        let reg = vec![ready, puller];
        let needs = |penalty| RouteNeeds {
            model: "m".into(),
            pull_penalty_ms: penalty,
            ..Default::default()
        };
        let cheap = route_candidates(&reg, JobKind::Text, &needs(10), &RttOverrides::new());
        assert_eq!(cheap[0].provider.address, "puller");
        assert!(cheap[0].needs_pull);
        let expensive =
            route_candidates(&reg, JobKind::Text, &needs(120_000), &RttOverrides::new());
        assert_eq!(expensive[0].provider.address, "ready");
        assert!(!expensive[0].needs_pull);
    }

    /// Hardware gates: VRAM floor, model-fits-VRAM, and gflops — all applied to
    /// EVERY kind, not just text.
    #[test]
    fn every_kind_honours_the_vram_and_gflops_gates() {
        let mut small = prov_caps("small", "http://s", 1, "ltx-video", &["VideoGeneration"]);
        small.vram_mb = 8188;
        let mut big = prov_caps("big", "http://b", 90, "ltx-video", &["VideoGeneration"]);
        big.vram_mb = 24576;
        let reg = vec![small, big];
        let needs = RouteNeeds {
            model: "ltx-video".into(),
            min_vram_mb: 16000,
            ..Default::default()
        };
        let got = route_candidates(&reg, JobKind::Video, &needs, &RttOverrides::new());
        assert_eq!(
            got.len(),
            1,
            "the 8 GB box must be excluded for a video job"
        );
        assert_eq!(got[0].provider.address, "big");

        // min_gflops is a floor, applied per kind.
        let weak = prov_caps("weak", "http://w", 1, "", &["ImageGeneration"]);
        let mut weak = weak;
        weak.gflops = 10_000;
        let strong = prov_caps("strong", "http://st", 99, "", &["ImageGeneration"]);
        let reg2 = vec![weak, strong];
        let needs2 = RouteNeeds {
            min_gflops: 20_000,
            ..Default::default()
        };
        let got2 = route_candidates(&reg2, JobKind::Image, &needs2, &RttOverrides::new());
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0].provider.address, "strong");
    }

    /// A provider that cannot receive work (stale pull poll) or that is not
    /// healthy (`last_seen` cleared by a failed /status poll) is not a candidate.
    #[test]
    fn unhealthy_and_undeliverable_providers_are_not_candidates() {
        let mut stale_pull = prov_caps("stale-pull", "http://sp", 1, "m", &["LLMInference"]);
        stale_pull.pulled_seen = Some(Instant::now() - Duration::from_secs(POLL_CAPS_TTL_SECS + 1));
        let mut never_pulled = prov_caps("never-pull", "http://np", 1, "m", &["LLMInference"]);
        never_pulled.pulled_seen = None;
        let mut dead = prov_caps("dead", "http://d", 1, "m", &["LLMInference"]);
        dead.last_seen = None;
        let reg = vec![stale_pull, never_pulled, dead];
        let needs = RouteNeeds {
            model: "m".into(),
            ..Default::default()
        };
        assert!(route_candidates(&reg, JobKind::Text, &needs, &RttOverrides::new()).is_empty());
    }

    /// Free capacity is preferred; when NO box has any, busy boxes are still
    /// returned so the job queues instead of being refused.
    #[test]
    fn free_capacity_is_preferred_but_a_busy_fleet_still_queues() {
        let mut busy = prov_caps("busy", "http://busy", 1, "m", &["LLMInference"]);
        busy.active = 1;
        busy.capacity = 1; // one GPU = one job: a running job means no free slot
        let idle = prov_caps("idle", "http://idle", 300, "m", &["LLMInference"]);
        let needs = RouteNeeds {
            model: "m".into(),
            ..Default::default()
        };
        let got = route_candidates(
            &[busy.clone(), idle],
            JobKind::Text,
            &needs,
            &RttOverrides::new(),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].provider.address, "idle");
        let only_busy = route_candidates(&[busy], JobKind::Text, &needs, &RttOverrides::new());
        assert_eq!(only_busy.len(), 1, "a busy fleet queues, it does not 503");
    }

    /// Item #70 failover: a `NotAttempted` refusal moves to the NEXT provider —
    /// here an image is served by the second box after the first refuses
    /// (no diffusion endpoint / provider-signed refund).
    #[test]
    fn a_refusal_moves_to_the_next_candidate_and_the_second_provider_serves() {
        use gpn_agent::hub_ap::ChannelFail;
        let first = prov_caps("first", "http://first", 1, "sd-turbo", &["ImageGeneration"]);
        let second = prov_caps(
            "second",
            "http://second",
            50,
            "sd-turbo",
            &["ImageGeneration"],
        );
        let cands = route_candidates(
            &[first, second],
            JobKind::Image,
            &RouteNeeds {
                model: "sd-turbo".into(),
                ..Default::default()
            },
            &RttOverrides::new(),
        );
        assert_eq!(cands[0].provider.address, "first");
        let mut tried = Vec::new();
        let out = failover("test-image", &cands, |p| {
            tried.push(p.address.clone());
            if p.address == "first" {
                Err(ChannelFail::NotAttempted(
                    "provider refunded the assignment".into(),
                ))
            } else {
                Ok(p.address.clone())
            }
        });
        match out {
            Failover::Served {
                value,
                provider,
                attempted,
                ..
            } => {
                assert_eq!(value, "second");
                assert_eq!(provider.address, "second");
                assert_eq!(attempted, 2);
            }
            other => panic!("expected the second provider to serve, got {other:?}"),
        }
        assert_eq!(tried, vec!["first", "second"]);
    }

    /// An `Ambiguous` failure (5xx / unverifiable 200) STOPS the walk: the
    /// provider may hold a pre-signed settle, so a retry could double-charge.
    #[test]
    fn an_ambiguous_failure_never_fails_over() {
        use gpn_agent::hub_ap::ChannelFail;
        let a = prov_caps("a", "http://a", 1, "m", &["LLMInference"]);
        let b = prov_caps("b", "http://b", 2, "m", &["LLMInference"]);
        let cands = route_candidates(
            &[a, b],
            JobKind::Text,
            &RouteNeeds {
                model: "m".into(),
                ..Default::default()
            },
            &RttOverrides::new(),
        );
        let mut tried = Vec::new();
        let out: Failover<String> = failover("test-text", &cands, |p| {
            tried.push(p.address.clone());
            Err(ChannelFail::Ambiguous(
                "500 after the assignment was signed".into(),
            ))
        });
        assert_eq!(tried, vec!["a"], "must not touch the second provider");
        match out {
            Failover::Ambiguous { reason, attempted } => {
                assert_eq!(attempted, 1);
                assert!(reason.contains("500"));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// Exhaustion is a 503 that names a reason for EVERY candidate, and the walk
    /// stops after `MAX_ROUTE_ATTEMPTS` (never an unbounded retry storm).
    #[test]
    fn exhaustion_reports_every_candidate_reason_and_caps_attempts() {
        use gpn_agent::hub_ap::ChannelFail;
        let reg: Vec<Provider> = (0..5)
            .map(|i| {
                prov_caps(
                    &format!("p{i}"),
                    &format!("http://p{i}"),
                    i as u64,
                    "m",
                    &["LLMInference"],
                )
            })
            .collect();
        let cands = route_candidates(
            &reg,
            JobKind::Text,
            &RouteNeeds {
                model: "m".into(),
                ..Default::default()
            },
            &RttOverrides::new(),
        );
        assert_eq!(cands.len(), 5);
        let mut tried = 0;
        let out: Failover<String> = failover("test-exhaust", &cands, |p| {
            tried += 1;
            Err(ChannelFail::NotAttempted(format!("{} is down", p.address)))
        });
        assert_eq!(
            tried, MAX_ROUTE_ATTEMPTS,
            "at most {MAX_ROUTE_ATTEMPTS} attempts"
        );
        let reasons = match out {
            Failover::Exhausted { reasons } => reasons,
            other => panic!("expected Exhausted, got {other:?}"),
        };
        assert_eq!(reasons.len(), MAX_ROUTE_ATTEMPTS);
        assert!(reasons[0].starts_with("p0: "));
        assert!(reasons[1].starts_with("p1: "));
        let body = exhaustion_json(JobKind::Image, &reasons);
        assert_eq!(body["error"]["type"], "gpn_unavailable");
        assert_eq!(
            body["error"]["candidates"].as_array().unwrap().len(),
            MAX_ROUTE_ATTEMPTS
        );
        assert!(body["error"]["message"].as_str().unwrap().contains("image"));
    }

    /// The text facade and the image facade use the same `route_candidates`, so
    /// the ordering rules cannot drift apart again.
    #[test]
    fn text_and_image_share_the_same_pipeline() {
        let mut llm = prov_caps("llm", "http://llm", 5, "llama3.2:1b", &["LLMInference"]);
        llm.models = vec!["llama3.2:1b".into()];
        let diff = prov_caps("diff", "http://diff", 5, "", &["ImageGeneration"]);
        let reg = vec![llm, diff];
        let text = pick(&reg, 0, 0, 0, 0.0, "llama3.2:1b", 0, &RttOverrides::new());
        let img = route_candidates(
            &reg,
            JobKind::Image,
            &RouteNeeds {
                model: "sd-turbo".into(),
                ..Default::default()
            },
            &RttOverrides::new(),
        );
        assert_eq!(text.unwrap().0.address, "llm");
        assert_eq!(img[0].provider.address, "diff");
        // Same filter set: strip the diffusion capability and nothing is left.
        let mut no_cap = reg.clone();
        no_cap[1].capabilities.clear();
        assert!(route_candidates(
            &no_cap,
            JobKind::Image,
            &RouteNeeds {
                model: "sd-turbo".into(),
                ..Default::default()
            },
            &RttOverrides::new()
        )
        .is_empty());
    }
}
