//! Home-page job applications: validate, append to a JSONL ledger, then
//! optionally deliver a Discord notification.
//!
//! The access point's `POST /join` is the only caller. Ordering is deliberate:
//! **store first, deliver second**. A Discord outage must never lose an
//! application, so delivery failure is reported (and logged) but the request
//! still succeeds.
//!
//! Nothing here holds or logs the webhook URL: it is read from the environment
//! or `$GPN_PREFIX/join-webhook.env`, and reqwest errors are stripped of their
//! URL before they reach a log line (`reqwest::Error::without_url`).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Largest application body we will parse.
pub const MAX_BODY: usize = 8 * 1024;
/// Applications accepted per IP per [`RATE_WINDOW_SECS`].
pub const RATE_MAX: u32 = 5;
/// Rate-limit window.
pub const RATE_WINDOW_SECS: u64 = 3600;
/// Per-field caps: name, email, role, message.
pub const LIMITS: (usize, usize, usize, usize) = (80, 200, 60, 4000);
/// How much of the message the Discord embed carries.
pub const EMBED_MESSAGE_CHARS: usize = 1000;

/// A validated application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    pub name: String,
    pub email: String,
    pub role: String,
    pub message: String,
}

fn field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Honeypot convention: the hidden `website` input is only ever filled by a bot.
pub fn is_honeypot(v: &Value) -> bool {
    !field(v, "website").is_empty()
}

/// Minimal RFC-5322-ish address check: one `@`, no whitespace, a dotted domain
/// whose last label is 2-24 characters. Deliberately strict about shape and
/// lenient about everything else (it is a filter, not a verifier).
pub fn email_ok(email: &str) -> bool {
    if email.is_empty() || email.len() > LIMITS.1 {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if local.is_empty() || local.len() > 64 || domain.is_empty() {
        return false;
    }
    if email.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !host.is_empty()
        && host.len() <= 190
        && (2..=24).contains(&tld.len())
        && !tld.contains('.')
        && !tld.contains('@')
}

/// Validate a parsed application body.
pub fn validate(v: &Value) -> Result<Application, String> {
    if !v.is_object() {
        return Err("Expected a JSON object.".into());
    }
    let name = field(v, "name");
    if name.chars().count() < 2 {
        return Err("A name of at least 2 characters is required.".into());
    }
    if name.chars().count() > LIMITS.0 {
        return Err("That name is too long (80 characters max).".into());
    }
    let email = field(v, "email");
    if email.is_empty() {
        return Err("An email address is required.".into());
    }
    if !email_ok(&email) {
        return Err("That email address doesn't look valid.".into());
    }
    let role = {
        let r = field(v, "role");
        if r.is_empty() {
            "unspecified".to_string()
        } else {
            r.chars().take(LIMITS.2).collect()
        }
    };
    let message = field(v, "message");
    if message.chars().count() > LIMITS.3 {
        return Err("Please keep the message under 4000 characters.".into());
    }
    Ok(Application {
        name,
        email,
        role,
        message,
    })
}

/// `$GPN_PREFIX`/`~/.gpn`.
pub fn prefix_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("GPN_PREFIX") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".gpn")
}

/// Append-only ledger. `GPN_JOIN_LEDGER` overrides (tests use a temp file).
pub fn ledger_path() -> PathBuf {
    std::env::var_os("GPN_JOIN_LEDGER")
        .map(PathBuf::from)
        .unwrap_or_else(|| prefix_dir().join("join-applications.jsonl"))
}

