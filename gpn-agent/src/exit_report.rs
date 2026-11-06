//! One loud, machine-readable exit path for every fatal GPN startup failure.
//!
//! ## Why this exists (2026-11-02)
//!
//! Every onboarding failure that session was **silent**. A missing ollama exited
//! 69 with a line buried in a scrollback nobody saw; a missing Visual C++
//! runtime died with `0xC0000135` and printed *nothing at all*; a corrupt
//! wallet, a failed node-key generation and a busy port each ended with a
//! closed window and no reason. The installer got better; the *runtime* exits
//! stayed mute.
//!
//! This module is the single door every fatal exit goes through:
//!
//! 1. a boxed, readable block on stderr — the failing check, what was found,
//!    and the exact command that fixes it;
//! 2. one JSON object appended to `~/.gpn/last-exit.json` (NDJSON: one object
//!    per line, newest last) so a launcher, a service manager or a support
//!    engineer can read the reason *after* the window closed;
//! 3. on Linux, a single `logger -t gpn-node …` line so it lands in syslog even
//!    when stdout went to a scheduler's bit bucket.
//!
//! The exit codes are preserved exactly (69 no-ollama, 78 not-dialable, 3
//! corrupt key, 4 base path, 2 bad config). Automation — the Windows launcher,
//! the systemd units, the supervisor's restart logic — depends on them.
//!
//! This file is shared verbatim with the chain crate via `#[path]`
//! (`gpn-node/node/src/main.rs`), so it must stay free of gpn-specific imports.
//! Only `std` + `serde`/`serde_json` are used.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A fatal-exit category. `code` is the process exit status (kept stable for
/// automation); `slug` is the stable, human/machine-readable name written to
/// `last-exit.json` (`"no-ollama"`, `"not-dialable"`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitKind {
    pub code: i32,
    pub slug: &'static str,
}

impl ExitKind {
    /// 69 `EX_UNAVAILABLE` — ollama is required and unreachable.
    pub const NO_OLLAMA: ExitKind = ExitKind {
        code: 69,
        slug: "no-ollama",
    };
    /// 78 `EX_CONFIG` — no evidence of inbound reachability.
    pub const NOT_DIALABLE: ExitKind = ExitKind {
        code: 78,
        slug: "not-dialable",
    };
    /// 78 `EX_CONFIG` — behind CGNAT; no forward is possible without help.
    pub const CGNAT: ExitKind = ExitKind {
        code: 78,
        slug: "cgnat",
    };
    /// 3 — the wallet keystore exists but is unusable/corrupt.
    pub const CORRUPT_KEY: ExitKind = ExitKind {
        code: 3,
        slug: "corrupt-key",
    };
    /// 4 — the data directory could not be created.
    pub const BASE_PATH: ExitKind = ExitKind {
        code: 4,
        slug: "base-path",
    };
    /// 2 — the config file / CLI could not be parsed.
    pub const BAD_CONFIG: ExitKind = ExitKind {
        code: 2,
        slug: "bad-config",
    };
    /// 2 — the supervisor computed an empty child set (nothing to run).
    pub const NO_CHILDREN: ExitKind = ExitKind {
        code: 2,
        slug: "no-children",
    };
    /// 67 `EX_NOUSER` — no usable compute GPU was found. A GPN node is
    /// miner + AI provider on one GPU; with no GPU (or no driver exposing it to
    /// `nvidia-smi`/`rocm-smi`) the node must say so and stop, instead of
    /// running with a fabricated device.
    pub const NO_GPU: ExitKind = ExitKind {
        code: 67,
        slug: "no-gpu",
    };
    /// 75 `EX_TEMPFAIL` — a required port is held by another process.
    pub const PORT_IN_USE: ExitKind = ExitKind {
        code: 75,
        slug: "port-in-use",
    };
    /// 65 `EX_DATAERR` — the chain spec / genesis hash is not the pinned one.
    pub const CHAIN_SPEC: ExitKind = ExitKind {
        code: 65,
        slug: "chain-spec",
    };
    /// 74 `EX_IOERR` — the filesystem holding the node is nearly full.
    pub const DISK_FULL: ExitKind = ExitKind {
        code: 74,
        slug: "disk-full",
    };
    /// 66 `EX_NOINPUT` — the binary could not load the Visual C++ runtime.
    /// Reported by the Windows *launcher* (the process itself never runs).
    pub const VC_RUNTIME: ExitKind = ExitKind {
        code: 66,
        slug: "vc-runtime",
    };
    /// `0xC000013A` (`STATUS_CONTROL_C_EXIT`) — the console this process was
    /// attached to was closed, so Windows delivered `CTRL_CLOSE_EVENT` and the
    /// process terminated. A node started with `Start-Process -WindowStyle
    /// Hidden` inherits the installer's console and dies with it.
    ///
    /// The code is kept as the *signed* i32 Windows reports through
    /// `%ERRORLEVEL%` (-1073741510); `kind_for_exit_code` accepts both the
    /// signed and the unsigned spelling.
    pub const CONSOLE_CLOSED: ExitKind = ExitKind {
        code: WINDOWS_CONSOLE_CLOSE,
        slug: "console-closed",
    };
    /// 75 — the launcher saw the same fatal failure 3 times in 10 minutes and
    /// stopped restarting (a restart loop is its own silent failure).
    pub const CRASH_LOOP: ExitKind = ExitKind {
        code: 75,
        slug: "crash-loop",
    };
    /// 1 — a provider loop could not start; the supervisor restarts it.
    pub const PROVIDER: ExitKind = ExitKind {
        code: 1,
        slug: "provider",
    };
    /// 1 — first-run proof-of-work onboarding (spec 47) did not complete.
    /// Non-fatal: the node still runs, but it is not a registered provider.
    pub const ONBOARD: ExitKind = ExitKind {
        code: 1,
        slug: "onboard",
    };
    /// 1 — anything else that must never exit silently.
    pub const GENERIC: ExitKind = ExitKind {
        code: 1,
        slug: "generic",
    };
}

