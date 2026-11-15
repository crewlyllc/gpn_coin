//! Provider-side **pull** transport for job assignments (#46 Phase 1).
//!
//! Historically the access point *pushed* an assignment to a provider with
//! `POST /assign` on the provider's heartbeat (`:8093`). That requires the
//! provider to accept **inbound** connections, which excludes exactly the
//! home-GPU / CGNAT population the network is meant to grow (see
//! `docs/design/design-network-membership-bootstrap.md` §5.6): a node behind
//! NAT can sync, mine and even reach the AP outbound, yet can never be pushed
//! to.
//!
//! The pull transport inverts the flow: the provider asks the AP for its own
//! assignments (`GET /assignments?provider=…&since=…&wait=…`, a bounded
//! long-poll) and writes them into its **own** stores with the exact same
//! shape the push path uses (`~/.gpn/assignments.json` + `LlmStore`). Every
//! provider claim loop already reads those files, so nothing downstream
//! changes: run / settle / heal are untouched.
//!
//! # Mixed fleets / Phase 3
//!
//! `GPN_ASSIGN_SOURCE` selects the transport:
//!
//! * `pull` — **the only transport** since #46 Phase 3 (and the default since
//!   Phase 2). Works behind NAT/CGNAT, which is the population the network is
//!   meant to grow.
//! * `push` — **degraded**: the AP no longer pushes assignments (the heartbeat's
//!   `POST /assign` endpoint was removed in Phase 3 step 2), so a push-only
//!   provider serves only jobs whose payload is already in its local store.
//!   Logged loudly at startup.
//! * `both` — identical to `pull` (accepted so an existing config keeps working).
//!
//! The old wake hint is gone with the push path: the AP writes the lease row
//! before it would have dispatched, and our long-poll answers within ~250 ms,
//! so there is nothing left to wake.
//!
//! # Capabilities ride the poll
//!
//! The AP used to learn a provider's models/GFLOPS/VRAM/load by dialling its
//! `:8093` `/status`. That made an unreachable heartbeat a routing black hole:
//! a NAT'd box could poll for work yet never be chosen. Each poll now carries a
//! `caps` snapshot read from the LOCAL heartbeat (loopback), so the AP's
//! registry is fed entirely by the provider's outbound connection.
//!
//! # Cursor
//!
//! The poller keeps a per-provider cursor (`~/.gpn/assign-cursor.json`, the
//! highest job id it has ingested) so a restart does not re-deliver the whole
//! assignment history. The AP caps a response at [`MAX_LEASES`] leases; the
//! cursor only ever moves forward, so an id can be delivered twice (harmless —
//! the claim loop and its settled cache are idempotent) but never skipped.
//!
//! # Auth
//!
//! Providers hold no user token, so each poll is **signed by the provider's own
//! registered account** (see [`crate::assign_auth`]): `x-gpn-account`,
//! `x-gpn-ts`, `x-gpn-nonce`, `x-gpn-sig`, with the signature over
//! `gpn-assign-v1|<account>|<ts>|<nonce>|<method>|<path>|<sha256(body)>`. The
//! AP verifies it against the account's on-chain key and registration, so
//! nothing has to be distributed to a provider on another machine.
//!
//! A provider with no usable signer falls back to the legacy shared
//! `GPN_ASSIGN_TOKEN` (`x-gpn-assign-token`) and says so once; the token is
//! still sent alongside a signature when configured, so an AP running the
//! legacy `GPN_ASSIGN_AUTH=token` keeps working during the transition.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::llm::{merge_json_object, LlmJob, LlmStore};

/// Maximum leases the AP returns per response (also the AP-side cap).
pub const MAX_LEASES: usize = 20;

/// Default long-poll budget. The AP returns as soon as it has work, so this is
/// an idle heartbeat, not a delay: one request per 25 s when nothing happens.
pub const DEFAULT_WAIT_MS: u64 = 25_000;

/// Default transport when `GPN_ASSIGN_SOURCE` is unset (#46 Phase 2).
pub const DEFAULT_SOURCE: &str = "pull";

/// The canonical public access point.
///
/// A node that does not say otherwise talks to this AP, so "install and start"
/// serves AI without a second, easily-missed configuration step. Before this
/// default existed a launcher that forgot `GPN_AP_URL` produced a box that
/// synced, mined and looked healthy — and never received a single job.
pub const DEFAULT_AP_URL: &str = "http://gpn-testnet.duckdns.org:8092";

/// Environment variable naming the access point (authoritative when set).
pub const AP_URL_ENV: &str = "GPN_AP_URL";

/// A poll that succeeded this recently is treated as evidence the node is
/// actually serving (the loop polls at least every 25 s).
pub const POLL_FRESH_SECS: u64 = 300;

/// Minimum spacing between pull-health state writes, so a 25 s poll loop does
/// not rewrite a file every iteration (a lease sighting always writes).
pub const POLL_STATE_MIN_SECS: u64 = 30;

/// How the access point URL was resolved. Pure value so the
/// default/override/disable matrix is testable without touching the process
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApUrl {
    /// Set by the operator — authoritative, never replaced by the default.
    Configured(String),
    /// Not set at all → the canonical AP is used (logged once at startup).
    Defaulted(String),
    /// Explicitly set to empty (`GPN_AP_URL=`) → AI serving deliberately off.
    Disabled,
}

impl ApUrl {
    /// The URL to poll, `None` when serving is disabled.
    pub fn url(&self) -> Option<&str> {
        match self {
            ApUrl::Configured(u) | ApUrl::Defaulted(u) => Some(u.as_str()),
            ApUrl::Disabled => None,
        }
    }

    /// Does this configuration let the node pull work?
    pub fn enabled(&self) -> bool {
        self.url().is_some()
    }

    /// `true` for an explicit `GPN_AP_URL=` (disabled), not for an unset var.
    pub fn is_disabled(&self) -> bool {
        matches!(self, ApUrl::Disabled)
    }
}

/// Classify a raw `GPN_AP_URL` value.
///
/// `None` = **unset** → default. `Some("")` (or whitespace) = **explicitly
/// disabled** → `Disabled`, which is *not* the same thing as unset: an operator
/// who writes `GPN_AP_URL=` means "this node must not serve AI", and silently
/// substituting the canonical AP would overrule them.
pub fn classify_ap_url(value: Option<&str>) -> ApUrl {
    match value {
        None => ApUrl::Defaulted(DEFAULT_AP_URL.to_string()),
        Some(raw) => {
            let trimmed = raw.trim().trim_end_matches('/');
            if trimmed.is_empty() {
                ApUrl::Disabled
            } else {
                ApUrl::Configured(trimmed.to_string())
            }
        }
    }
}

/// Read `GPN_AP_URL` from the environment, distinguishing **unset** (default
/// applies) from **set to empty** (disabled). `std::env::var` cannot tell them
/// apart, which is exactly the bug this module had.
pub fn ap_url_from_env() -> ApUrl {
    match std::env::var_os(AP_URL_ENV) {
        None => classify_ap_url(None),
        Some(os) => classify_ap_url(Some(os.to_string_lossy().as_ref())),
    }
}