/// A short, random, URL-safe id.
pub fn new_id() -> String {
    let mut buf = [0u8; 6];
    if getrandom::getrandom(&mut buf).is_ok() {
        return buf.iter().map(|b| format!("{b:02x}")).collect();
    }
    // Fallback: time-derived. Still unique enough for a ledger line.
    format!("{:x}", crate::abuse::now_secs())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a unix timestamp (no chrono dependency).
pub fn iso8601(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let secs = (ts % 86_400) as i64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's days-from-civil, inverted (days since 1970-01-01 → y/m/d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Append one JSON line. Never rewrites: open with `append(true)`, `create(true)`.
/// The file (and anything we create) is 0600.
pub fn append(path: &Path, record: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Tighten a pre-existing file too (e.g. created by an older deploy).
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    writeln!(f, "{}", serde_json::to_string(record)?)?;
    f.flush()
}

/// `KEY=value` lookup in a dotenv-style file (surrounding quotes tolerated).
pub fn webhook_from_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("GPN_JOIN_WEBHOOK=")?;
        let v = rest.trim().trim_matches('"').trim_matches('\'').trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// `GPN_JOIN_WEBHOOK` from the environment, else `$GPN_PREFIX/join-webhook.env`.
/// Returns `None` rather than an error: a missing webhook is not fatal.
pub fn webhook() -> Option<String> {
    if let Ok(v) = std::env::var("GPN_JOIN_WEBHOOK") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let path = std::env::var_os("GPN_JOIN_WEBHOOK_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| prefix_dir().join("join-webhook.env"));
    webhook_from_file(&path)
}

/// The Discord message body. Message text is truncated on a char boundary so a
/// multi-byte character can never panic or split.
pub fn discord_payload(rec: &Value, app: &Application) -> Value {
    let msg: String = app.message.chars().take(EMBED_MESSAGE_CHARS).collect();
    let msg = if app.message.chars().count() > EMBED_MESSAGE_CHARS {
        format!("{msg}…")
    } else if msg.is_empty() {
        "_(no message)_".to_string()
    } else {
        msg
    };
    let id = rec.get("id").and_then(|v| v.as_str()).unwrap_or("-");
    let at = rec
        .get("received_at")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    json!({
        "username": "GPN applications",
        "embeds": [{
            "title": "New GPN application",
            "color": 0x3b82f6u32,
            "fields": [
                {"name": "Name", "value": app.name, "inline": true},
                {"name": "Email", "value": app.email, "inline": true},
                {"name": "Role", "value": app.role, "inline": true},
                {"name": "Message", "value": msg, "inline": false}
            ],
            "footer": {"text": format!("id {id} · {at}")}
        }]
    })
}

/// A reqwest failure, with the webhook URL scrubbed and the cause chain kept.
/// The cause matters (DNS vs TLS vs HTTP) and none of it may carry the URL.
fn describe_err(e: &reqwest::Error, secret: &str) -> String {
    let mut out = e.to_string();
    if let Some(url) = e.url() {
        out = out.replace(url.as_str(), "<webhook>");
    }
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(inner) = src {
        out.push_str(&format!(": {inner}"));
        src = inner.source();
    }
    if !secret.is_empty() {
        out = out.replace(secret, "<webhook>");
    }
    out
}

/// POST the embed. Returns the HTTP status on success; an error string that is
/// guaranteed not to contain the webhook URL.
pub fn deliver(webhook_url: &str, payload: &Value) -> Result<u16, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| describe_err(&e, webhook_url))?;
    let resp = client
        .post(webhook_url)
        .json(payload)
        .send()
        .map_err(|e| describe_err(&e, webhook_url))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        Ok(status)
    } else {
        Err(format!("http {status}"))
    }
}

/// Per-IP sliding window. In-memory (a restart forgets it) — the point is to
/// blunt a naive flood of the public form, not to be an accounting system.
fn buckets() -> &'static Mutex<HashMap<String, (u32, u64)>> {
    static B: std::sync::OnceLock<Mutex<HashMap<String, (u32, u64)>>> = std::sync::OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

/// [`rate_limit_ok`] with an explicit clock (tests).
pub fn rate_limit_ok_at(ip: &str, now: u64) -> bool {
    let mut m = buckets().lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 10_000 {
        m.retain(|_, (_, reset)| *reset > now);
    }
    let slot = m
        .entry(ip.to_string())
        .or_insert((0, now + RATE_WINDOW_SECS));
    if now >= slot.1 {
        *slot = (1, now + RATE_WINDOW_SECS);
        return true;
    }
    if slot.0 >= RATE_MAX {
        return false;
    }
    slot.0 += 1;
    true
}

