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
//! # Mixed fleets
//!
//! `GPN_ASSIGN_SOURCE` selects the transport:
//!
//! * `push` — old behaviour only (AP must be able to reach `:8093`).
//! * `pull` — poll only.
//! * `both` — **default**: pull in parallel with the push listener. The push
//!   is then only a latency optimisation, and a NAT'd provider still works.
//!
//! `GPN_AP_URL` is required for pull (e.g. `http://gpn-testnet.duckdns.org:8092`);
//! without it the mode degrades to push and says so once.
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
//! Providers hold no user token, so the endpoint is bound by the same shared
//! `GPN_ASSIGN_TOKEN` secret the push path already uses (`X-Gpn-Assign-Token`).
//! Unset ⇒ legacy/permissive, exactly like `POST /assign`.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::llm::{merge_json_object, LlmJob, LlmStore};

/// Maximum leases the AP returns per response (also the AP-side cap).
pub const MAX_LEASES: usize = 20;

/// Default long-poll budget. The AP returns as soon as it has work, so this is
/// an idle heartbeat, not a delay: one request per 25 s when nothing happens.
pub const DEFAULT_WAIT_MS: u64 = 25_000;

/// Which transport(s) this provider uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignSource {
    /// `POST /assign` only (pre-#46 behaviour).
    Push,
    /// `GET /assignments` only.
    Pull,
    /// Both, in parallel (default).
    Both,
}

impl AssignSource {
    /// Parse `GPN_ASSIGN_SOURCE`; unknown values fall back to `Both` so a typo
    /// can never silently leave a NAT'd provider unreachable.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "push" => AssignSource::Push,
            "pull" => AssignSource::Pull,
            _ => AssignSource::Both,
        }
    }

    /// Does this mode run the pull loop?
    pub fn pulls(self) -> bool {
        matches!(self, AssignSource::Pull | AssignSource::Both)
    }

    /// Does this mode keep the `POST /assign` listener authoritative?
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
    /// Shared AP↔provider secret (`GPN_ASSIGN_TOKEN`); empty = permissive.
    pub token: String,
    /// Long-poll budget in ms.
    pub wait_ms: u64,
    /// Backoff ceiling between failed polls.
    pub max_backoff: Duration,
}

impl PullConfig {
    /// Resolve from the environment. `None` when pull is not requested or no
    /// `GPN_AP_URL` is configured (the caller then keeps push-only behaviour).
    pub fn from_env(provider: &str) -> Option<Self> {
        let source = AssignSource::parse(
            &std::env::var("GPN_ASSIGN_SOURCE").unwrap_or_else(|_| "both".into()),
        );
        if !source.pulls() {
            return None;
        }
        let ap_url = std::env::var("GPN_AP_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())?;
        Some(Self {
            ap_url,
            provider: provider.to_string(),
            token: std::env::var("GPN_ASSIGN_TOKEN").unwrap_or_default(),
            wait_ms: std::env::var("GPN_ASSIGN_WAIT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_WAIT_MS),
            max_backoff: Duration::from_secs(30),
        })
    }

    /// Build the poll URL for a cursor position.
    pub fn poll_url(&self, since: u64) -> String {
        format!(
            "{}/assignments?provider={}&since={}&wait={}",
            self.ap_url,
            urlencode(&self.provider),
            since,
            self.wait_ms
        )
    }
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

/// Persist one lease exactly like `POST /assign` does, so every downstream
/// consumer (claim loops, `run_job`, result callback) is unchanged.
///
/// Returns whether the off-chain payload was stored.
pub fn apply_lease(lease: &Lease, provider: &str, assignment_store: &PathBuf) -> bool {
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
            stored_job = store.put(lease.job_id, job.clone()).is_ok();
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
/// Path of the shared assignments file (`$HOME/.gpn/assignments.json`).
pub fn assignment_store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(format!("{home}/.gpn/assignments.json"))
}

fn cursor_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(format!("{home}/.gpn/assign-cursor.json"))
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
    println!(
        "[assign] pull transport starting — ap={} provider={} since={} wait={}ms",
        cfg.ap_url,
        short(&provider_label),
        since,
        cfg.wait_ms
    );
    loop {
        let url = cfg.poll_url(since);
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
        if !cfg.token.is_empty() {
            req = req.header("X-Gpn-Assign-Token", &cfg.token);
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
            }
            Ok(resp) => {
                // 401 = our token does not match the AP's. That will never fix
                // itself, so say it loudly once and then back off hard.
                errors += 1;
                if resp.status().as_u16() == 401 {
                    eprintln!(
                        "[assign] AP rejected the pull ({}) — check GPN_ASSIGN_TOKEN matches the AP",
                        resp.status()
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
    fn source_parsing_defaults_safely() {
        assert_eq!(AssignSource::parse("push"), AssignSource::Push);
        assert_eq!(AssignSource::parse(" PULL "), AssignSource::Pull);
        assert_eq!(AssignSource::parse("both"), AssignSource::Both);
        // A typo must not leave a NAT'd provider unreachable.
        assert_eq!(AssignSource::parse("pul"), AssignSource::Both);
        assert_eq!(AssignSource::parse(""), AssignSource::Both);
        assert!(AssignSource::Both.pulls() && AssignSource::Both.pushes());
        assert!(!AssignSource::Push.pulls());
        assert!(!AssignSource::Pull.pushes());
    }

    #[test]
    fn poll_url_encodes_provider_and_carries_cursor() {
        let cfg = PullConfig {
            ap_url: "http://ap:8092".into(),
            provider: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into(),
            token: String::new(),
            wait_ms: 25000,
            max_backoff: Duration::from_secs(30),
        };
        assert_eq!(
            cfg.poll_url(42),
            "http://ap:8092/assignments?provider=5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY&since=42&wait=25000"
        );
        assert_eq!(urlencode("0xab/cd+ef"), "0xab%2Fcd%2Bef");
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
    fn seed_cursor_uses_the_highest_local_id() {
        let dir = std::env::temp_dir().join(format!("gpn-seed-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = dir.join("assignments.json");
        assert_eq!(seed_cursor_from_store(&store), 0, "absent store seeds at 0");
        std::fs::write(
            &store,
            serde_json::json!({"12": {"provider": "5P"}, "90": {"provider": "5P"}})
                .to_string(),
        )
        .unwrap();
        assert_eq!(seed_cursor_from_store(&store), 90);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