/// `0xC000013A` as the signed i32 Windows hands to `%ERRORLEVEL%` and to
/// `ExitStatus::code()`. The unsigned spelling is 3221225786.
pub const WINDOWS_CONSOLE_CLOSE: i32 = -1_073_741_510;

/// The truthful one-liner for a console-close exit (shared by the supervisor,
/// the exit log and the launcher's generated reporter).
pub const CONSOLE_CLOSED_DETAIL: &str = "the console this node was started from was closed";

/// The fix that actually works: an inherited console is the problem, so stop
/// inheriting one.
pub const CONSOLE_CLOSED_REMEDIATION: &str =
    "start it detached (the installer does this) or run it as a scheduled task";

impl std::fmt::Display for ExitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (exit {})", self.slug, self.code)
    }
}

/// One recorded fatal exit. Serialised as a single NDJSON line.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Report {
    /// RFC-3339 UTC timestamp, e.g. `2026-11-03T17:06:00Z`.
    pub ts: String,
    /// Unix seconds — lets a launcher do the 3-strikes arithmetic without
    /// parsing dates.
    #[serde(default)]
    pub ts_unix: u64,
    pub code: i32,
    pub kind: String,
    pub detail: String,
    pub remediation: String,
    pub version: String,
    pub os: String,
    #[serde(default)]
    pub pid: u32,
}

/// Where the exit log lives.
///
/// `GPN_EXIT_LOG` wins (tests, and any launcher that relocates the prefix);
/// otherwise `$GPN_PREFIX/last-exit.json`; otherwise `~/.gpn/last-exit.json`
/// (`%USERPROFILE%\.gpn\last-exit.json` on Windows).
pub fn default_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_EXIT_LOG") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    let prefix = std::env::var("GPN_PREFIX").unwrap_or_else(|_| format!("{home}/.gpn"));
    PathBuf::from(prefix).join("last-exit.json")
}

/// Build a report for `kind` with a guaranteed non-empty detail and fix.
pub fn new_report(kind: ExitKind, detail: impl AsRef<str>, remediation: impl AsRef<str>) -> Report {
    Report {
        ts: rfc3339(now_unix()),
        ts_unix: now_unix(),
        code: kind.code,
        kind: kind.slug.to_string(),
        detail: non_empty(detail.as_ref(), "(no detail was recorded)"),
        remediation: non_empty(
            remediation.as_ref(),
            "run `gpn_node --diagnose` for a PASS/FAIL checklist with the fix per line",
        ),
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        pid: std::process::id(),
    }
}