/// The one-time startup line for a defaulted AP URL, if this resolution needs
/// one. Kept separate from the printing so tests can assert the wording.
pub fn default_notice(ap: &ApUrl) -> Option<String> {
    match ap {
        ApUrl::Defaulted(u) => Some(format!("[assign] no GPN_AP_URL set — defaulting to {u}")),
        _ => None,
    }
}

/// The loud line for a node that cannot receive work. Emitted once per process
/// at startup, never from the poll loop.
pub const NO_AP_WARNING: &str =
    "[provider] WARNING: no access point URL — this node will NOT serve AI jobs (mining only)";

/// Local heartbeat the capability snapshot is read from. Loopback, so it works
/// on a box whose `:8093` is not reachable from anywhere else — which is the
/// whole point of #46 Phase 3 step 2.
pub const DEFAULT_STATUS_URL: &str = "http://127.0.0.1:8093/status";

/// Which transport(s) this provider uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignSource {
    /// No pull loop (degraded since #46 Phase 3: nothing is pushed any more,
    /// so only payloads already in the local store can be served).
    Push,
    /// `GET /assignments` only.
    Pull,
    /// Same as `Pull`; kept so an existing `GPN_ASSIGN_SOURCE=both` config
    /// keeps working after the push endpoint was removed.
    Both,
}

/// Legacy transport matrix, kept for callers that only know a raw URL.
///
/// The provider supervision path no longer uses this: since the AP URL has a
/// canonical default, "pull without a URL" can only mean the operator set
/// `GPN_AP_URL=` on purpose (disabled), and silently degrading to the removed
/// push path would hide that. See [`PullConfig::from_env`].
pub fn resolve(source: &str, ap_url: Option<&str>) -> AssignSource {
    let want = AssignSource::parse(source);
    if want.pulls() && ap_url.is_none() {
        AssignSource::Push
    } else {
        want
    }
}

impl AssignSource {
    /// Parse `GPN_ASSIGN_SOURCE`. `pull` is the default, and so is every value
    /// we do not recognise: a typo must never quietly pin a NAT'd provider to
    /// a transport it cannot receive on (`resolve` still falls back to push
    /// when there is no AP URL at all, so nothing is lost either way).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "push" => AssignSource::Push,
            "both" => AssignSource::Both,
            _ => AssignSource::Pull,
        }
    }

    /// Does this mode run the pull loop?
    pub fn pulls(self) -> bool {
        matches!(self, AssignSource::Pull | AssignSource::Both)
    }

    /// Does this mode keep answering pushes from an old AP? Since #46 Phase 3
    /// step 2 the heartbeat has no `/assign` endpoint at all, so this only
    /// labels the (degraded) configuration — it never decides whether work
    /// arrives.
    pub fn pushes(self) -> bool {
        matches!(self, AssignSource::Push | AssignSource::Both)
    }

    /// Stable label for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            AssignSource::Push => "push",
            AssignSource::Pull => "pull",
            AssignSource::Both => "both",
        }
    }
}

/// Configuration for the pull loop, resolved from the environment.
#[derive(Debug, Clone)]
pub struct PullConfig {
    /// AP base URL, without a trailing slash (e.g. `http://host:8092`).
    pub ap_url: String,
    /// This provider's identity, as the AP keyed the assignment record
    /// (hex `0x…` or ss58 — the AP accepts either).
    pub provider: String,
    /// Legacy shared AP↔provider secret (`GPN_ASSIGN_TOKEN`); empty = none.
    /// Only used when this process has no signer (`GPN_SIGNER` /
    /// `SIGNER_URI` / keystore).
    pub token: String,
    /// Long-poll budget in ms.
    pub wait_ms: u64,
    /// Backoff ceiling between failed polls.
    pub max_backoff: Duration,
    /// The capabilities THIS PROCESS can serve (#100).
    ///
    /// The caps snapshot is read from the machine-wide heartbeat, which
    /// describes every provider running here — so without this filter a
    /// fine-tune-only process advertises `LLMInference`/`ImageGeneration` too
    /// and the AP registers an identity it will route work to that can never
    /// claim it. Empty = no filtering (legacy behaviour).
    pub capabilities: Vec<String>,
}

