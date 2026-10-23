//! Progress-based liveness watchdog for the long-lived provider processes
//! (queue item #72).
//!
//! ## Why
//!
//! `gpn-provider` went silent for ~23 minutes on 2026-10-22 while a job sat
//! on-chain `Pending`: the systemd unit stayed `active`, nothing was logged, and
//! the only symptom was a job that never moved. A plain restart fixed it
//! instantly — so the process was wedged, not dead, and nothing noticed.
//!
//! Nothing in the provider loop had a bounded wait: a chain read over a
//! half-open websocket (the RPC client sets no request timeout), a stalled
//! block import, or any future unbounded `await` simply parks the `loop {
//! process_once(..) }` forever. The unit cannot see that: `Type=simple` only
//! knows the PID is alive.
//!
//! ## What this is
//!
//! Two halves, deliberately separate:
//!
//! 1. **A progress tick.** The main loop bumps a counter and writes the current
//!    *phase* (`reconcile`, `claim`, `gpu-wait`, `infer`, `settle`, …) to
//!    `~/.gpn/provider-tick.json` at every milestone. It is an observation
//!    point, not just a liveness ping: a stale tick names the step that hung.
//! 2. **A watchdog thread.** A plain `std::thread` (not a tokio task — a wedged
//!    runtime must not be able to wedge its own supervisor) polls that file. If
//!    no progress has been made for the phase's budget it logs ONE loud WARN
//!    naming the phase and the stall, then exits the process with
//!    [`STALL_EXIT_CODE`], which `Restart=always` turns into a real restart.
//!
//! Phase budgets exist because some steps are legitimately slow: waiting on the
//! cross-process GPU lock can take up to `GPN_GPU_LOCK_TIMEOUT_SECS` (default
//! 900 s) and a single inference is capped at 300 s. A flat timeout short enough
//! to catch a hung chain read would kill those; a flat timeout long enough for
//! them would not catch a 23-minute wedge quickly. The budget is therefore
//! per-phase, and only the *default* (everything non-slow) uses the operator
//! knob.
//!
//! Knobs: `GPN_PROVIDER_WATCHDOG=0` disables, `GPN_PROVIDER_WATCHDOG_SECS`
//! (default 300 s, floor 60 s, cap 24 h) sets the default-phase budget,
//! `GPN_PROVIDER_TICK` overrides the tick path.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Exit status used when the watchdog declares the process wedged. systemd's
/// `Restart=always` restarts on any exit; the distinct code (75 = EX_TEMPFAIL)
/// makes the cause visible in `systemctl --user show -p ExecMainStatus`.
pub const STALL_EXIT_CODE: i32 = 75;

/// Default no-progress budget for ordinary phases.
pub const DEFAULT_STALE_SECS: u64 = 300;
/// Floor / cap for the operator knob.
pub const MIN_STALE_SECS: u64 = 60;
pub const MAX_STALE_SECS: u64 = 24 * 60 * 60;

/// Extra slack on top of a slow phase's own upper bound.
const SLOW_PHASE_MARGIN: Duration = Duration::from_secs(300);
/// The GPU lock wait (default 900 s) plus the inference timeout (300 s).
const DEFAULT_GPU_LOCK_SECS: u64 = 900;
const INFERENCE_TIMEOUT_SECS: u64 = 300;

/// One observation of provider progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    pub pid: u32,
    pub loops: u64,
    /// Step the process is in (or was in when it stopped making progress).
    pub phase: String,
    /// Unix seconds of the last progress bump.
    pub updated_unix: u64,
}