fn non_empty(s: &str, fallback: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        fallback.to_string()
    } else {
        t.to_string()
    }
}

/// The boxed, human-readable block printed to stderr.
pub fn render(r: &Report) -> String {
    const W: usize = 76;
    let mut out = String::new();
    let rule = format!("+{}+", "-".repeat(W));
    let blank = format!("|{}|", " ".repeat(W));
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&format!("|{}|\n", pad(" GPN NODE STOPPED", W)));
    out.push_str(&format!(
        "|{}|\n",
        pad(&format!(" {}", r.kind.to_uppercase()), W)
    ));
    out.push_str(&format!(
        "|{}|\n",
        pad(
            &format!(
                " exit code {}  ·  version {}  ·  {}",
                r.code, r.version, r.os
            ),
            W
        )
    ));
    out.push_str(&rule);
    out.push('\n');

    let mut field = |label: &str, body: &str| {
        let lines = wrap(body, W - 18);
        for (i, line) in lines.iter().enumerate() {
            let name = if i == 0 { label } else { "" };
            out.push_str(&format!(" {:<14} {}\n", name, line));
        }
    };
    field("What failed :", &r.kind);
    field("What I found:", &r.detail);
    field("Fix         :", &r.remediation);
    let path = default_path();
    field("Recorded    :", &path.to_string_lossy());
    field(
        "Help        :",
        "docs/run-a-node.md  ·  gpn_node --diagnose",
    );
    out.push_str(&blank);
    out.push('\n');
    out.push_str(
        "[gpn-node] this was NOT a silent crash — the reason above is also in the exit log.\n",
    );
    out
}

/// Left-pad/truncate one box line to exactly `w` columns.
fn pad(s: &str, w: usize) -> String {
    let mut s = s.chars().take(w).collect::<String>();
    let len = s.chars().count();
    if len < w {
        s.push_str(&" ".repeat(w - len));
    }
    s
}

/// Word-wrap `text` to `width` columns, preserving explicit newlines.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for para in text.lines() {
        if para.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in para.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
            // A single word longer than the column: hard-split it.
            while line.chars().count() > width {
                let head: String = line.chars().take(width).collect();
                out.push(head);
                line = line.chars().skip(width).collect();
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Serialise one report (no trailing newline).
pub fn to_json_line(r: &Report) -> String {
    serde_json::to_string(r).unwrap_or_else(|_| {
        // Never let serialisation be the reason the reason is lost.
        format!(
            "{{\"ts\":\"{}\",\"code\":{},\"kind\":\"{}\",\"detail\":\"(unserialisable)\",\
             \"remediation\":\"{}\",\"version\":\"{}\",\"os\":\"{}\"}}",
            r.ts,
            r.code,
            r.kind,
            r.remediation.replace('"', "'"),
            r.version,
            r.os
        )
    })
}

/// Append `r` to the exit log at `path`. Best-effort: a failure to write the log
/// must never mask the original failure.
pub fn record_at(path: &Path, r: &Report) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
    }
    let line = format!("{}\n", to_json_line(r));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("cannot append to {}: {e}", path.display()))?;
    let _ = f.flush();
    maybe_trim(path);
    Ok(())
}

/// Keep the log bounded (a crash loop must not grow it without limit).
fn maybe_trim(path: &Path) {
    const KEEP: usize = 32;
    const TRIGGER: usize = 64;
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= TRIGGER {
        return;
    }
    let tail = lines[lines.len() - KEEP..].join("\n") + "\n";
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, tail).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Record to the default path, best-effort.
pub fn record(r: &Report) {
    let _ = record_at(&default_path(), r);
}

/// On Linux, mirror one line to syslog so a scheduled task's closed window is
/// not the end of the story. Never blocks the exit and never fails.
#[cfg(target_os = "linux")]
fn syslog_line(r: &Report) {
    let line = format!(
        "fatal kind={} code={} detail={} fix={}",
        r.kind, r.code, r.detail, r.remediation
    );
    // `logger` is present on every supported distro; ignore every failure (a
    // missing logger must not change the exit code or print noise).
    let _ = std::process::Command::new("logger")
        .args(["-t", "gpn-node", "--", &line])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(target_os = "linux"))]