impl PullConfig {
    /// Resolve from the environment. `None` when the effective transport does
    /// not pull — either because push was asked for explicitly, or because no
    /// `GPN_AP_URL` is configured.
    ///
    /// #46 Phase 3 removed the push *delivery* path, so a non-pulling provider
    /// receives no assignments from a remote AP at all: it can only serve jobs
    /// whose payload already exists in its local store (i.e. one sharing the
    /// AP's `~/.gpn`) plus whatever it wins by first-claim on chain. That is a
    /// degraded mode, so it is logged loudly rather than silently.
    pub fn from_env(provider: &str) -> Option<Self> {
        let raw = std::env::var("GPN_ASSIGN_SOURCE").unwrap_or_else(|_| DEFAULT_SOURCE.into());
        let ap = ap_url_from_env();
        // Once, at startup: say that the canonical AP is being assumed. This is
        // the line that used to be missing entirely, which made a miner-only
        // box look perfectly healthy.
        if let Some(msg) = default_notice(&ap) {
            eprintln!("{msg}");
        }
        let source = AssignSource::parse(&raw);
        if !source.pulls() {
            eprintln!(
                "⚠ [assign] NO pull transport (GPN_ASSIGN_SOURCE={} selects push only) — \
                 assignment PUSH was removed in #46 Phase 3, so this provider will only serve \
                 jobs whose payload is already in its local store; set GPN_AP_URL (e.g. \
                 http://<ap-host>:8092) to receive work.",
                raw.trim()
            );
            return None;
        }
        let ap_url = match ap {
            ApUrl::Configured(u) | ApUrl::Defaulted(u) => u,
            ApUrl::Disabled => {
                // `GPN_AP_URL=` is an explicit operator decision, and since the
                // push path is gone it means this box will sit idle forever.
                eprintln!("{NO_AP_WARNING}");
                return None;
            }
        };
        Some(Self {
            ap_url,
            provider: provider.to_string(),
            token: std::env::var("GPN_ASSIGN_TOKEN").unwrap_or_default(),
            wait_ms: std::env::var("GPN_ASSIGN_WAIT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_WAIT_MS),
            max_backoff: Duration::from_secs(30),
            capabilities: Vec::new(),
        })
    }

    /// Declare what this process can actually serve, so its poll advertises the
    /// truth about *this* provider rather than the whole host (#100).
    pub fn with_capabilities(mut self, caps: &[&str]) -> Self {
        self.capabilities = caps.iter().map(|c| c.to_string()).collect();
        self
    }

    /// The request target (path + query) for a cursor position.
    ///
    /// This exact string is what the signature covers — it carries `provider`,
    /// so leaving it out of the signed message would let any caller read
    /// another provider's assignments.
    ///
    /// `take=false` adds `take=0`: "refresh my capabilities and hand me any lease
    /// you already wrote for me, but do not consider me for new work". The
    /// provider sends it when it cannot serve right now — see [`serve_gate`].
    pub fn poll_target(&self, since: u64, caps: &str, take: bool) -> String {
        let mut target = format!(
            "/assignments?provider={}&since={}&wait={}",
            urlencode(&self.provider),
            since,
            self.wait_ms
        );
        if !take {
            target.push_str("&take=0");
        }
        if !caps.is_empty() {
            target.push_str("&caps=");
            target.push_str(&urlencode(caps));
        }
        target
    }

    /// Absolute poll URL, carrying this provider's capabilities (`caps`) so the
    /// AP can route to us without ever dialling our `:8093` (#46 Phase 3
    /// step 2).
    pub fn poll_url(&self, since: u64, caps: &str, take: bool) -> String {
        format!("{}{}", self.ap_url, self.poll_target(since, caps, take))
    }
}

/// Snapshot this provider's capabilities for the poll.
///
/// The AP used to learn them by dialling the provider's `/status` heartbeat,
/// which needs an inbound port and therefore silently dropped every NAT'd /
/// CGNAT box from routing (it could poll and work, but never be picked). We
/// read the **local** heartbeat over loopback and ship the same JSON in the
/// poll, so the AP's registry is fed by the provider's own outbound connection.
///
/// **Silence, not a negative answer, when the local heartbeat cannot be read.**
/// An empty return means "this poll carries no capability data" and
/// `poll_target` then omits the `caps` parameter entirely, so the AP keeps the
/// record it already has. That is deliberately different from a snapshot whose
/// capability list is *empty*: `filter_capabilities` marks those
/// `caps_declared: true`, and the AP treats them as "this box serves none of
/// the kinds it advertises" (fail closed, #100) and replaces the entry's
/// `capabilities`/`gflops` with nothing. Before 2026-11-05 a failed fetch
/// produced exactly that (empty object → declared-empty list), so a `/status`
/// read slower than the 1.5 s timeout silently un-advertised the box's
/// capabilities and its attested GFLOPS until the next good poll (#141).
pub fn caps_snapshot(provider: &str) -> String {
    caps_snapshot_with(provider, &[])
}

/// Same, but narrowed to the capabilities this process can serve.
///
/// The heartbeat is a property of the **host**; the poll speaks for one
/// **process**. Sending the host's full capability set made every co-located
/// provider look like it could run every job kind, so the AP routed chat work
/// to a fine-tune-only identity (and `/providers` listed the box twice with the
/// same five capabilities). `own` empty keeps the old behaviour for any caller
/// that has not declared a capability set.
pub fn caps_snapshot_with(provider: &str, own: &[String]) -> String {
    snapshot(provider, own)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// The local heartbeat snapshot the poll carries, or `None` when it cannot be
/// read (see [`caps_snapshot_with`] for why silence ≠ "serves nothing").
///
/// Public because the poll loop needs the SAME read to decide whether it can
/// serve: `active`/`capacity` come out of this very JSON, and fetching it twice
/// per poll would let the two reads disagree.
pub fn snapshot(provider: &str, own: &[String]) -> Option<Value> {
    // No local heartbeat = no data. Say nothing rather than "I serve nothing":
    // the AP must never be told that a failed read is a deliberate empty set.
    let mut v = match fetch_heartbeat(provider) {
        Some(v) => v,
        None => {
            note_caps_unavailable();
            return None;
        }
    };
    if let Some(o) = v.as_object_mut() {
        // Identity of the polling PROCESS, so the AP can aggregate several
        // co-located providers on one address instead of letting the last
        // poll win (#100).
        o.insert(
            "caps_source".into(),
            Value::String(std::process::id().to_string()),
        );
    }
    filter_capabilities(&mut v, own);
    Some(v)
}

/// Why this provider must **not** take new work right now (owner rule,
/// 2026-11-15): "the node decides for itself whether to pull".
///
/// The access point is no longer allowed to rank providers (no latency, no busy
/// penalty, no active-count penalty), so the *only* thing standing between a
/// busy box and a second job is the box itself. This is that decision:
///
/// * `at-capacity` — the local heartbeat says `active >= capacity` (one GPU =
///   one job unless the operator raised capacity).
/// * `gpu-busy` — the cross-process GPU lock is held, i.e. a job, a diffusion
///   render or another local workload is using the GPU (`gpu_yield`). The
///   miner's yield flag is *not* this: only a real GPU section takes the lock.
/// * `operator-idle` — `~/.gpn/idle.json` says the operator wants the GPU back
///   (dashboard idle / "gaming"), which also stops the claim loop; pulling
///   work the claim loop would refuse is how a job gets leased to a box that
///   will never accept it.
///
/// Pure so it is testable: the caller supplies the three facts.
pub fn serve_gate(
    active: u64,
    capacity: u64,
    claims_allowed: bool,
    gpu_locked: bool,
) -> Option<&'static str> {
    if !claims_allowed {
        return Some("operator-idle");
    }
    if gpu_locked {
        return Some("gpu-busy");
    }
    if capacity > 0 && active >= capacity {
        return Some("at-capacity");
    }
    None
}

/// Keep only the capabilities this process declared, and mark the list as
/// deliberate so an empty result is never "unknown, assume everything".
pub fn filter_capabilities(v: &mut Value, own: &[String]) {
    if own.is_empty() {
        return;
    }
    let have: Vec<String> = v
        .get("capabilities")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let kept: Vec<String> = own
        .iter()
        .filter(|c| have.iter().any(|h| h.eq_ignore_ascii_case(c)))
        .cloned()
        .collect();
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "capabilities".into(),
            Value::Array(kept.into_iter().map(Value::String).collect()),
        );
        o.insert("caps_declared".into(), Value::Bool(true));
    }
}

/// Rate-limited note that a poll is going out without capability data.
///
/// This used to be silent: the snapshot was built from an empty object, so
/// nothing said "the local /status did not answer" and the only visible symptom
/// was an AP that had quietly lost the box's capabilities (#141). One line per
/// 5 minutes is enough to notice a heartbeat that has stopped answering without
/// flooding a 2 s poll loop.
fn note_caps_unavailable() {
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<Option<std::time::Instant>>> = OnceLock::new();
    let m = LAST.get_or_init(|| Mutex::new(None));
    let mut g = match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let now = std::time::Instant::now();
    let due = g
        .map(|t| now.duration_since(t).as_secs() >= 300)
        .unwrap_or(true);
    if due {
        *g = Some(now);
        eprintln!(
            "[assign] local /status unreadable — this poll carries no caps (the AP keeps its \
             last record; publish nothing rather than a false 'serves nothing')"
        );
    }
}

/// Say it when the serve gate **changes**, and only then: one line when the box
/// stops taking work and one when it resumes. A 2 s poll loop must not flood the
/// journal, and an operator needs to see the transition ("my box stopped
/// getting jobs" is otherwise invisible from this side).
fn note_serve_gate(blocked: Option<&'static str>) {
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<Option<Option<&'static str>>>> = OnceLock::new();
    let m = LAST.get_or_init(|| Mutex::new(None));
    let mut g = match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if *g == Some(blocked) {
        return;
    }
    let first = g.is_none();
    *g = Some(blocked);
    match blocked {
        Some(why) => println!(
            "[assign] not taking new work: {why} — polling with take=0 (caps still refreshed; \
             leases already written for this provider are still delivered)"
        ),
        None if !first => println!("[assign] taking work again (serve gate clear)"),
        None => {}
    }
}

