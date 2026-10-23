//! Integration tests for idle mode + the local dashboard, driven through the
//! **public** API only (`gpn_agent::idle`, `gpn_agent::gui`).
//!
//! No chain, no GPU, no ollama: the whole point of the design is that idle is a
//! small, testable state machine plus one file, and the dashboard is a pure
//! router over a `StatusSource`.
//!
//! `GPN_IDLE_FILE` is process-global, so the one test that uses it owns a
//! serialisation lock and every other test passes an explicit path.

use gpn_agent::gui::{self, GuiOptions, StatusSource};
use gpn_agent::idle;
use serde_json::{json, Value};
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Unique temp path for one test.
fn tmp(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gpn-idle-it-{tag}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("idle.json")
}

fn opts(path: PathBuf, force: bool) -> GuiOptions {
    GuiOptions {
        listen: "127.0.0.1:0".into(),
        force_idle: force,
        idle_path: Some(path),
        auto_idle: None,
    }
}

fn src(peers: Option<u64>) -> Arc<dyn StatusSource> {
    Arc::new(move || {
        json!({
            "node": "up",
            "peers": peers,
            "best": 1234,
            "mining": true,
            "gpu": "test gpu",
            "models": ["llama3.2:1b"],
        })
    })
}

/// Minimal HTTP client (no extra dev-dependency).
fn http(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
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

/// The claim gate the providers use, end to end through the env override:
/// active → idle → auto-expire → active.
#[test]
fn idle_state_survives_reads_and_expires_to_active() {
    let _g = lock();
    let p = tmp("machine");
    std::env::set_var("GPN_IDLE_FILE", &p);
    let now = idle::now_unix();

    // Active to start with: providers may claim.
    assert!(idle::claims_allowed());
    assert!(idle::current().is_none());

    // Idle for 60 minutes.
    let st = idle::start_at(&p, 60, "gaming", now).unwrap();
    assert_eq!(st.until, now + 3_600);
    assert_eq!(idle::current().map(|s| s.until), Some(st.until));
    assert!(
        !idle::claims_allowed(),
        "providers must not claim while idle"
    );
    assert!(!idle::refusal_reason(Some(4), false).is_some());

    // Restarting the process does not lose it: the file is the state.
    assert_eq!(
        idle::current().map(|s| s.reason),
        Some("gaming".to_string())
    );

    // A file that expired while the node was down reads as ACTIVE and is pruned.
    idle::write_state(
        &p,
        &idle::IdleState {
            until: now.saturating_sub(1),
            reason: "old".into(),
            started: now.saturating_sub(3_601),
        },
    )
    .unwrap();
    assert!(
        idle::current().is_none(),
        "expired file must read as active"
    );
    assert!(idle::claims_allowed());
    assert!(!p.exists(), "expired file is pruned on read");

    std::env::remove_var("GPN_IDLE_FILE");
}

/// Six hours is the ceiling; longer is refused with a message naming it, and
/// zero is refused too.
#[test]
fn duration_limits_are_enforced() {
    let p = tmp("limits");
    assert!(idle::validate_minutes(360).is_ok());
    let err = idle::validate_minutes(361).unwrap_err();
    assert!(err.to_string().contains("360"));
    assert!(idle::validate_minutes(0).is_err());
    assert!(idle::start_at(&p, 361, "x", idle::now_unix()).is_err());
    assert!(!p.exists());
    // Config-sourced values are clamped instead (a default, not an instruction).
    assert_eq!(idle::clamp_minutes(10_000), 360);
}

/// The safety valve: a peer-less node may be the only block producer.
#[test]
fn sole_authority_idle_is_refused_unless_forced() {
    assert!(idle::refusal_reason(Some(0), false).is_some());
    assert!(idle::refusal_reason(None, false).is_some());
    assert!(idle::refusal_reason(Some(0), true).is_none());
    assert!(idle::refusal_reason(Some(1), false).is_none());
}

// ── the dashboard over real HTTP ───────────────────────────────────────────

#[test]
fn dashboard_endpoints_over_http() {
    let p = tmp("api");
    let h = gui::spawn(opts(p.clone(), false), src(Some(3))).expect("bind");
    let addr = h.addr;

    // GET / — the page itself (embedded, no external asset).
    let (code, body) = http(addr, "GET", "/", "");
    assert_eq!(code, 200);
    assert!(
        body.contains("Idle mode"),
        "page must render the idle panel"
    );
    assert!(!body.contains("https://cdn"), "no CDN assets allowed");

    // GET /api/status — 200 with the status document + idle block.
    let (code, body) = http(addr, "GET", "/api/status", "");
    assert_eq!(code, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["peers"], 3);
    assert_eq!(v["best"], 1234);
    assert_eq!(v["idle"]["active"], false);
    assert_eq!(v["max_idle_minutes"], 360);

    // POST /api/idle — 200 and a countdown in status.
    let (code, body) = http(addr, "POST", "/api/idle", r#"{"minutes":120}"#);
    assert_eq!(code, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["minutes"], 120);
    assert!(v["until"].as_u64().unwrap() > idle::now_unix());
    let (_, s) = http(addr, "GET", "/api/status", "");
    let sv: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(sv["idle"]["active"], true);
    assert!(sv["idle"]["remaining_secs"].as_u64().unwrap() > 7_000);

    // POST /api/idle with a bad body — 400, state untouched.
    for bad in [
        r#"{"minutes":0}"#,
        r#"{"minutes":999}"#,
        r#"{"minutes":"1h"}"#,
    ] {
        let (code, body) = http(addr, "POST", "/api/idle", bad);
        assert_eq!(code, 400, "{bad} → {code} {body}");
        assert!(body.contains("\"ok\":false"));
    }

    // POST /api/resume — 200 and back to active.
    let (code, body) = http(addr, "POST", "/api/resume", "");
    assert_eq!(code, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["resumed"], true);
    assert_eq!(v["idle"]["active"], false);

    // Unknown route — 404 JSON.
    let (code, _) = http(addr, "GET", "/api/nope", "");
    assert_eq!(code, 404);

    h.stop();
}

/// A peer-less node cannot be idled from the dashboard either — 409 with a
/// machine-readable reason — unless the operator forced it.
#[test]
fn dashboard_refuses_to_idle_a_lone_authority() {
    let p = tmp("lone");
    let h = gui::spawn(opts(p.clone(), false), src(Some(0))).expect("bind");
    let (code, body) = http(h.addr, "POST", "/api/idle", r#"{"minutes":60}"#);
    assert_eq!(code, 409, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["refused"], "sole-authority");
    assert!(!p.exists(), "a refused request must not idle the node");
    h.stop();

    // Forced: the operator knows it is a single-box dev chain.
    let p2 = tmp("lone-forced");
    let h2 = gui::spawn(opts(p2.clone(), true), src(Some(0))).expect("bind");
    let (code, body) = http(h2.addr, "POST", "/api/idle", r#"{"minutes":60}"#);
    assert_eq!(code, 200, "{body}");
    h2.stop();
    assert!(idle::read_state_at(&p2).is_some());
}

/// The dashboard is a local control surface: it must never be bound to a
/// non-loopback address, because it can stop the miner.
#[test]
fn dashboard_refuses_non_loopback_binds() {
    let p = tmp("bind");
    for bad in ["0.0.0.0:8099", "192.168.0.5:8099", "10.1.2.3:8099"] {
        let o = GuiOptions {
            listen: bad.into(),
            force_idle: false,
            idle_path: Some(p.clone()),
            auto_idle: None,
        };
        assert!(
            gui::spawn(o, src(Some(1))).is_err(),
            "{bad} must be refused"
        );
    }
}
