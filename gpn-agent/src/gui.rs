//! The local node dashboard (`http://127.0.0.1:8099`).
//!
//! A tiny `tiny_http` server — the same HTTP stack the scheduler already uses —
//! that serves one self-contained HTML page plus three JSON endpoints:
//!
//! | method | path           | meaning                                    |
//! |--------|----------------|--------------------------------------------|
//! | GET    | `/`            | the dashboard (embedded, no CDN, no build) |
//! | GET    | `/api/status`  | node + idle status as JSON                 |
//! | GET    | `/api/referral`| this node's own referral code + share link |
//! | POST   | `/api/idle`    | `{"minutes": n, "reason": "..."}`          |
//! | POST   | `/api/resume`  | leave idle now                             |
//!
//! **Loopback only.** The bind address is validated before the socket is
//! created: a dashboard that can be reached from the LAN would be an
//! unauthenticated remote control for the miner. `127.0.0.1` (or `localhost`)
//! only — no `0.0.0.0`, no LAN IPs, ever.
//!
//! The routing logic is a **pure function** ([`handle`]) so the endpoint shapes
//! can be tested without a socket; [`spawn`] only does I/O.

use crate::idle;
use crate::idle_auto::{AutoIdleShared, AUTO_REASON_PREFIX};
use serde_json::{json, Value};
use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tiny_http::{Header, Response, Server};

/// The embedded dashboard page (one file, no external requests).
pub const INDEX_HTML: &str = include_str!("gui/index.html");

/// Where the GUI listens by default.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8099";

/// Anything that can describe the node to the dashboard.
///
/// The supervisor owns the real implementation (child states, RPC height,
/// peers, GPU); tests use a closure. Keeping it a trait is what makes the
/// endpoints testable without a chain.
pub trait StatusSource: Send + Sync {
    fn status(&self) -> Value;
}

impl<F> StatusSource for F
where
    F: Fn() -> Value + Send + Sync,
{
    fn status(&self) -> Value {
        self()
    }
}

/// The `GET /api/referral` body: the operator's code and share link.
///
/// No wallet is a **state**, not an error: `{wallet:false, error:"…"}` with the
/// sentence the card shows, because the dashboard must be honest about a
/// machine that has not been provisioned yet rather than print a placeholder
/// code. Derivation lives in [`crate::referral`] — one implementation only.
fn referral_json(status: &Value) -> Value {
    let address = status
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if address.is_empty() {
        return json!({
            "wallet": false,
            "error": "this node has no payout account yet, so it has no referral code — run `gpn_node --ensure-wallet` (or `gpn_node --setup`) to create the wallet"
        });
    }
    match crate::referral::derive_code(&address) {
        Ok(compact) => json!({
            "wallet": true,
            "address": address,
            "code": crate::referral::display_form(&compact),
            "compact": compact,
            "link": crate::referral::share_link(&compact),
        }),
        Err(e) => json!({
            "wallet": true,
            "address": address,
            "error": e.to_string(),
        }),
    }
}

/// GUI runtime options.
#[derive(Debug, Clone)]
pub struct GuiOptions {
    /// `host:port`. Must be loopback (validated by [`check_loopback`]).
    pub listen: String,
    /// `--force-idle` / `GPN_IDLE_FORCE=1`: bypass the sole-authority valve.
    pub force_idle: bool,
    /// Idle state file. `None` → [`idle::idle_path`] (`$GPN_IDLE_FILE`/`~/.gpn`).
    pub idle_path: Option<PathBuf>,
    /// Auto-idle toggle + last reading, when the sampler thread is running.
    /// `None` in tests and in supervisors without the sampler.
    pub auto_idle: Option<Arc<AutoIdleShared>>,
    /// Second-coin state file (row 294). `None` → [`secondary::state_path`].
    pub secondary_path: Option<PathBuf>,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.to_string(),
            force_idle: force_idle_env(),
            idle_path: None,
            auto_idle: None,
            secondary_path: None,
        }
    }
}