fn syslog_line(_r: &Report) {}

/// Print the block, record it, and return the exit code — for call sites that
/// already return an `i32` to `main` (`run_ensure_wallet`, gates, …).
pub fn emit(kind: ExitKind, detail: impl AsRef<str>, remediation: impl AsRef<str>) -> i32 {
    let r = new_report(kind, detail, remediation);
    eprint!("{}", render(&r));
    record(&r);
    syslog_line(&r);
    kind.code
}

/// The one fatal exit. Prints, records, logs, then terminates with the
/// category's stable exit code.
pub fn fatal(kind: ExitKind, detail: impl AsRef<str>, remediation: impl AsRef<str>) -> ! {
    let code = emit(kind, detail, remediation);
    std::process::exit(code)
}

/// Read every recorded report (oldest first). Unparseable lines are skipped:
/// a torn write must not hide the entries around it.
pub fn read_reports(path: &Path) -> Vec<Report> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|l| serde_json::from_str::<Report>(l).ok())
        .collect()
}

/// 3-strikes detection: `Some(last)` when the same `kind` appears at least
/// `strikes` times inside `window_secs` before `now_unix`.
///
/// Pure, so the launcher's rule is unit-tested here rather than only in
/// PowerShell.
pub fn crash_loop(
    reports: &[Report],
    now_unix: u64,
    window_secs: u64,
    strikes: usize,
) -> Option<&Report> {
    if strikes == 0 {
        return None;
    }
    let last = reports.last()?;
    let cutoff = now_unix.saturating_sub(window_secs);
    let n = reports
        .iter()
        .filter(|r| r.kind == last.kind && r.ts_unix >= cutoff)
        .count();
    (n >= strikes).then_some(last)
}

/// Best-effort category for a free-form startup error, so a busy port or a bad
/// chain spec is not reported as a generic crash. Never used to *change* an
/// exit code that a call site already knows (those pass an `ExitKind`).
pub fn kind_for_text(msg: &str) -> ExitKind {
    let m = msg.to_ascii_lowercase();
    if m.contains("address already in use")
        || m.contains("os error 98")
        || m.contains("os error 10048")
        || m.contains("cannot bind")
        || m.contains("failed to bind")
    {
        return ExitKind::PORT_IN_USE;
    }
    if m.contains("chain spec") || m.contains("genesis") {
        return ExitKind::CHAIN_SPEC;
    }
    ExitKind::GENERIC
}

/// The one-line fix that belongs with [`kind_for_text`]'s category.
pub fn default_fix(kind: ExitKind) -> &'static str {
    match kind {
        ExitKind::NO_GPU => {
            "no compute GPU found: check that `nvidia-smi` runs (Windows: the \
             driver is installed; Linux: the NVIDIA driver is loaded), and that \
             the card is not disabled in Device Manager"
        }
        ExitKind::CONSOLE_CLOSED => CONSOLE_CLOSED_REMEDIATION,
        ExitKind::VC_RUNTIME => {
            "install the Microsoft Visual C++ runtime: winget install --id \
             Microsoft.VCRedist.2015+.x64 -e"
        }
        ExitKind::PORT_IN_USE => {
            "stop the process holding the port, or move this node with GPN_RPC_PORT / \
             GPN_LISTEN / GPN_GUI_LISTEN; `gpn_node --diagnose` names the busy port"
        }
        ExitKind::CHAIN_SPEC => {
            "delete the local spec file and re-run so the verified spec is re-fetched; \
             never hand-edit genesis (`gpn_node --diagnose` checks it)"
        }
        ExitKind::NO_OLLAMA => "gpn_node --setup",
        ExitKind::CORRUPT_KEY => {
            "restore the keystore from backup, then `gpn_node --ensure-wallet`"
        }
        _ => "run `gpn_node --diagnose` for a PASS/FAIL checklist with the fix per line",
    }
}