/// Fetch the machine-wide heartbeat JSON, or `None` when it could not be read
/// (connection error, timeout, non-JSON body, JSON that is not an object).
/// `None` is "unknown", which every caller must keep distinct from "empty".
fn fetch_heartbeat(provider: &str) -> Option<Value> {
    let url = std::env::var("GPN_STATUS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_STATUS_URL.to_string());
    let fetched = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
        .ok()
        .and_then(|c| c.get(&url).send().ok())
        .and_then(|r| r.json::<Value>().ok());
    let mut v = match fetched {
        Some(Value::Object(m)) => Value::Object(m),
        _ => return None,
    };
    if let Some(o) = v.as_object_mut() {
        if o.get("address")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .is_empty()
        {
            o.insert("address".into(), Value::String(provider.to_string()));
        }
        o.entry("capacity").or_insert(serde_json::json!(1));
        o.insert("poll".into(), serde_json::json!(true));
    }
    Some(v)
}

/// Minimal percent-encoding for a query value (providers are `0x…`/base58, so
/// only `+`/`/`/`=`-class characters need escaping, but be safe).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A parsed lease from the AP.
#[derive(Debug, Clone)]
pub struct Lease {
    /// On-chain job id.
    pub job_id: u64,
    /// Model requested (may be `auto`/empty).
    pub model: String,
    /// Whether the provider should pull the model first.
    pub pull: bool,
    /// Where to POST the finished result.
    pub callback: String,
    /// Per-job token the AP uses to authenticate our result POST.
    pub callback_token: String,
    /// The full off-chain payload, when the AP still holds it.
    pub job: Option<LlmJob>,
}

/// Parse an AP `/assignments` response body.
///
/// Accepts `{"leases":[…]}` (documented) and a bare array (defensive), and
/// skips malformed entries instead of failing the whole poll — one bad lease
/// must never stop a provider from serving the rest.
pub fn parse_leases(body: &str) -> Vec<Lease> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return vec![];
    };
    let arr = match &v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o
            .get("leases")
            .and_then(|l| l.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => vec![],
    };
    arr.iter()
        .filter_map(|l| {
            let job_id = l.get("job_id").and_then(|x| x.as_u64())?;
            let job = l
                .get("job")
                .and_then(|j| serde_json::from_value::<LlmJob>(j.clone()).ok());
            Some(Lease {
                job_id,
                model: l
                    .get("model")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                pull: l.get("pull").and_then(|x| x.as_bool()).unwrap_or(false),
                callback: l
                    .get("callback")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                callback_token: l
                    .get("callback_token")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                job,
            })
        })
        .collect()
}

/// Persist one lease exactly what the old push path wrote, so every downstream
/// consumer (claim loops, `run_job`, result callback) is unchanged.
///
/// Returns whether the off-chain payload was stored.
pub fn apply_lease(lease: &Lease, provider: &str, assignment_store: &std::path::Path) -> bool {
    apply_lease_to(lease, provider, assignment_store, &LlmStore::new())
}

/// Same as [`apply_lease`] against an explicit payload store (testable, and
/// usable if a provider ever runs a non-default store path).
pub fn apply_lease_to(
    lease: &Lease,
    provider: &str,
    assignment_store: &std::path::Path,
    store: &LlmStore,
) -> bool {
    let mut stored_job = false;
    if let Some(job) = &lease.job {
        // Never overwrite a payload we already hold: OUR copy is the newer one
        // (it carries the response/status this provider wrote). A re-delivered
        // lease must not reset a finished job to `queued`.
        if store.get(lease.job_id).is_none() {
            match store.put(lease.job_id, job.clone()) {
                Ok(()) => stored_job = true,
                // Never silent: the caller treats `false` as "no payload" and
                // skips the job, so the reason has to be in the log or the
                // failure looks like a routing bug (#185, box 1: every write
                // to a write-denied store path was discarded here).
                Err(e) => eprintln!("{}", put_failure_note(lease.job_id, store.path(), &e)),
            }
        } else {
            stored_job = true;
        }
    }
    let rec = serde_json::json!({
        "provider": provider,
        "model": lease.model,
        "pull": lease.pull,
        "callback": lease.callback,
        "callback_token": lease.callback_token,
    });
    let _ = merge_json_object(assignment_store, &lease.job_id.to_string(), rec);
    stored_job
}
/// The one line printed when a lease payload cannot be persisted.
///
/// Split out so the wording (which names the *file*, not just the failure) is
/// covered by a test: #185 was hours of work because the write error was thrown
/// away and the only symptom was "payload missing".
pub fn put_failure_note(job_id: u64, path: &Path, err: &std::io::Error) -> String {
    format!(
        "[assign] job #{job_id} could not be stored to {} ({err}) - the provider cannot run it \
         and will leave it Pending",
        path.display()
    )
}

/// Path of the shared assignments file (`$HOME/.gpn/assignments.json`).
///
/// Resolved through [`crate::firstrun::state_home`] so a Windows task-started
/// process without `HOME` still lands in `%USERPROFILE%\.gpn` instead of the
/// working directory (#185).
pub fn assignment_store_path() -> PathBuf {
    crate::firstrun::state_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gpn/assignments.json")
}

fn cursor_path() -> PathBuf {
    crate::firstrun::state_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gpn/assign-cursor.json")
}

/// Read the persisted cursor for `provider`. `None` when no cursor exists yet
/// (first run), so the caller can seed from the local store instead of
/// replaying the whole assignment history.
pub fn read_cursor(provider: &str) -> Option<u64> {
    let s = std::fs::read_to_string(cursor_path()).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    v.get(provider).and_then(|x| x.as_u64())
}

/// Highest job id already present in the provider's local assignment store —
/// the correct starting cursor when no cursor file exists, because those
/// assignments were already delivered (by push, before this build existed).
pub fn seed_cursor_from_store(store: &std::path::Path) -> u64 {
    std::fs::read_to_string(store)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.parse::<u64>().ok())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Persist the cursor for `provider`. Best-effort: a lost cursor only means one