/// [`rate_limit_ok_at`] against the wall clock.
pub fn rate_limit_ok(ip: &str) -> bool {
    rate_limit_ok_at(ip, crate::abuse::now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpn-join-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("apps.jsonl")
    }

    fn good() -> Value {
        json!({
            "name": "Ada Lovelace",
            "email": "ada@example.com",
            "role": "GPU / CUDA engineering",
            "message": "I wrote a memory-hard kernel once."
        })
    }

    #[test]
    fn accepts_a_normal_application() {
        let app = validate(&good()).unwrap();
        assert_eq!(app.name, "Ada Lovelace");
        assert_eq!(app.role, "GPU / CUDA engineering");
    }

    #[test]
    fn rejects_missing_and_malformed_fields() {
        let cases = [
            (json!({}), "name"),
            (json!({"name":"x","email":"ada@example.com"}), "name"),
            (
                json!({"name":"Ada Lovelace","email":"not-an-email"}),
                "email",
            ),
            (
                json!({"name":"Ada Lovelace","email":"ada@example.com","message":"x".repeat(4001)}),
                "4000",
            ),
            (json!([1, 2, 3]), "JSON object"),
        ];
        for (body, needle) in cases {
            let err = validate(&body).unwrap_err();
            assert!(
                err.to_lowercase().contains(&needle.to_lowercase()),
                "expected {needle:?} in {err:?}"
            );
        }
    }

    #[test]
    fn email_shapes() {
        for ok in ["a@b.co", "first.last+tag@sub.example.org"] {
            assert!(email_ok(ok), "{ok} should be accepted");
        }
        for bad in ["", "a@b", "a@b.c", "@b.co", "a@.co", "a b@c.co", "a@@b.co"] {
            assert!(!email_ok(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn honeypot_detection() {
        assert!(is_honeypot(&json!({"website": "http://spam.example"})));
        assert!(is_honeypot(&json!({"website": " x "})));
        assert!(!is_honeypot(&json!({"website": ""})));
        assert!(!is_honeypot(&good()));
    }

    #[test]
    fn two_submissions_append_two_lines_at_0600() {
        let path = tmp("append");
        let rec =
            |id: &str| json!({"id": id, "received_at": iso8601(1_700_000_000), "name": "Ada"});
        append(&path, &rec("one")).unwrap();
        append(&path, &rec("two")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "append must never rewrite: {text:?}");
        assert_eq!(
            serde_json::from_str::<Value>(lines[0]).unwrap()["id"],
            "one"
        );
        assert_eq!(
            serde_json::from_str::<Value>(lines[1]).unwrap()["id"],
            "two"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "ledger must be owner-only");
        }
    }

    #[test]
    fn rate_limit_allows_five_then_blocks_and_resets() {
        let ip = "198.51.100.77";
        for i in 0..RATE_MAX {
            assert!(rate_limit_ok_at(ip, 1_000_000), "attempt {i}");
        }
        assert!(!rate_limit_ok_at(ip, 1_000_000));
        // A different IP is unaffected.
        assert!(rate_limit_ok_at("198.51.100.78", 1_000_000));
        // The window expired.
        assert!(rate_limit_ok_at(ip, 1_000_000 + RATE_WINDOW_SECS));
    }

    #[test]
    fn ids_are_short_and_distinct() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 12);
        assert_ne!(a, b);
    }

    #[test]
    fn iso8601_is_utc_and_fixed_width() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn webhook_file_is_parsed() {
        let dir = std::env::temp_dir().join(format!("gpn-join-wh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join("join-webhook.env");
        std::fs::write(
            &env_file,
            "# c\nGPN_JOIN_WEBHOOK=\"https://example.invalid/hook\"\n",
        )
        .unwrap();
        assert_eq!(
            webhook_from_file(&env_file).as_deref(),
            Some("https://example.invalid/hook")
        );
        assert_eq!(webhook_from_file(&dir.join("missing.env")), None);
    }

    #[test]
    fn delivery_to_a_dead_endpoint_fails_without_leaking_it() {
        // Port 1 refuses immediately on every platform we run.
        let err = deliver("http://127.0.0.1:1/hook", &json!({"content": "x"})).unwrap_err();
        assert!(!err.contains("127.0.0.1"), "error leaked the URL: {err}");
    }

    #[test]
    fn embed_truncates_a_long_message() {
        let mut v = good();
        v["message"] = json!("x".repeat(3999));
        let app = validate(&v).unwrap();
        let payload = discord_payload(&json!({"id": "abc", "received_at": "now"}), &app);
        let text = payload["embeds"][0]["fields"][3]["value"].as_str().unwrap();
        assert!(text.chars().count() <= EMBED_MESSAGE_CHARS + 1);
        assert!(text.ends_with('…'));
    }
}