/// `GPN_IDLE_FORCE=1` — documented override of the sole-authority valve.
pub fn force_idle_env() -> bool {
    matches!(
        std::env::var("GPN_IDLE_FORCE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// One HTTP reply, decoupled from `tiny_http` so it can be asserted in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Reply {
    fn json(code: u16, v: Value) -> Self {
        Self {
            code,
            content_type: "application/json",
            body: v.to_string(),
        }
    }
    fn html(body: &str) -> Self {
        Self {
            code: 200,
            content_type: "text/html; charset=utf-8",
            body: body.to_string(),
        }
    }
    fn error(code: u16, msg: impl Into<String>) -> Self {
        Self::json(code, json!({"ok": false, "error": msg.into()}))
    }
}

/// Reject anything that is not a loopback bind address.
///
/// Returns the address to hand to `tiny_http` (`127.0.0.1:port`, with
/// `localhost` normalised). Deliberately explicit rather than "0.0.0.0 but
/// firewalled": the operator's firewall is not our security model.
pub fn check_loopback(listen: &str) -> Result<String, String> {
    let listen = listen.trim();
    // Normalise `localhost` → 127.0.0.1 (it can resolve to ::1, and we want a
    // predictable v4 loopback for the printed hint).
    let normalised = if let Some(port) = listen.strip_prefix("localhost:") {
        format!("127.0.0.1:{port}")
    } else {
        listen.to_string()
    };
    let addr: SocketAddr = normalised
        .parse()
        .map_err(|_| format!("invalid --gui-listen address {listen:?} (expected HOST:PORT)"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "refusing to bind the dashboard to {}: it only ever listens on 127.0.0.1 \
             (a LAN-reachable dashboard would be an unauthenticated remote control for the miner)",
            addr.ip()
        ));
    }
    Ok(normalised)
}

/// The idle portion of the status document.
fn idle_json(path: &std::path::Path, now: u64) -> Value {
    match idle::state_at(path, now) {
        Some(st) => json!({
            "active": true,
            "until": st.until,
            "started": st.started,
            "reason": st.reason,
            "auto": st.reason.starts_with(AUTO_REASON_PREFIX),
            "remaining_secs": st.until.saturating_sub(now),
        }),
        None => json!({
            "active": false,
            "until": Value::Null,
            "started": Value::Null,
            "reason": "",
            "auto": false,
            "remaining_secs": 0,
        }),
    }
}

/// The second-coin block for the dashboard (row 294): the operator's saved
/// choices, the coins this card may be offered, and what each miner is doing
/// right now. Read from files, never from GUI state — the supervisor is the
/// only writer of the status file, so the dashboard cannot show a second
/// miner that is not actually running.
fn secondary_json(vram_gb: f64, now: u64, state_path: Option<&std::path::Path>) -> Value {
    use crate::secondary as sec;
    let state = state_path
        .map(sec::read_state_at)
        .unwrap_or_else(sec::read_state)
        .unwrap_or_default();
    let status = sec::read_status();
    let cards: Vec<Value> = status
        .cards
        .iter()
        .map(|c| {
            json!({
                "device": c.device,
                "coin": c.coin,
                "state": c.state,
                "pid": c.pid,
                "intensity_percent": c.intensity_percent,
                "since": c.since,
                "for_secs": now.saturating_sub(c.since),
                "detail": c.detail,
            })
        })
        .collect();
    let coins: Vec<Value> = sec::available_coins(vram_gb)
        .into_iter()
        .map(|c| {
            json!({
                "coin": c.as_str(),
                "dag_gb": c.dag_gb(),
                "pools": c.pools().iter().map(|(n, u)| json!({"name": n, "url": u})).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "enabled": state.enabled,
        "cards": state.cards,
        "vram_gb": vram_gb,
        "available_coins": coins,
        "min_intensity": sec::MIN_INTENSITY,
        "max_intensity": sec::MAX_INTENSITY,
        "default_intensity": sec::DEFAULT_INTENSITY,
        "status": {"updated": status.updated, "cards": cards},
    })
}

/// Pure request router: `(status_code, content_type, body)`.
///
/// `now` is injected so the auto-resume boundary is testable.
pub fn handle(
    method: &str,
    path: &str,
    body: &str,
    source: &dyn StatusSource,
    opts: &GuiOptions,
    now: u64,
) -> Reply {
    let path = path.split('?').next().unwrap_or(path);
    let idle_path = opts.idle_path.clone().unwrap_or_else(idle::idle_path);

    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => Reply::html(INDEX_HTML),

        ("GET", "/api/status") => {
            let mut status = source.status();
            if let Some(obj) = status.as_object_mut() {
                obj.insert("idle".into(), idle_json(&idle_path, now));
                obj.insert("now".into(), json!(now));
                obj.insert("max_idle_minutes".into(), json!(idle::MAX_IDLE_MINUTES));
                obj.insert("durations".into(), json!([30, 60, 120, 240, 360]));
                obj.insert("force_idle".into(), json!(opts.force_idle));
                obj.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
                let vram_gb = obj
                    .get("gpu_vram_gb")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                obj.insert(
                    "secondary".into(),
                    secondary_json(vram_gb, now, opts.secondary_path.as_deref()),
                );
                obj.insert(
                    "dashboard".into(),
                    json!({"listen": opts.listen, "loopback": true}),
                );
                // Auto-idle: on/off + why it is engaged. Present whenever the
                // sampler thread runs (which is always, in gpn_node).
                if let Some(auto) = &opts.auto_idle {
                    obj.insert("auto_idle".into(), auto.status_json(now));
                }
            }
            Reply::json(200, status)
        }

        // GET /api/referral (row 224) — this node's own code, for the card.
        // Derived from the account the status already reports, so the dashboard
        // can never advertise a code the node itself would not publish.
        ("GET", "/api/referral") => Reply::json(200, referral_json(&source.status())),

        // POST /api/idle/auto {"enabled": true|false} — the dashboard toggle.
        ("POST", "/api/idle/auto") => {
            let Some(auto) = &opts.auto_idle else {
                return Reply::error(
                    409,
                    "auto-idle is not running in this process (start gpn_node with --gui)",
                );
            };
            let parsed: Value =
                match serde_json::from_str(if body.trim().is_empty() { "{}" } else { body }) {
                    Ok(v) => v,
                    Err(e) => return Reply::error(400, format!("invalid JSON body: {e}")),
                };
            let Some(on) = parsed.get("enabled").and_then(Value::as_bool) else {
                return Reply::error(400, "missing boolean \"enabled\"");
            };
            auto.set_enabled(on);
            println!(
                "[gui] auto-idle {} — {} (threshold {}% util, window {} min)",
                if on { "ON" } else { "OFF" },
                if on {
                    "will step aside when the GPU is busy with non-GPN work"
                } else {
                    "never idles on its own; manual idle still works"
                },
                auto.cfg.util_percent,
                auto.cfg.minutes
            );
            Reply::json(200, json!({"ok": true, "auto_idle": auto.status_json(now)}))
        }

        ("POST", "/api/idle") => {
            let parsed: Value =
                match serde_json::from_str(if body.trim().is_empty() { "{}" } else { body }) {
                    Ok(v) => v,
                    Err(e) => return Reply::error(400, format!("invalid JSON body: {e}")),
                };
            let Some(minutes) = parsed.get("minutes").and_then(Value::as_u64) else {
                return Reply::error(
                    400,
                    "missing or non-integer \"minutes\" (1 … 360); the dashboard sends 30/60/120/240/360",
                );
            };
            if let Err(e) = idle::validate_minutes(minutes) {
                return Reply::error(400, e.to_string());
            }
            let reason = parsed
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("dashboard")
                .to_string();

            // Safety valve: idling the only block producer halts the chain.
            let peers = source.status().get("peers").and_then(Value::as_u64);
            if let Some(why) = idle::refusal_reason(peers, opts.force_idle) {
                eprintln!("[gui] REFUSED idle request: {why}");
                return Reply::json(
                    409,
                    json!({
                        "ok": false,
                        "error": why,
                        "refused": "sole-authority",
                        "peers": peers,
                        "force_hint": "--force-idle / GPN_IDLE_FORCE=1",
                    }),
                );
            }

            match idle::start_at(&idle_path, minutes, &reason, now) {
                Ok(st) => {
                    println!(
                        "[gui] idle for {minutes} min (until {}), reason={reason:?} — \
                         mining and job claiming paused, node keeps syncing",
                        st.until
                    );
                    Reply::json(
                        200,
                        json!({
                            "ok": true,
                            "minutes": minutes,
                            "until": st.until,
                            "started": st.started,
                            "reason": st.reason,
                            "idle": idle_json(&idle_path, now),
                        }),
                    )
                }
                Err(e) => Reply::error(500, e.to_string()),
            }
        }

        ("POST", "/api/resume") => {
            let was = idle::clear_at(&idle_path);
            println!(
                "[gui] resume requested — {}",
                if was {
                    "idle ended, resuming mining + job claiming"
                } else {
                    "node was already active"
                }
            );
            Reply::json(
                200,
                json!({
                    "ok": true,
                    "resumed": was,
                    "idle": idle_json(&idle_path, now),
                }),
            )
        }

        // ── second coin (row 294) ──────────────────────────────────────────
        // GET returns the saved state + the live status; POST replaces the
        // operator's choices (validated here, so the API can never accept what
        // the supervisor would refuse to run).
        ("GET", "/api/secondary") => {
            let vram_gb = source
                .status()
                .get("gpu_vram_gb")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            Reply::json(
                200,
                json!({"ok": true, "secondary": secondary_json(vram_gb, now, opts.secondary_path.as_deref())}),
            )
        }
        ("POST", "/api/secondary") => {
            use crate::secondary as sec;
            let sec_path = opts.secondary_path.clone().unwrap_or_else(sec::state_path);
            let parsed: Value =
                match serde_json::from_str(if body.trim().is_empty() { "{}" } else { body }) {
                    Ok(v) => v,
                    Err(e) => return Reply::error(400, format!("invalid JSON body: {e}")),
                };
            let enabled = parsed
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let cards: Vec<sec::CardConfig> = match parsed.get("cards") {
                Some(Value::Array(a)) => {
                    match serde_json::from_value::<Vec<sec::CardConfig>>(Value::Array(a.clone())) {
                        Ok(v) => v,
                        Err(e) => return Reply::error(400, format!("cards: {e}")),
                    }
                }
                None => Vec::new(),
                _ => return Reply::error(400, "\"cards\" must be a list"),
            };
            let state = sec::SecondaryState { enabled, cards };
            if let Err(e) = state.validate() {
                return Reply::json(400, json!({"ok": false, "error": e}));
            }
            if let Err(e) = sec::write_state_at(&sec_path, &state) {
                return Reply::error(500, e);
            }
            println!(
                "[gui] second coin: {} — {} card(s) (supervisor applies within 5 s)",
                if enabled { "enabled" } else { "disabled" },
                state.cards.len()
            );
            let vram_gb = source
                .status()
                .get("gpu_vram_gb")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            Reply::json(
                200,
                json!({"ok": true, "secondary": secondary_json(vram_gb, now, opts.secondary_path.as_deref())}),
            )
        }

        ("GET", _) | ("POST", _) => Reply::error(404, format!("no route for {method} {path}")),
        _ => Reply::error(
            405,
            format!("method {method} not allowed (use GET or POST)"),
        ),
    }
}

/// Bind, serve, and return the bound address plus a stop switch.
///
/// The returned handle owns the accept loop; set the `AtomicBool` to stop it
/// (tests do this; the supervisor just leaks it for the process lifetime).
pub fn spawn(opts: GuiOptions, source: Arc<dyn StatusSource>) -> Result<GuiHandle, String> {
    let listen = check_loopback(&opts.listen)?;
    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    let addr = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| format!("{listen} did not resolve to an IP socket"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let opts_thread = opts.clone();
    let handle = std::thread::Builder::new()
        .name("gpn-gui".into())
        .spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                // Poll with a timeout so `stop` is honoured promptly.
                match server.recv_timeout(Duration::from_millis(250)) {
                    Ok(Some(req)) => serve_one(req, &*source, &opts_thread),
                    Ok(None) => continue,
                    Err(_) => break, // server closed
                }
            }
        })
        .map_err(|e| format!("cannot start the dashboard thread: {e}"))?;
    Ok(GuiHandle {
        addr,
        stop,
        thread: Some(handle),
    })
}