/// re-delivery, which the settled cache absorbs.
pub fn write_cursor(provider: &str, since: u64) {
    let path = cursor_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut map = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    map.insert(provider.to_string(), Value::from(since));
    let body = Value::Object(map).to_string();
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        if f.write_all(body.as_bytes()).is_ok() && f.sync_all().is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Spawn the background poll thread. Returns immediately; the thread runs for
/// the life of the process and never panics its parent.
///
/// Call this from a provider binary **after** its own identity is known and
/// before (or alongside) its claim loop.
pub fn spawn(cfg: PullConfig) {
    let label = cfg.provider.clone();
    let _ = std::thread::Builder::new()
        .name("gpn-assign-pull".into())
        .spawn(move || run_loop(cfg, label))
        .map_err(|e| eprintln!("[assign] could not start pull thread: {e}"));
}

/// The poll loop. Public for integration tests and for a `--once` diagnostic.
pub fn run_loop(cfg: PullConfig, provider_label: String) {
    let store = assignment_store_path();
    let mut since = match read_cursor(&cfg.provider) {
        Some(c) => c,
        None => {
            // First run of the pull transport: everything already in our store
            // was delivered by push, so start after it instead of replaying it.
            let seeded = seed_cursor_from_store(&store);
            if seeded > 0 {
                write_cursor(&cfg.provider, seeded);
            }
            seeded
        }
    };
    let mut backoff = Duration::from_secs(1);
    let mut announced = false;
    let mut errors: u64 = 0;
    // Signed pulls: the same key the push heartbeat uses. Resolved once, so a
    // provider either always signs or always falls back to the legacy token.
    let signer = crate::assign_auth::signer_from_env();
    let account = signer
        .as_ref()
        .and_then(|_| crate::assign_auth::account_ss58(&cfg.provider));
    if signer.is_some() && account.is_none() {
        eprintln!(
            "[assign] provider {:?} is not a decodable account — cannot sign pulls",
            short(&provider_label)
        );
    }
    let auth_label = if signer.is_some() && account.is_some() {
        "signature"
    } else {
        if cfg.token.is_empty() {
            eprintln!(
                "[assign] no signer (GPN_SIGNER / SIGNER_URI / keystore) and no \
                 GPN_ASSIGN_TOKEN — a remote AP will reject this pull; run \
                 `gpn_node --ensure-wallet` or set a signer"
            );
        } else {
            eprintln!(
                "[assign] no signer — falling back to the legacy GPN_ASSIGN_TOKEN \
                 (deprecated: signed pulls need GPN_SIGNER / SIGNER_URI / a keystore)"
            );
        }
        "token"
    };
    println!(
        "[assign] pull transport starting — ap={} provider={} since={} wait={}ms auth={}",
        cfg.ap_url,
        short(&provider_label),
        since,
        cfg.wait_ms,
        auth_label
    );
    loop {
        // Fresh caps every poll: `active` (the one-GPU-one-job lock) and the
        // model list change minute to minute, so a cached snapshot would route
        // work to a box that is mid-job.
        //
        // The same read decides whether WE are willing to take work (owner rule,
        // 2026-11-15): since the AP no longer ranks providers by load, a busy
        // box that keeps polling would be handed jobs it cannot run. The gate is
        // evaluated here, on the node, and its verdict travels in the poll
        // (`take=0`), so the AP learns our decision without having to guess.
        let snap = snapshot(&cfg.provider, &cfg.capabilities);
        let caps = snap.as_ref().map(|v| v.to_string()).unwrap_or_default();
        let field = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let (active, capacity) = snap
            .as_ref()
            .map(|v| (field(v, "active"), field(v, "capacity")))
            .unwrap_or((0, 0));
        let blocked = serve_gate(
            active,
            capacity,
            crate::idle::claims_allowed(),
            crate::gpu_yield::lock_is_held(),
        );
        let take = blocked.is_none();
        note_serve_gate(blocked);
        let target = cfg.poll_target(since, &caps, take);
        let url = format!("{}{target}", cfg.ap_url);
        let client = match crate::net::ipv4_only_client(Duration::from_millis(cfg.wait_ms + 10_000))
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[assign] http client build failed: {e}");
                std::thread::sleep(backoff);
                continue;
            }
        };
        let mut req = client.get(&url);
        // Signed pull: GET with an empty body, over the exact target (query
        // included) that is about to be sent.
        if let (Some(signer), Some(account)) = (&signer, &account) {
            if let Some(nonce) = crate::assign_auth::new_nonce() {
                for (name, value) in crate::assign_auth::sign_headers(
                    signer,
                    account,
                    "GET",
                    &target,
                    b"",
                    unix_now(),
                    &nonce,
                ) {
                    req = req.header(name, value);
                }
            }
        }
        // Sent alongside a signature too: an AP still running the legacy
        // `GPN_ASSIGN_AUTH=token` ignores the extra headers and keeps working.
        if !cfg.token.is_empty() {
            req = req.header("x-gpn-assign-token", &cfg.token);
        }
        match req.send() {
            Ok(resp) if resp.status().is_success() => {
                backoff = Duration::from_secs(1);
                let body = resp.text().unwrap_or_default();
                let leases = parse_leases(&body);
                if !announced {
                    announced = true;
                    println!("[assign] pull transport active (AP answered on first poll)");
                }
                let mut newest = since;
                for lease in &leases {
                    let stored = apply_lease(lease, &cfg.provider, &store);
                    println!(
                        "[assign] pulled #{} (payload {}): {}",
                        lease.job_id,
                        if stored { "stored" } else { "missing" },
                        short(&lease.model)
                    );
                    newest = newest.max(lease.job_id);
                }
                if newest > since {
                    since = newest;
                    write_cursor(&cfg.provider, since);
                }
                // Evidence for the dashboard: the AP really answered. Throttled
                // inside `record_poll_ok`, so this is not per-poll disk churn.
                record_poll_ok(
                    &cfg.provider,
                    &cfg.ap_url,
                    unix_now(),
                    leases.iter().map(|l| l.job_id).max(),
                );
            }
            Ok(resp) => {
                // 401 = our token does not match the AP's. That will never fix
                // itself, so say it loudly once and then back off hard.
                errors += 1;
                if resp.status().as_u16() == 401 {
                    // The AP names the reason (`unregistered`, `stale_ts`, …);
                    // surface it — a bare 401 cost a day on box 1.
                    let body = resp.text().unwrap_or_default();
                    let reason = serde_json::from_str::<Value>(&body)
                        .ok()
                        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
                        .unwrap_or_else(|| body.chars().take(80).collect());
                    eprintln!(
                        "[assign] AP rejected the pull (401): {reason} — this provider's account \
                         must be a registered provider on chain (and its clock within 90s)"
                    );
                } else if errors <= 3 || errors % 20 == 0 {
                    eprintln!("[assign] AP answered {} (attempt {errors})", resp.status());
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(cfg.max_backoff);
            }
            Err(e) => {
                errors += 1;
                if errors <= 3 || errors % 20 == 0 {
                    eprintln!("[assign] pull failed (attempt {errors}): {e}");
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(cfg.max_backoff);
            }
        }
    }
}

/// Unix seconds. One definition so the poll loop and the dashboard agree.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Path of the pull-health file (`$HOME/.gpn/assign-pull-state.json`).
///
/// This is the *evidence* the dashboard reports on: a timestamp written only
/// after the AP actually answered a poll. Without it the dashboard could only
/// guess, and "configured" is not the same as "serving".
pub fn pull_state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(format!("{home}/.gpn/assign-pull-state.json"))
}

/// One provider's last confirmed poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollState {
    /// AP the provider polled.
    pub ap_url: String,
    /// Unix seconds of the last 2xx poll.
    pub last_ok: u64,
    /// Highest job id seen for this provider, when one was ever delivered.
    pub last_lease: Option<u64>,
}