/// True when `code` is Windows' `0xC000013A` — the process died because the
/// console it was attached to went away. Both spellings (`-1073741510` and
/// `3221225786`) are the same 32-bit value.
pub fn is_console_close(code: i32) -> bool {
    code as u32 == 0xC000_013A
}

/// Map a raw process exit status to a known fatal kind, when we can be sure.
///
/// Deliberately conservative: an unknown status returns `None` so a caller can
/// keep its own category (the supervisor uses `provider` for it) instead of
/// being told the wrong reason. Used to classify a *child's* exit code, which
/// arrives with no "kind" attached.
pub fn kind_for_exit_code(code: i32) -> Option<ExitKind> {
    if is_console_close(code) {
        return Some(ExitKind::CONSOLE_CLOSED);
    }
    match code as u32 {
        0xC000_0135 => Some(ExitKind::VC_RUNTIME),
        69 => Some(ExitKind::NO_OLLAMA),
        78 => Some(ExitKind::NOT_DIALABLE),
        3 => Some(ExitKind::CORRUPT_KEY),
        4 => Some(ExitKind::BASE_PATH),
        2 => Some(ExitKind::BAD_CONFIG),
        75 => Some(ExitKind::PORT_IN_USE),
        _ => None,
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// RFC-3339 UTC without a date crate (Howard Hinnant's civil-from-days).
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    if m <= 2 {
        y += 1;
    }
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(kind: ExitKind, ts_unix: u64) -> Report {
        Report {
            ts: rfc3339(ts_unix),
            ts_unix,
            code: kind.code,
            kind: kind.slug.to_string(),
            detail: "detail".into(),
            remediation: "fix".into(),
            version: "0.0.0".into(),
            os: "test".into(),
            pid: 1,
        }
    }

    #[test]
    fn exit_codes_are_the_documented_ones() {
        assert_eq!(ExitKind::NO_OLLAMA.code, 69);
        assert_eq!(ExitKind::NOT_DIALABLE.code, 78);
        assert_eq!(ExitKind::CORRUPT_KEY.code, 3);
        assert_eq!(ExitKind::BASE_PATH.code, 4);
        assert_eq!(ExitKind::BAD_CONFIG.code, 2);
    }

    #[test]
    fn empty_detail_and_fix_are_never_emitted_empty() {
        let r = new_report(ExitKind::GENERIC, "   ", "");
        assert!(!r.detail.trim().is_empty());
        assert!(!r.remediation.trim().is_empty());
        assert!(r.remediation.contains("--diagnose"));
    }

    #[test]
    fn json_line_round_trips_with_every_required_field() {
        let r = new_report(
            ExitKind::NO_OLLAMA,
            "refused at 127.0.0.1:11434",
            "gpn_node --setup",
        );
        let line = to_json_line(&r);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        for k in [
            "ts",
            "code",
            "kind",
            "detail",
            "remediation",
            "version",
            "os",
        ] {
            assert!(v.get(k).is_some(), "missing {k}: {line}");
        }
        assert_eq!(v["code"], 69);
        assert_eq!(v["kind"], "no-ollama");
    }

    #[test]
    fn boxed_block_mentions_kind_detail_and_fix() {
        let r = new_report(
            ExitKind::CORRUPT_KEY,
            "keystore.json is not JSON",
            "restore the file from backup",
        );
        let text = render(&r);
        assert!(text.contains("GPN NODE STOPPED"));
        assert!(text.contains("CORRUPT-KEY"));
        assert!(text.contains("69") || text.contains("3"));
        assert!(text.contains("keystore.json is not JSON"));
        assert!(text.contains("restore the file from backup"));
        assert!(text.contains("Fix"));
    }

    #[test]
    fn record_appends_one_line_per_failure_and_survives_a_torn_tail() {
        let dir = std::env::temp_dir().join(format!("gpn-exit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("last-exit.json");
        record_at(&path, &report(ExitKind::NO_OLLAMA, 100)).unwrap();
        record_at(&path, &report(ExitKind::CORRUPT_KEY, 101)).unwrap();
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{not json}}").unwrap();
        }
        record_at(&path, &report(ExitKind::NOT_DIALABLE, 102)).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 4);
        let reports = read_reports(&path);
        assert_eq!(
            reports.len(),
            3,
            "the torn line is skipped, neighbours kept"
        );
        assert_eq!(reports.last().unwrap().kind, "not-dialable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_loop_needs_three_of_the_same_kind_inside_the_window() {
        let now = 10_000u64;
        let two = [
            report(ExitKind::NO_OLLAMA, now - 60),
            report(ExitKind::NO_OLLAMA, now - 30),
        ];
        assert!(crash_loop(&two, now, 600, 3).is_none());

        let three = [
            report(ExitKind::NO_OLLAMA, now - 60),
            report(ExitKind::NO_OLLAMA, now - 30),
            report(ExitKind::NO_OLLAMA, now - 5),
        ];
        assert!(crash_loop(&three, now, 600, 3).is_some());

        // Same count, but one is outside the 10-minute window.
        let spread = [
            report(ExitKind::NO_OLLAMA, now - 3_000),
            report(ExitKind::NO_OLLAMA, now - 30),
            report(ExitKind::NO_OLLAMA, now - 5),
        ];
        assert!(crash_loop(&spread, now, 600, 3).is_none());

        // Different kinds never add up to a loop.
        let mixed = [
            report(ExitKind::NO_OLLAMA, now - 60),
            report(ExitKind::CORRUPT_KEY, now - 30),
            report(ExitKind::NO_OLLAMA, now - 5),
        ];
        assert!(crash_loop(&mixed, now, 600, 3).is_none());
    }

    #[test]
    fn startup_errors_are_classified_without_changing_existing_codes() {
        assert_eq!(
            kind_for_text("Service(Os { code: 98, message: \"Address already in use\" })").slug,
            "port-in-use"
        );
        assert_eq!(kind_for_text("cannot bind 0.0.0.0:9944").code, 75);
        assert_eq!(
            kind_for_text("invalid chain spec: no genesis").slug,
            "chain-spec"
        );
        assert_eq!(kind_for_text("something else entirely").code, 1);
        assert!(!default_fix(ExitKind::PORT_IN_USE).is_empty());
    }

    #[test]
    fn console_close_codes_map_to_console_closed_in_both_spellings() {
        // 0xC000013A, as PowerShell's %ERRORLEVEL% reports it (signed) and as
        // log parsers usually print it (unsigned). They are the same i32.
        assert_eq!(WINDOWS_CONSOLE_CLOSE, -1_073_741_510);
        assert!(is_console_close(-1_073_741_510));
        assert!(is_console_close(0xC000_013Au32 as i32));
        assert!(!is_console_close(1));

        let kind = kind_for_exit_code(-1_073_741_510).expect("console close is classified");
        assert_eq!(kind.slug, "console-closed");
        assert_eq!(kind.code, -1_073_741_510);
        assert_eq!(kind, ExitKind::CONSOLE_CLOSED);
        assert!(!default_fix(kind).trim().is_empty());
        assert!(default_fix(kind).contains("detached"));

        // The record text is truthful, not "unknown".
        let r = new_report(
            kind,
            format!("{CONSOLE_CLOSED_DETAIL} while running"),
            CONSOLE_CLOSED_REMEDIATION,
        );
        let text = render(&r);
        assert!(text.contains("CONSOLE-CLOSED"));
        assert!(text.contains("console this node was started from was closed"));
        assert!(text.contains("start it detached"));
    }

    #[test]
    fn loader_and_known_codes_are_classified_and_unknown_ones_are_not() {
        assert_eq!(
            kind_for_exit_code(0xC000_0135u32 as i32),
            Some(ExitKind::VC_RUNTIME)
        );
        assert_eq!(kind_for_exit_code(69), Some(ExitKind::NO_OLLAMA));
        assert_eq!(kind_for_exit_code(78), Some(ExitKind::NOT_DIALABLE));
        assert_eq!(kind_for_exit_code(3), Some(ExitKind::CORRUPT_KEY));
        assert_eq!(kind_for_exit_code(1_234), None);
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn wrap_never_loses_words_and_respects_width() {
        let text = "The quick brown fox jumps over the lazy dog and keeps running past the fence";
        for line in wrap(text, 20) {
            assert!(line.chars().count() <= 20, "too wide: {line}");
        }
        assert_eq!(wrap(text, 20).join(" "), text);
    }
}