fn serve_one(mut req: tiny_http::Request, source: &dyn StatusSource, opts: &GuiOptions) {
    let method = req.method().as_str().to_string();
    let path = req.url().to_string();
    let mut body = String::new();
    // Bounded read: the dashboard only ever sends a few bytes, and an unauthenticated
    // local listener should not accept an unbounded upload.
    let _ = req.as_reader().take(8 * 1024).read_to_string(&mut body);
    let reply = handle(&method, &path, &body, source, opts, idle::now_unix());
    let mut resp = Response::from_string(reply.body).with_status_code(reply.code);
    if let Ok(h) = Header::from_bytes(&b"Content-Type"[..], reply.content_type.as_bytes()) {
        resp = resp.with_header(h);
    }
    if let Ok(h) = Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]) {
        resp = resp.with_header(h);
    }
    let _ = req.respond(resp);
}

/// A running dashboard.
pub struct GuiHandle {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GuiHandle {
    /// URL to print at startup.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    /// Stop the accept loop and join the thread.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for GuiHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Do not join on drop: the supervisor drops this at shutdown while the
        // thread is already being torn down (joining could block on a request).
    }
}

/// The one-line hint printed at node startup when the GUI is NOT enabled.
pub fn startup_hint(_bin: &str) -> String {
    // Only reached when the dashboard is OFF (opt-out). The default is now on.
    format!("  dashboard: off (GPN_GUI=0 / --no-gui) — re-enable with --gui on {DEFAULT_LISTEN}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn opts(tag: &str) -> GuiOptions {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gpn-gui-test-{tag}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        GuiOptions {
            listen: DEFAULT_LISTEN.into(),
            force_idle: false,
            idle_path: Some(dir.join("idle.json")),
            auto_idle: None,
            secondary_path: Some(dir.join("secondary.json")),
        }
    }

    fn source(peers: Option<u64>) -> impl StatusSource {
        move || json!({"node": "up", "peers": peers, "best": 42})
    }

    #[test]
    fn index_is_served_from_the_binary() {
        let r = handle("GET", "/", "", &source(Some(1)), &opts("index"), 1_000);
        assert_eq!(r.code, 200);
        assert!(r.content_type.starts_with("text/html"));
        assert!(r.body.contains("<html"), "the page must be embedded");
        // No CDN: the page must not reference any external URL.
        assert!(!r.body.contains("http://") && !r.body.contains("https://"));
    }

    #[test]
    fn the_dashboard_renders_the_ai_serving_card() {
        // The card is driven by `serving.reason` — the operator must be able to
        // tell "receiving work" from "configured but nothing confirmed"
        // without reading the journal.
        let r = handle("GET", "/", "", &source(Some(1)), &opts("serving"), 1_000);
        assert!(r.body.contains("AI serving"), "missing the AI serving card");
        assert!(r.body.contains("s.serving"), "the card must read serving.*");
        assert!(
            r.body.contains("servingSub"),
            "the card must show the reason"
        );
    }

    #[test]
    fn the_dashboard_shows_the_operators_own_referral_code() {
        // Row 224, owner point 2026-11-20: the operator has to be able to FIND
        // and COPY their own code. Both halves are asserted here: the card is
        // in the page, and the value it shows comes from the payout account.
        let r = handle("GET", "/", "", &source(Some(1)), &opts("ref"), 1_000);
        assert!(r.body.contains("Your referral code"), "missing the card");
        assert!(
            r.body.contains("id=\"refCode\""),
            "missing the code element"
        );
        assert!(
            r.body.contains("id=\"refCopy\"") && r.body.contains("navigator.clipboard"),
            "the card must have a working Copy button"
        );
        assert!(
            r.body.contains("/api/referral"),
            "the card must read the code from the API, not compute one"
        );
    }

    #[test]
    fn referral_endpoint_derives_the_code_from_the_payout_account() {
        let src =
            || json!({"node": "up", "account": "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"});
        let r = handle("GET", "/api/referral", "", &src, &opts("refjson"), 1_000);
        assert_eq!(r.code, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["wallet"], true, "{v}");
        assert_eq!(v["code"], "GPN-AX3R-YC7C-KV", "{v}");
        assert_eq!(v["compact"], "GPNAX3RYC7CKV", "{v}");
        assert_eq!(
            v["link"], "https://gpncoin.ai/join?ref=GPN-AX3R-YC7C-KV",
            "the link must be /join, never a download URL: {v}"
        );
    }

    #[test]
    fn referral_endpoint_says_so_when_there_is_no_wallet() {
        // No account: a state, not an error, and never a placeholder code.
        let r = handle(
            "GET",
            "/api/referral",
            "",
            &source(Some(1)),
            &opts("noref"),
            1_000,
        );
        assert_eq!(r.code, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["wallet"], false, "{v}");
        assert!(v.get("code").is_none(), "no code may be invented: {v}");
        assert!(
            v["error"]
                .as_str()
                .unwrap_or("")
                .contains("--ensure-wallet"),
            "the message must name the fix: {v}"
        );
    }

    #[test]
    fn status_includes_idle_block_and_the_dashboard_metadata() {
        let o = opts("status");
        let r = handle("GET", "/api/status", "", &source(Some(2)), &o, 1_000);
        assert_eq!(r.code, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["peers"], 2);
        assert_eq!(v["best"], 42);
        assert_eq!(v["idle"]["active"], false);
        assert_eq!(v["max_idle_minutes"], 360);
        assert_eq!(v["durations"], json!([30, 60, 120, 240, 360]));
        assert_eq!(v["dashboard"]["loopback"], true);
    }

    #[test]
    fn idle_then_resume_is_a_200_round_trip() {
        let o = opts("roundtrip");
        let r = handle(
            "POST",
            "/api/idle",
            r#"{"minutes":60}"#,
            &source(Some(3)),
            &o,
            1_000,
        );
        assert_eq!(r.code, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["until"], 1_000 + 3_600);
        assert_eq!(v["idle"]["active"], true);

        // Status now reports the countdown.
        let s: Value = serde_json::from_str(
            &handle("GET", "/api/status", "", &source(Some(3)), &o, 1_500).body,
        )
        .unwrap();
        assert_eq!(s["idle"]["active"], true);
        assert_eq!(s["idle"]["remaining_secs"], 3_100);

        // Resume clears it.
        let r = handle("POST", "/api/resume", "", &source(Some(3)), &o, 1_600);
        assert_eq!(r.code, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["resumed"], true);
        assert_eq!(v["idle"]["active"], false);

        // Resume again is idempotent (200, resumed=false).
        let v: Value = serde_json::from_str(
            &handle("POST", "/api/resume", "", &source(Some(3)), &o, 1_700).body,
        )
        .unwrap();
        assert_eq!(v["resumed"], false);
    }

    #[test]
    fn auto_resume_at_the_deadline_is_visible_in_status() {
        let o = opts("deadline");
        handle(
            "POST",
            "/api/idle",
            r#"{"minutes":1}"#,
            &source(Some(1)),
            &o,
            10_000,
        );
        // Just before: idle.
        let s: Value = serde_json::from_str(
            &handle("GET", "/api/status", "", &source(Some(1)), &o, 10_059).body,
        )
        .unwrap();
        assert_eq!(s["idle"]["active"], true);
        assert_eq!(s["idle"]["remaining_secs"], 1);
        // At the boundary: active, and the file is gone.
        let s: Value = serde_json::from_str(
            &handle("GET", "/api/status", "", &source(Some(1)), &o, 10_060).body,
        )
        .unwrap();
        assert_eq!(s["idle"]["active"], false);
        assert!(!o.idle_path.as_ref().unwrap().exists());
    }

    #[test]
    fn idle_rejects_zero_long_and_malformed_requests_with_400() {
        let o = opts("bad");
        for body in [
            r#"{"minutes":0}"#,
            r#"{"minutes":361}"#,
            r#"{"minutes":1440}"#,
            r#"{"minutes":"60"}"#,
            r#"{}"#,
            r#"not json"#,
        ] {
            let r = handle("POST", "/api/idle", body, &source(Some(2)), &o, 1_000);
            assert_eq!(
                r.code, 400,
                "body {body} must be a 400, got {} {}",
                r.code, r.body
            );
            assert!(
                !o.idle_path.as_ref().unwrap().exists(),
                "a 400 must not write state"
            );
        }
    }

    #[test]
    fn over_long_idle_names_the_maximum() {
        let o = opts("toolong");
        let r = handle(
            "POST",
            "/api/idle",
            r#"{"minutes":600}"#,
            &source(Some(2)),
            &o,
            1_000,
        );
        assert_eq!(r.code, 400);
        assert!(r.body.contains("360"), "{}", r.body);
    }

    #[test]
    fn a_lone_node_is_refused_with_409_and_can_be_forced() {
        let o = opts("sole");
        let r = handle(
            "POST",
            "/api/idle",
            r#"{"minutes":60}"#,
            &source(Some(0)),
            &o,
            1_000,
        );
        assert_eq!(r.code, 409, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["refused"], "sole-authority");
        assert!(!o.idle_path.as_ref().unwrap().exists());

        // Unreadable peer count (node starting / RPC down) is refused too.
        let r = handle(
            "POST",
            "/api/idle",
            r#"{"minutes":60}"#,
            &source(None),
            &o,
            1_000,
        );
        assert_eq!(r.code, 409);

        // Force override.
        let forced = GuiOptions {
            force_idle: true,
            ..o.clone()
        };
        let r = handle(
            "POST",
            "/api/idle",
            r#"{"minutes":60}"#,
            &source(Some(0)),
            &forced,
            1_000,
        );
        assert_eq!(r.code, 200, "{}", r.body);
    }

    #[test]
    fn auto_idle_block_and_toggle_endpoint() {
        use crate::idle_auto::{AutoIdleConfig, AutoIdleShared};
        let o = opts("auto");
        let dir = o
            .idle_path
            .as_ref()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let shared = Arc::new(AutoIdleShared::new(
            AutoIdleConfig::default(),
            false,
            dir.join("idle-auto.json"),
        ));
        let o = GuiOptions {
            auto_idle: Some(shared.clone()),
            ..o
        };

        // Status advertises the toggle (off by default).
        let s: Value = serde_json::from_str(
            &handle("GET", "/api/status", "", &source(Some(2)), &o, 1_000).body,
        )
        .unwrap();
        assert_eq!(s["auto_idle"]["enabled"], false);
        assert_eq!(s["auto_idle"]["threshold_pct"], 40);
        assert_eq!(s["auto_idle"]["minutes"], 30);

        // Toggle on → persisted + visible.
        let r = handle(
            "POST",
            "/api/idle/auto",
            r#"{"enabled":true}"#,
            &source(Some(2)),
            &o,
            1_000,
        );
        assert_eq!(r.code, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["auto_idle"]["enabled"], true);
        assert!(shared.enabled());

        // Bad body → 400, unchanged.
        let r = handle(
            "POST",
            "/api/idle/auto",
            r#"{}"#,
            &source(Some(2)),
            &o,
            1_000,
        );
        assert_eq!(r.code, 400);
        assert!(shared.enabled());

        // Toggle off.
        let r = handle(
            "POST",
            "/api/idle/auto",
            r#"{"enabled":false}"#,
            &source(Some(2)),
            &o,
            1_000,
        );
        assert_eq!(r.code, 200);
        assert!(!shared.enabled());

        // A dashboard-written auto window reports its reason.
        let _ = handle(
            "POST",
            "/api/idle",
            r#"{"minutes":5,"reason":"auto: foreign GPU load 87%"}"#,
            &source(Some(2)),
            &o,
            1_000,
        );
        let s: Value = serde_json::from_str(
            &handle("GET", "/api/status", "", &source(Some(2)), &o, 1_100).body,
        )
        .unwrap();
        assert_eq!(s["idle"]["active"], true);
        assert_eq!(s["idle"]["auto"], true);
        assert!(s["idle"]["reason"]
            .as_str()
            .unwrap()
            .contains("foreign GPU load"));

        // Without a sampler (tests, fleet-less runs) the toggle is a 409.
        let plain = opts("auto-none");
        let r = handle(
            "POST",
            "/api/idle/auto",
            r#"{"enabled":true}"#,
            &source(Some(2)),
            &plain,
            1_000,
        );
        assert_eq!(r.code, 409);
    }

    #[test]
    fn unknown_routes_are_404_and_other_methods_405() {
        let o = opts("routes");
        assert_eq!(
            handle("GET", "/nope", "", &source(Some(1)), &o, 1).code,
            404
        );
        assert_eq!(
            handle("POST", "/api/status", "", &source(Some(1)), &o, 1).code,
            404
        );
        assert_eq!(
            handle("DELETE", "/api/idle", "", &source(Some(1)), &o, 1).code,
            405
        );
    }

    #[test]
    fn query_strings_do_not_break_routes() {
        let o = opts("query");
        assert_eq!(
            handle("GET", "/api/status?x=1", "", &source(Some(1)), &o, 1).code,
            200
        );
    }

    #[test]
    fn second_coin_can_be_enabled_from_the_dashboard_and_is_validated() {
        let dir = std::env::temp_dir().join(format!("gpn-gui-sec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let o = GuiOptions {
            secondary_path: Some(dir.join("secondary.json")),
            ..Default::default()
        };
        let src = || json!({"peers": 3, "gpu_vram_gb": 8.0});

        // Off by default, and an 8 GB card is offered ergo + alephium only.
        let r = handle("GET", "/api/secondary", "", &src, &o, 100);
        assert_eq!(r.code, 200);
        let body = r.body;
        assert!(body.contains("\"ergo\""), "{body}");
        assert!(body.contains("\"alephium\""), "{body}");
        assert!(
            !body.contains("\"ravencoin\""),
            "an 8 GB card must not be offered a 4 GB second DAG: {body}"
        );
        assert!(body.contains("\"enabled\":false"), "{body}");

        // An intensity outside the safe band is refused with a reason.
        let bad = json!({"enabled": true, "cards": [{
            "device": 0, "coin": "ergo",
            "address": "9f4QF8Rn5wLhVvjP3g2Kp8sVnTq7YbWc1xZmAoE5rtyAbCdEfGh",
            "pool": "ergo.2miners.com:6060", "intensity_percent": 90
        }]})
        .to_string();
        let r = handle("POST", "/api/secondary", &bad, &src, &o, 100);
        assert_eq!(r.code, 400, "{}", r.body);
        assert!(r.body.contains("intensity"), "{}", r.body);

        // A valid choice is saved (the supervisor picks it up from the file).
        let good = json!({"enabled": true, "cards": [{
            "device": 0, "coin": "ergo",
            "address": "9f4QF8Rn5wLhVvjP3g2Kp8sVnTq7YbWc1xZmAoE5rtyAbCdEfGh",
            "pool": "ergo.2miners.com:6060", "intensity_percent": 30
        }]})
        .to_string();
        let r = handle("POST", "/api/secondary", &good, &src, &o, 100);
        assert_eq!(r.code, 200, "{}", r.body);
        assert!(r.body.contains("\"enabled\":true"), "{}", r.body);
        let saved = std::fs::read_to_string(dir.join("secondary.json")).unwrap();
        assert!(saved.contains("ergo.2miners.com:6060"), "{saved}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_loopback_binds_are_refused() {
        assert!(check_loopback("127.0.0.1:8099").is_ok());
        assert!(check_loopback("127.0.0.1:0").is_ok());
        assert_eq!(check_loopback("localhost:8099").unwrap(), "127.0.0.1:8099");
        assert!(check_loopback("[::1]:8099").is_ok());
        for bad in [
            "0.0.0.0:8099",
            "192.168.1.10:8099",
            "10.0.0.5:8099",
            "example.com:8099",
        ] {
            assert!(check_loopback(bad).is_err(), "{bad} must be refused");
        }
        assert!(check_loopback("nonsense").is_err());
    }

    /// End-to-end over a real socket: bind :0, fetch the page and the API, and
    /// drive one idle/resume cycle through actual HTTP.
    #[test]
    fn serves_over_http_and_stops_cleanly() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpStream;

        fn get(addr: SocketAddr, req: &str) -> (u16, String) {
            let mut s = TcpStream::connect(addr).expect("connect");
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            let mut buf = String::new();
            s.read_to_string(&mut buf).unwrap();
            let code = buf
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            (code, body)
        }

        let o = GuiOptions {
            listen: "127.0.0.1:0".into(),
            ..opts("socket")
        };
        let handle = spawn(o, Arc::new(source(Some(5)))).expect("bind ephemeral port");
        let addr = handle.addr;
        assert!(handle.url().starts_with("http://127.0.0.1:"));

        let (code, body) = get(
            addr,
            "GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(code, 200);
        assert!(body.contains("\"peers\":5"), "{body}");

        let payload = r#"{"minutes":30}"#;
        let (code, body) = get(
            addr,
            &format!(
                "POST /api/idle HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            ),
        );
        assert_eq!(code, 200, "{body}");
        assert!(body.contains("\"active\":true"), "{body}");

        let (code, body) = get(
            addr,
            "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(code, 200);
        assert!(body.contains("<title>"), "index page body: {body}");

        // Row 224 over a real socket: the code the browser will copy.
        let (code, body) = get(
            addr,
            "GET /api/referral HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(code, 200, "{body}");
        assert!(
            body.contains("\"wallet\":false") && body.contains("--ensure-wallet"),
            "the socket must answer the wallet state honestly: {body}"
        );

        handle.stop();
        // tiny_http's listener is closed when the accept thread exits; the
        // socket can take a moment to disappear, so poll briefly instead of
        // racing the OS.
        let mut closed = false;
        for _ in 0..50 {
            if TcpStream::connect(addr).is_err() {
                closed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(closed, "the dashboard must stop listening after stop()");
    }

    #[test]
    fn concurrent_requests_do_not_serialise_on_a_poisoned_lock() {
        // The router holds no locks; this is a smoke test that two threads can
        // call it with different sources without interference.
        let hits = Arc::new(AtomicUsize::new(0));
        let mut hs = Vec::new();
        for i in 0..4u64 {
            let hits = hits.clone();
            hs.push(std::thread::spawn(move || {
                let o = opts(&format!("thread{i}"));
                let r = handle("GET", "/api/status", "", &source(Some(i)), &o, 1);
                assert_eq!(r.code, 200);
                hits.fetch_add(1, Ordering::SeqCst);
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(hits.load(Ordering::SeqCst), 4);
    }
}