fn read_state_map() -> serde_json::Map<String, Value> {
    std::fs::read_to_string(pull_state_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Parse one provider's entry from the state document.
pub fn poll_state_of(map: &serde_json::Map<String, Value>, provider: &str) -> Option<PollState> {
    let e = map.get(provider)?;
    Some(PollState {
        ap_url: e.get("ap_url").and_then(|x| x.as_str())?.to_string(),
        last_ok: e.get("last_ok").and_then(|x| x.as_u64())?,
        last_lease: e.get("last_lease").and_then(|x| x.as_u64()),
    })
}

/// Read one provider's last confirmed poll (best-effort; `None` = no evidence).
pub fn read_poll_state(provider: &str) -> Option<PollState> {
    poll_state_of(&read_state_map(), provider)
}

/// The most recent confirmed poll across every provider on this host. This is
/// what the dashboard reports: one provider serving is a serving node.
pub fn last_poll_ok() -> Option<u64> {
    read_state_map()
        .values()
        .filter_map(|e| e.get("last_ok").and_then(|x| x.as_u64()))
        .max()
}

/// Throttle decision for [`record_poll_ok`], pure so it is testable: a lease
/// sighting always records, an idle poll at most once per
/// [`POLL_STATE_MIN_SECS`].
pub fn should_record(prev: Option<&PollState>, now: u64, lease: Option<u64>) -> bool {
    match prev {
        None => true,
        Some(p) => lease.is_some() || now.saturating_sub(p.last_ok) >= POLL_STATE_MIN_SECS,
    }
}

/// Record a successful poll (best-effort, atomic). Called from the poll loop
/// after the AP answered — never from a failed request, so the dashboard's
/// "ok" always means a real 2xx.
pub fn record_poll_ok(provider: &str, ap_url: &str, now: u64, lease: Option<u64>) {
    let path = pull_state_path();
    let mut map = read_state_map();
    let prev = poll_state_of(&map, provider);
    if !should_record(prev.as_ref(), now, lease) {
        return;
    }
    let last_lease = lease.or_else(|| prev.as_ref().and_then(|p| p.last_lease));
    map.insert(
        provider.to_string(),
        serde_json::json!({
            "ap_url": ap_url,
            "last_ok": now,
            "last_lease": last_lease,
        }),
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        if f.write_all(Value::Object(map).to_string().as_bytes())
            .is_ok()
            && f.sync_all().is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Derive the dashboard verdict from real state. Pure: `enabled` comes from the
/// AP-URL resolution, `last_ok` from the poll loop's evidence.
pub fn serving_reason(enabled: bool, last_ok: Option<u64>, now: u64) -> (&'static str, bool) {
    if !enabled {
        return ("no ap url", false);
    }
    match last_ok {
        Some(t) if now.saturating_sub(t) <= POLL_FRESH_SECS => ("ok", true),
        Some(_) => ("not polling", true),
        None => ("configured, not confirmed", false),
    }
}

/// The `serving` block of `GET /api/status` for the local dashboard.
pub fn serving_status(now: u64) -> Value {
    let ap = ap_url_from_env();
    let last_ok = last_poll_ok();
    let (reason, registered) = serving_reason(ap.enabled(), last_ok, now);
    serde_json::json!({
        "enabled": ap.enabled(),
        "ap_url": ap.url().unwrap_or(""),
        "registered": registered,
        "reason": reason,
        "last_poll_ok": last_ok,
    })
}

/// First 10 chars + `…` for log lines (keeps journals readable).
fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}…", &s[..10])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_parsing_defaults_to_pull_and_falls_back_without_an_ap() {
        assert_eq!(AssignSource::parse("push"), AssignSource::Push);
        assert_eq!(AssignSource::parse(" PULL "), AssignSource::Pull);
        assert_eq!(AssignSource::parse("both"), AssignSource::Both);
        // #46 Phase 2: pull is the default, and a typo must not leave a NAT'd
        // provider pinned to a transport it cannot receive on.
        assert_eq!(AssignSource::parse("pul"), AssignSource::Pull);
        assert_eq!(AssignSource::parse(""), AssignSource::Pull);
        assert!(AssignSource::Both.pulls() && AssignSource::Both.pushes());
        assert!(!AssignSource::Push.pulls());
        assert!(!AssignSource::Pull.pushes());
        assert_eq!(DEFAULT_SOURCE, "pull");
    }

    #[test]
    fn a_missing_ap_url_degrades_to_push_instead_of_polling_nothing() {
        let ap = Some("http://ap:8092");
        // With an AP: the requested mode is honoured.
        assert_eq!(resolve("pull", ap), AssignSource::Pull);
        assert_eq!(resolve("both", ap), AssignSource::Both);
        assert_eq!(resolve("push", ap), AssignSource::Push);
        assert_eq!(resolve("", ap), AssignSource::Pull);
        // Legacy matrix without one (only reachable for callers that pass a raw
        // Option<&str>, not the env-resolving from_env).
        assert_eq!(resolve("pull", None), AssignSource::Push);
        assert_eq!(resolve("both", None), AssignSource::Push);
        assert_eq!(resolve("", None), AssignSource::Push);
        assert_eq!(resolve("push", None), AssignSource::Push);
    }

    // ── Access point URL: default / override / disable ───────────────────────

    #[test]
    fn unset_ap_url_defaults_to_the_canonical_access_point() {
        assert_eq!(
            classify_ap_url(None),
            ApUrl::Defaulted(DEFAULT_AP_URL.to_string())
        );
        assert_eq!(DEFAULT_AP_URL, "http://gpn-testnet.duckdns.org:8092");
        assert!(classify_ap_url(None).enabled());
        assert_eq!(
            default_notice(&classify_ap_url(None)).as_deref(),
            Some(format!("[assign] no GPN_AP_URL set — defaulting to {DEFAULT_AP_URL}").as_str())
        );
    }

    #[test]
    fn an_explicit_ap_url_wins_over_the_default() {
        let ap = classify_ap_url(Some(" http://10.0.0.5:8092/ "));
        assert_eq!(ap, ApUrl::Configured("http://10.0.0.5:8092".into()));
        assert_eq!(ap.url(), Some("http://10.0.0.5:8092"));
        assert!(default_notice(&ap).is_none(), "no default notice when set");
        assert!(!ap.is_disabled());
    }

    #[test]
    fn an_empty_ap_url_disables_serving_and_warns_loudly() {
        assert_eq!(classify_ap_url(Some("")), ApUrl::Disabled);
        assert_eq!(classify_ap_url(Some("   ")), ApUrl::Disabled);
        assert!(!classify_ap_url(Some("")).enabled());
        assert!(classify_ap_url(Some("")).is_disabled());
        assert!(
            default_notice(&ApUrl::Disabled).is_none(),
            "disabled is not 'unset': no default notice"
        );
        assert!(NO_AP_WARNING.contains("NOT serve AI jobs (mining only)"));
    }

    #[test]
    fn from_env_applies_the_default_keeps_overrides_and_honours_disable() {
        // `GPN_AP_URL`/`GPN_ASSIGN_SOURCE` are process-global: hold the crate-wide
        // env lock so no other test observes these values mid-test.
        let _g = crate::test_env::lock_env();
        let me = "5ProviderDefault";

        std::env::remove_var(AP_URL_ENV);
        std::env::remove_var("GPN_ASSIGN_SOURCE");
        let cfg = PullConfig::from_env(me).expect("unset → default AP, pull enabled");
        assert_eq!(cfg.ap_url, DEFAULT_AP_URL);

        std::env::set_var(AP_URL_ENV, "http://ap.internal:9000/");
        let cfg = PullConfig::from_env(me).expect("explicit URL wins");
        assert_eq!(cfg.ap_url, "http://ap.internal:9000");

        // Explicit empty: disabled — no pull loop, and NOT quietly defaulted.
        std::env::set_var(AP_URL_ENV, "");
        assert!(PullConfig::from_env(me).is_none());

        // Push-only is still possible but never the default.
        std::env::set_var(AP_URL_ENV, "http://ap.internal:9000");
        std::env::set_var("GPN_ASSIGN_SOURCE", "push");
        assert!(PullConfig::from_env(me).is_none());

        std::env::remove_var(AP_URL_ENV);
        std::env::remove_var("GPN_ASSIGN_SOURCE");
    }

    // ── Poll evidence + dashboard verdict ────────────────────────────────────

    #[test]
    fn poll_state_is_throttled_so_the_loop_does_not_churn_the_disk() {
        let prev = PollState {
            ap_url: "http://ap:8092".into(),
            last_ok: 1_000,
            last_lease: Some(7),
        };
        // Idle poll 2 s later: no write.
        assert!(!should_record(Some(&prev), 1_002, None));
        // A lease sighting always records.
        assert!(should_record(Some(&prev), 1_002, Some(8)));
        // Idle poll past the throttle window records.
        assert!(should_record(Some(&prev), 1_030, None));
        // First ever success records.
        assert!(should_record(None, 5, None));
        // And a lease that arrives with no prior state records too.
        assert!(should_record(None, 5, Some(1)));
    }

    #[test]
    fn poll_state_round_trips_and_feeds_the_dashboard() {
        let dir = std::env::temp_dir().join(format!("gpn-pollstate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Redirect HOME so nothing touches the live ~/.gpn. The lock is
        // crate-wide: `firstrun` used to point HOME at /home/tester here and
        // this write silently landed in a nonexistent directory.
        let _g = crate::test_env::lock_env();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &dir);

        let now = 1_700_000_000u64;
        record_poll_ok("5P", DEFAULT_AP_URL, now, Some(42));
        let st = read_poll_state("5P").expect("state recorded");
        assert_eq!(st.ap_url, DEFAULT_AP_URL);
        assert_eq!(st.last_ok, now);
        assert_eq!(st.last_lease, Some(42));
        assert_eq!(last_poll_ok(), Some(now));

        // The reason matrix the dashboard renders.
        assert_eq!(serving_reason(false, Some(now), now), ("no ap url", false));
        assert_eq!(serving_reason(true, Some(now), now), ("ok", true));
        assert_eq!(
            serving_reason(true, Some(now), now + POLL_FRESH_SECS),
            ("ok", true),
            "the boundary is inclusive"
        );
        assert_eq!(
            serving_reason(true, Some(now), now + POLL_FRESH_SECS + 1),
            ("not polling", true),
            "stale evidence is not 'ok'"
        );
        assert_eq!(
            serving_reason(true, None, now),
            ("configured, not confirmed", false),
            "never claim 'ok' without a 2xx on record"
        );

        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serving_status_has_the_documented_shape() {
        let _g = crate::test_env::lock_env();
        let old = std::env::var_os(AP_URL_ENV);
        std::env::set_var(AP_URL_ENV, "");
        let v = serving_status(unix_now());
        assert_eq!(v["enabled"], serde_json::json!(false));
        assert_eq!(v["ap_url"], serde_json::json!(""));
        assert_eq!(v["registered"], serde_json::json!(false));
        assert_eq!(v["reason"], serde_json::json!("no ap url"));
        match old {
            Some(o) => std::env::set_var(AP_URL_ENV, o),
            None => std::env::remove_var(AP_URL_ENV),
        }
    }

    #[test]
    fn poll_url_encodes_provider_and_carries_cursor() {
        let cfg = PullConfig {
            ap_url: "http://ap:8092".into(),
            provider: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into(),
            token: String::new(),
            wait_ms: 25000,
            max_backoff: Duration::from_secs(30),
            capabilities: Vec::new(),
        };
        assert_eq!(
            cfg.poll_url(42, "", true),
            "http://ap:8092/assignments?provider=5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY&since=42&wait=25000"
        );
        // #46 Phase 3 step 2: caps ride the poll, percent-encoded.
        assert_eq!(
            cfg.poll_url(42, r#"{"gflops":2500}"#, true),
            "http://ap:8092/assignments?provider=5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY&since=42&wait=25000&caps=%7B%22gflops%22%3A2500%7D"
        );
        assert_eq!(urlencode("0xab/cd+ef"), "0xab%2Fcd%2Bef");
    }

    /// The node-side decision the owner rule puts in the provider's hands: since
    /// the access point no longer ranks by load or busy state (2026-11-15), a box
    /// that cannot serve is the only thing that can keep it from being handed
    /// work. Each of the three reasons is separate and testable.
    #[test]
    fn a_provider_that_cannot_serve_does_not_ask_for_work() {
        // Idle (healthy, empty GPU, free slot) -> may take work.
        assert_eq!(serve_gate(0, 4, true, false), None);
        // Capacity reached: one GPU, one job.
        assert_eq!(serve_gate(1, 1, true, false), Some("at-capacity"));
        // Unknown capacity (0) is NOT "full" — fail open, the AP's own capacity
        // gate still applies.
        assert_eq!(serve_gate(3, 0, true, false), None);
        // GPU held by a mining seal / another local workload.
        assert_eq!(serve_gate(0, 4, true, true), Some("gpu-busy"));
        // Operator set the dashboard to idle / not accepting work.
        assert_eq!(serve_gate(0, 4, false, false), Some("operator-idle"));
        // Operator idle wins the reason even if the GPU also looks busy: the
        // operator's intent is the more useful line in the log.
        assert_eq!(serve_gate(4, 4, false, true), Some("operator-idle"));
    }

    /// A blocked provider's poll says so (`take=0`), and an unblocked one does
    /// not — the flag is what removes it from the AP's waiting queue.
    #[test]
    fn the_poll_carries_whether_this_box_will_take_work() {
        let cfg = PullConfig {
            ap_url: "http://ap:8092".into(),
            provider: "5Test".into(),
            token: String::new(),
            wait_ms: 1000,
            max_backoff: Duration::from_secs(30),
            capabilities: Vec::new(),
        };
        assert!(!cfg.poll_target(7, "", true).contains("take=0"));
        assert!(cfg.poll_target(7, "", false).contains("&take=0"));
        // Caps still ride a blocked poll: the AP must keep an accurate view of a
        // box that is temporarily unavailable (it is not offline).
        let blocked = cfg.poll_target(7, r#"{"active":1}"#, false);
        assert!(blocked.contains("take=0") && blocked.contains("caps="));
    }

    #[test]
    fn a_failed_local_status_read_publishes_no_caps_at_all() {
        let _g = crate::test_env::lock_env();
        // Point at a dead port so the heartbeat fetch fails deterministically.
        std::env::set_var("GPN_STATUS_URL", "http://127.0.0.1:1/status");
        let s = caps_snapshot_with(
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY",
            &["FineTuning".to_string()],
        );
        // (#141) An unreadable heartbeat is UNKNOWN, not "serves nothing":
        // an empty string makes `poll_target` omit `&caps=` entirely, so the AP
        // keeps the record it already has instead of replacing 5 capabilities
        // and an attested 824 GFLOPS with an empty declared set.
        assert_eq!(
            s, "",
            "a failed local read must not be published as a capability declaration"
        );
        assert!(
            !s.contains("caps_declared"),
            "a failed read must never claim a deliberate empty capability set"
        );
        std::env::remove_var("GPN_STATUS_URL");
    }

    #[test]
    fn a_process_declares_only_the_capabilities_it_can_serve() {
        // The heartbeat describes the whole HOST (this box runs all five kinds).
        let mut v = serde_json::json!({
            "capabilities": ["LLMInference", "ImageGeneration", "VideoGeneration",
                             "MLInference", "FineTuning"],
            "models": ["llama3.2:1b"]
        });
        filter_capabilities(&mut v, &["FineTuning".to_string()]);
        assert_eq!(
            v.get("capabilities").unwrap(),
            &serde_json::json!(["FineTuning"]),
            "a fine-tune provider must not advertise chat/image"
        );
        assert_eq!(v.get("caps_declared").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(
            v.get("models").unwrap(),
            &serde_json::json!(["llama3.2:1b"]),
            "the model list is a host fact and is left alone"
        );

        // A declaration the host does not offer yields an EMPTY, explicit list
        // (the AP then fails closed) rather than being widened.
        let mut v2 = serde_json::json!({ "capabilities": ["LLMInference"] });
        filter_capabilities(&mut v2, &["ImageGeneration".to_string()]);
        assert_eq!(v2.get("capabilities").unwrap(), &serde_json::json!([]));

        // No declaration = legacy behaviour, untouched.
        let mut v3 = serde_json::json!({ "capabilities": ["LLMInference", "FineTuning"] });
        filter_capabilities(&mut v3, &[]);
        assert_eq!(
            v3.get("capabilities").unwrap(),
            &serde_json::json!(["LLMInference", "FineTuning"])
        );
    }

    #[test]
    fn leases_parse_from_object_and_array() {
        let job = serde_json::to_value(LlmJob::new_llm("m", "hi", 0.2, 16)).unwrap();
        let body = serde_json::json!({
            "seq": 7,
            "leases": [
                {"job_id": 11, "model": "m", "pull": true,
                 "callback": "http://ap:8092", "callback_token": "tok", "job": job},
                {"job_id": "not-a-number"},
                {"model": "no id"}
            ]
        })
        .to_string();
        let leases = parse_leases(&body);
        assert_eq!(leases.len(), 1, "malformed leases are skipped, not fatal");
        assert_eq!(leases[0].job_id, 11);
        assert_eq!(leases[0].callback_token, "tok");
        assert!(leases[0].job.is_some(), "payload survives the round trip");
        assert_eq!(leases[0].job.as_ref().unwrap().prompt, "hi");

        // Bare array (defensive) and total garbage.
        let arr = r#"[{"job_id":5}]"#;
        assert_eq!(parse_leases(arr)[0].job_id, 5);
        assert!(parse_leases("<html>502</html>").is_empty());
    }

    #[test]
    fn apply_lease_writes_both_stores() {
        let dir = std::env::temp_dir().join(format!("gpn-assign-pull-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = dir.join("assignments.json");
        let payload_store = LlmStore::with_path(dir.join("llm_store.json"));
        let lease = Lease {
            job_id: 4242,
            model: "llama3.2:1b".into(),
            pull: false,
            callback: "http://ap:8092".into(),
            callback_token: "ctok".into(),
            job: Some(LlmJob::new_llm("llama3.2:1b", "hello", 0.2, 64)),
        };
        assert!(apply_lease_to(&lease, "5Provider", &store, &payload_store));
        let s = std::fs::read_to_string(&store).unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["4242"]["provider"], "5Provider");
        assert_eq!(v["4242"]["callback_token"], "ctok");
        assert_eq!(v["4242"]["model"], "llama3.2:1b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_redelivered_lease_never_resets_a_finished_job() {
        // The cursor makes re-delivery rare, but a lost cursor file must not
        // resurrect a job: our local record holds the response the provider
        // wrote, and the AP's copy is the original queued payload.
        let dir = std::env::temp_dir().join(format!("gpn-assign-redel-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store_path = dir.join("assignments.json");
        // Isolated payload store — never the live ~/.gpn/llm_store.json.
        let payload_store = LlmStore::with_path(dir.join("llm_store.json"));
        let lease = Lease {
            job_id: 9,
            model: "m".into(),
            pull: false,
            callback: String::new(),
            callback_token: String::new(),
            job: Some(LlmJob::new_llm("m", "hi", 0.1, 32)),
        };
        assert!(apply_lease_to(&lease, "5P", &store_path, &payload_store));
        // Simulate the provider finishing it.
        payload_store.complete(9, "answer", "hash", None).unwrap();
        // Re-delivery must keep the finished record, not reset it to queued.
        assert!(apply_lease_to(&lease, "5P", &store_path, &payload_store));
        let after = payload_store.get(9).expect("record survives");
        assert_eq!(after.status, "done");
        assert_eq!(after.response.as_deref(), Some("answer"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_store_write_is_reported_not_treated_as_a_delivery() {
        // #185, box 1: the provider's store path was write-denied
        // (C:\Windows\System32\.gpn), `put` failed, the error was discarded and
        // the caller then said "assigned but no local payload" - which reads
        // like a routing bug and hid the real cause for hours. The contract:
        // an unwritable payload store yields `false` (job is NOT runnable).
        let dir = std::env::temp_dir().join(format!("gpn-assign-denied-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // A regular FILE where a directory must be: every write under it fails.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let payload_store = LlmStore::with_path(blocker.join("sub/llm_store.json"));
        let assignments = dir.join("assignments.json");
        let lease = Lease {
            job_id: 6580,
            model: "deepseek-r1:14b".into(),
            pull: true,
            callback: String::new(),
            callback_token: String::new(),
            job: Some(LlmJob::new_llm("deepseek-r1:14b", "hi", 0.1, 32)),
        };
        assert!(
            !apply_lease_to(&lease, "5Hgbpi", &assignments, &payload_store),
            "a payload that could not be stored must not be reported as stored"
        );
        assert!(payload_store.get(6580).is_none());
        // And the failure line names the file the process believed in - the
        // fact that was missing when #185 was first diagnosed.
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access is denied.");
        let note = put_failure_note(6580, payload_store.path(), &err);
        assert!(note.contains("#6580"), "{note}");
        assert!(note.contains("llm_store.json"), "{note}");
        assert!(note.contains("Access is denied"), "{note}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seed_cursor_uses_the_highest_local_id() {
        let dir = std::env::temp_dir().join(format!("gpn-seed-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = dir.join("assignments.json");
        assert_eq!(seed_cursor_from_store(&store), 0, "absent store seeds at 0");
        std::fs::write(
            &store,
            serde_json::json!({"12": {"provider": "5P"}, "90": {"provider": "5P"}}).to_string(),
        )
        .unwrap();
        assert_eq!(seed_cursor_from_store(&store), 90);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