impl Tick {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"pid\":{},\"loops\":{},\"phase\":\"{}\",\"updated_unix\":{}}}",
            self.pid,
            self.loops,
            escape_json(&self.phase),
            self.updated_unix
        )
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Minimal reader for the tick document this module writes. Accepts the keys
/// in any order and tolerates missing fields (a partially written or
/// hand-edited file must never make the watchdog crash — it is the last thing
/// standing between a wedge and an operator).
pub fn parse_tick(s: &str) -> Option<Tick> {
    fn num(s: &str, key: &str) -> Option<u64> {
        let pat = format!("\"{key}\":");
        let i = s.find(&pat)? + pat.len();
        let rest = &s[i..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    }
    fn string(s: &str, key: &str) -> Option<String> {
        let pat = format!("\"{key}\":\"");
        let i = s.find(&pat)? + pat.len();
        let rest = &s[i..];
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some(other) => out.push(other),
                    None => return None,
                },
                c => out.push(c),
            }
        }
        None
    }
    Some(Tick {
        pid: num(s, "pid")? as u32,
        loops: num(s, "loops").unwrap_or(0),
        phase: string(s, "phase").unwrap_or_default(),
        updated_unix: num(s, "updated_unix")?,
    })
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the tick lives.
pub fn tick_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_PROVIDER_TICK") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(format!("{home}/.gpn/provider-tick.json"))
}

/// Default-phase budget from the environment (floor 60 s, cap 24 h).
pub fn stale_after_from_env() -> Duration {
    let secs = std::env::var("GPN_PROVIDER_WATCHDOG_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_STALE_SECS)
        .clamp(MIN_STALE_SECS, MAX_STALE_SECS);
    Duration::from_secs(secs)
}

/// Is the watchdog enabled? (`GPN_PROVIDER_WATCHDOG=0` disables it.)
pub fn watchdog_enabled() -> bool {
    std::env::var("GPN_PROVIDER_WATCHDOG")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// How long a phase may go without progress before it counts as a wedge.
///
/// Slow, legitimately-blocking steps get a budget derived from their own upper
/// bound; everything else gets the operator-configured default.
pub fn stale_budget(phase: &str, default: Duration) -> Duration {
    match phase {
        // Waiting for the cross-process GPU lock: bounded by the lock timeout
        // (env-tunable, default 900 s).
        "gpu-wait" => {
            let lock_secs = std::env::var("GPN_GPU_LOCK_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(DEFAULT_GPU_LOCK_SECS);
            Duration::from_secs(lock_secs) + SLOW_PHASE_MARGIN
        }
        // A single inference call: capped by the ollama client timeout.
        "infer" => Duration::from_secs(INFERENCE_TIMEOUT_SECS) + SLOW_PHASE_MARGIN,
        // Startup does bounded-but-slow work before the loop: an attestation
        // (bounded at 90 s), the on-chain endpoint advert (15 s) and baseline
        // model pulls (a cold pull can take minutes). Generous, but still a
        // bound — a startup that hangs forever is exactly the silent-zombie
        // case this item exists for.
        "startup" => Duration::from_secs(30 * 60),
        _ => default,
    }
}

/// Is this tick stale at `now`? A tick from another process (or a
/// future-dated one) is never stale: the caller falls back to its own start
/// instant, so a leftover file from a previous run cannot kill a fresh one.
pub fn is_stale(tick: Option<&Tick>, pid: u32, now_unix: u64, budget: Duration) -> bool {
    let Some(t) = tick else { return false };
    if t.pid != pid {
        return false;
    }
    now_unix.saturating_sub(t.updated_unix) > budget.as_secs()
}

/// Write the tick atomically (temp + rename) so a reader never sees half a
/// document. Failures are silent: a read-only HOME must not take the provider
/// down — the in-process check still works off `Progress`.
pub fn write_tick(path: &Path, tick: &Tick) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, tick.to_json()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

pub fn read_tick(path: &Path) -> Option<Tick> {
    let s = std::fs::read_to_string(path).ok()?;
    parse_tick(&s)
}

/// The provider's progress state: one shared, cheap-to-update cell.
pub struct Progress {
    path: PathBuf,
    pid: u32,
    loops: u64,
    phase: String,
    last_bump: u64,
    default_budget: Duration,
}

impl Progress {
    pub fn new(path: PathBuf, default_budget: Duration) -> Self {
        Self {
            path,
            pid: std::process::id(),
            loops: 0,
            phase: "startup".to_string(),
            last_bump: now_unix(),
            default_budget,
        }
    }

    /// Mark a new step. Writes the tick only when the phase actually changes,
    /// so a hot inner loop cannot turn this into a write storm.
    pub fn set_phase(&mut self, phase: &str) {
        if self.phase == phase {
            return;
        }
        self.phase = phase.to_string();
        self.bump();
    }

    /// One completed pass: counter + tick write.
    pub fn finish_pass(&mut self) {
        self.loops += 1;
        self.phase = "idle".to_string();
        self.bump();
    }

    pub fn bump(&mut self) {
        self.last_bump = now_unix();
        write_tick(
            &self.path,
            &Tick {
                pid: self.pid,
                loops: self.loops,
                phase: self.phase.clone(),
                updated_unix: self.last_bump,
            },
        );
    }

    pub fn tick(&self) -> Tick {
        Tick {
            pid: self.pid,
            loops: self.loops,
            phase: self.phase.clone(),
            updated_unix: self.last_bump,
        }
    }

    pub fn stale_for(&self, now_unix: u64) -> Duration {
        Duration::from_secs(
            now_unix
                .saturating_sub(self.last_bump)
                .min(Duration::from_secs(MAX_STALE_SECS).as_secs()),
        )
    }

    pub fn is_stale(&self, now_unix: u64) -> bool {
        self.stale_for(now_unix) > stale_budget(&self.phase, self.default_budget)
    }
}

/// One watchdog decision, kept pure so it can be tested without threads.
pub enum Verdict {
    Ok,
    /// A different process owns the file; keep waiting (never a kill).
    ForeignOwner,
    Stalled {
        phase: String,
        silent_for: Duration,
        budget: Duration,
    },
}

pub fn verdict(tick: Option<&Tick>, pid: u32, now_unix: u64, default_budget: Duration) -> Verdict {
    let Some(t) = tick else { return Verdict::Ok };
    if t.pid != pid {
        return Verdict::ForeignOwner;
    }
    let silent = Duration::from_secs(now_unix.saturating_sub(t.updated_unix));
    let budget = stale_budget(&t.phase, default_budget);
    if silent > budget {
        Verdict::Stalled {
            phase: t.phase.clone(),
            silent_for: silent,
            budget,
        }
    } else {
        Verdict::Ok
    }
}

/// The WARN line the watchdog emits before it restarts the process. Returned as
/// a string (instead of printed) so the wording is testable.
pub fn stall_warning(t: &Tick, pid: u32, silent_for: Duration, budget: Duration) -> String {
    format!(
        "⚠ [watchdog] provider made NO progress for {}s (phase \"{}\", {} pass(es) completed, \
         pid {pid}, budget {}s) — the loop is wedged; exiting with {} so systemd restarts it",
        silent_for.as_secs(),
        t.phase,
        t.loops,
        budget.as_secs(),
        STALL_EXIT_CODE
    )
}

/// Arm the watchdog thread. It polls the shared `Progress` cell (not the file:
/// the in-process value cannot be missed or left behind by an aborted write)
/// and, on a stall, logs [`stall_warning`] once and exits the process.
pub fn spawn(progress: std::sync::Arc<std::sync::Mutex<Progress>>, poll: Duration) {
    let poll = poll.max(Duration::from_secs(1));
    let spawned = std::thread::Builder::new()
        .name("gpn-provider-watchdog".to_string())
        .spawn(move || loop {
            std::thread::sleep(poll);
            let (stalled, warning) = {
                let Ok(p) = progress.lock() else { continue };
                let now = now_unix();
                if p.is_stale(now) {
                    let t = p.tick();
                    let budget = stale_budget(&t.phase, p.default_budget);
                    (true, stall_warning(&t, p.pid, p.stale_for(now), budget))
                } else {
                    (false, String::new())
                }
            };
            if stalled {
                eprintln!("{warning}");
                let _ = std::io::Write::flush(&mut std::io::stderr());
                std::process::exit(STALL_EXIT_CODE);
            }
        });
    if let Err(e) = spawned {
        eprintln!("[watchdog] could not start the liveness thread: {e} — no auto-restart armed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(pid: u32, phase: &str, updated: u64, loops: u64) -> Tick {
        Tick {
            pid,
            loops,
            phase: phase.to_string(),
            updated_unix: updated,
        }
    }

    #[test]
    fn tick_roundtrips_through_json() {
        let t = tick(4242, "claim", 1_700_000_000, 17);
        let back = parse_tick(&t.to_json()).expect("parses");
        assert_eq!(back, t);
        // A hostile phase string must not break the document.
        let t = tick(1, "run #7 \"quoted\" \\ nope", 5, 0);
        let back = parse_tick(&t.to_json()).expect("parses");
        assert_eq!(back.phase, t.phase);
    }

    #[test]
    fn a_partial_or_hand_edited_tick_is_tolerated() {
        assert!(parse_tick("").is_none());
        assert!(parse_tick("{\"pid\":1}").is_none());
        let t = parse_tick("{\"pid\":1,\"updated_unix\":1000}").expect("parses");
        assert_eq!(t.loops, 0);
        assert_eq!(t.phase, "");
    }

    #[test]
    fn staleness_respects_the_phase_budget_and_ignores_other_processes() {
        let default = Duration::from_secs(300);
        let now = 10_000u64;
        // Fresh, stale, and foreign-owner cases.
        assert!(!is_stale(
            Some(&tick(7, "claim", now - 10, 1)),
            7,
            now,
            default
        ));
        assert!(is_stale(
            Some(&tick(7, "claim", now - 301, 1)),
            7,
            now,
            default
        ));
        assert!(!is_stale(
            Some(&tick(9, "claim", now - 5000, 1)),
            7,
            now,
            default
        ));
        assert!(!is_stale(None, 7, now, default));

        // A slow phase gets its own, much larger budget: 10 minutes inside
        // `gpu-wait` is normal, 10 minutes inside `claim` is a wedge.
        let gpu = stale_budget("gpu-wait", default);
        let infer = stale_budget("infer", default);
        assert!(gpu > default && infer > default);
        assert!(!is_stale(
            Some(&tick(7, "gpu-wait", now - 600, 1)),
            7,
            now,
            gpu
        ));
        assert!(is_stale(
            Some(&tick(7, "claim", now - 600, 1)),
            7,
            now,
            default
        ));
    }

    #[test]
    fn verdict_names_the_wedged_phase() {
        let default = Duration::from_secs(300);
        let now = 50_000u64;
        match verdict(
            Some(&tick(3, "reconcile", now - 1_000, 42)),
            3,
            now,
            default,
        ) {
            Verdict::Stalled {
                phase,
                silent_for,
                budget,
            } => {
                assert_eq!(phase, "reconcile");
                assert_eq!(silent_for, Duration::from_secs(1_000));
                assert_eq!(budget, default);
            }
            _ => panic!("expected a stall verdict"),
        }
        assert!(matches!(
            verdict(Some(&tick(4, "claim", now - 1_000, 1)), 3, now, default),
            Verdict::ForeignOwner
        ));
        assert!(matches!(
            verdict(Some(&tick(3, "claim", now - 1, 1)), 3, now, default),
            Verdict::Ok
        ));
    }

    #[test]
    fn the_warning_says_what_hung_and_what_happens_next() {
        let t = tick(11, "claim", 100, 5);
        let w = stall_warning(&t, 11, Duration::from_secs(400), Duration::from_secs(300));
        assert!(w.contains("\"claim\""), "{w}");
        assert!(w.contains("400s"), "{w}");
        assert!(w.contains("pid 11"), "{w}");
        assert!(w.contains(&STALL_EXIT_CODE.to_string()), "{w}");
        assert!(w.contains("systemd"), "{w}");
    }

    #[test]
    fn progress_marks_pass_completion_and_only_writes_on_change() {
        let dir = std::env::temp_dir().join(format!("gpn-wd-test-{}", std::process::id()));
        let path = dir.join("provider-tick.json");
        let mut p = Progress::new(path.clone(), Duration::from_secs(300));
        p.set_phase("claim");
        let first = read_tick(&path).expect("tick written");
        assert_eq!(first.loops, 0);
        assert_eq!(first.phase, "claim");
        p.finish_pass();
        let second = read_tick(&path).expect("tick rewritten");
        assert_eq!(second.loops, 1);
        assert_eq!(second.phase, "idle");
        assert_eq!(second.pid, std::process::id());
        // Settled in the past => stale.
        p.last_bump = now_unix().saturating_sub(10_000);
        assert!(p.is_stale(now_unix()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_operator_knob_is_floored_and_capped() {
        let keep = std::env::var("GPN_PROVIDER_WATCHDOG_SECS").ok();
        std::env::set_var("GPN_PROVIDER_WATCHDOG_SECS", "1");
        assert_eq!(stale_after_from_env(), Duration::from_secs(MIN_STALE_SECS));
        std::env::set_var("GPN_PROVIDER_WATCHDOG_SECS", "999999999");
        assert_eq!(stale_after_from_env(), Duration::from_secs(MAX_STALE_SECS));
        std::env::set_var("GPN_PROVIDER_WATCHDOG_SECS", "120");
        assert_eq!(stale_after_from_env(), Duration::from_secs(120));
        match keep {
            Some(v) => std::env::set_var("GPN_PROVIDER_WATCHDOG_SECS", v),
            None => std::env::remove_var("GPN_PROVIDER_WATCHDOG_SECS"),
        }
    }

    /// End-to-end proof of the restart mechanism: this same test binary is
    /// re-executed as a *child provider* that arms the watchdog and then wedges
    /// (sleeps forever without a progress bump). The child must log the WARN and
    /// exit with [`STALL_EXIT_CODE`] — which is what `Restart=always` acts on.
    /// Uses a 2 s budget so the proof costs seconds, not the 300 s default.
    #[test]
    fn a_wedged_child_process_is_warned_and_exits_for_a_restart() {
        if std::env::var("GPN_WD_CHILD").is_ok() {
            let path = std::env::temp_dir().join(format!(
                "gpn-wd-child-{}-{}.json",
                std::process::id(),
                now_unix()
            ));
            let p = std::sync::Arc::new(std::sync::Mutex::new(Progress::new(
                path,
                Duration::from_secs(2),
            )));
            p.lock().unwrap().set_phase("claim");
            spawn(p, Duration::from_secs(1));
            // The wedge: no progress from here on.
            std::thread::sleep(Duration::from_secs(120));
            std::process::exit(0); // must never be reached
        }

        let exe = std::env::current_exe().expect("test binary path");
        let mut child = std::process::Command::new(exe)
            .args([
                "provider_watchdog::tests::a_wedged_child_process_is_warned_and_exits_for_a_restart",
                "--exact",
                "--nocapture",
            ])
            .env("GPN_WD_CHILD", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the wedged child");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait().expect("poll the wedged child") {
                Some(status) => {
                    let mut err = String::new();
                    if let Some(mut s) = child.stderr.take() {
                        let _ = std::io::Read::read_to_string(&mut s, &mut err);
                    }
                    assert_eq!(
                        status.code(),
                        Some(STALL_EXIT_CODE),
                        "the watchdog must exit with the restart code, got {status:?}; stderr: {err}"
                    );
                    assert!(err.contains("[watchdog]"), "missing WARN line: {err}");
                    assert!(err.contains("claim"), "the WARN must name the phase: {err}");
                    return;
                }
                None => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the wedged child was never restarted (watchdog did not fire)"
                    );
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }
}
