//! Auto-idle — step aside when the operator's **other** GPU work shows up.
//!
//! `gpu_yield.rs` coordinates GPN's own providers with each other and with the
//! miner. It cannot see the rest of the machine: a game, a local model in
//! another app, a renderer. Before this module the only way to stop the node
//! competing with those was to click **Go idle** by hand — exactly the moment
//! the operator is least likely to be looking at a dashboard.
//!
//! ## What counts as "foreign load"
//!
//! `nvidia-smi` tells us three things: overall utilisation, VRAM in use, and
//! which PIDs hold GPU memory (both compute **and** graphics contexts). A
//! reading is *foreign* when **all** of these hold:
//!
//! 1. utilisation is at/above [`DEFAULT_UTIL_PCT`] (default 40 %), **and**
//! 2. GPN's own GPU lock is **not** held (no provider job of ours is running —
//!    a job we started is not something to step aside for), **and**
//! 3. the VRAM belongs to a **non-GPN** process: at least
//!    [`DEFAULT_MIN_FOREIGN_VRAM_MB`] (512 MiB) of GPU memory is held by PIDs
//!    that are not `gpn-*` / under `~/.gpn/`. Failing that, only when **no**
//!    GPN process holds GPU memory at all (nothing of ours can explain the
//!    load).
//!
//! Rule 3 is why the desktop itself never triggers it: Xorg + the compositor
//! sit at ~275 MiB permanently. The threshold is above that baseline and far
//! below a game (GBs) or a second model (hundreds of MiB to GBs).
//!
//! ## Conservative by construction
//!
//! - **Opt-in.** `GPN_IDLE_AUTO=1` (default **off**). The dashboard toggle
//!   persists the choice in `~/.gpn/idle-auto.json`.
//! - **Debounced.** `N` consecutive busy samples (`GPN_IDLE_AUTO_BUSY_SAMPLES`,
//!   default 3, one every 5 s) before anything happens, so a compositor blip or
//!   a shader compile does not park the node.
//! - **Short window.** Auto-idle enters for `GPN_IDLE_AUTO_MINUTES` (default
//!   30), clamped by the existing 6 h ceiling; it auto-resumes as soon as the
//!   load is gone for `M` quiet samples (default 3, one every 20 s) or at the
//!   deadline, whichever comes first.
//! - **Never fights the operator.** A manual idle (any reason not starting with
//!   `auto:`) is never auto-resumed, and a manual resume starts a
//!   `GPN_IDLE_AUTO_COOLDOWN_SECS` (default 600 s) window in which auto-idle
//!   will not re-engage.
//! - **Never the last block producer.** The same [`crate::idle::refusal_reason`]
//!   valve the manual path uses applies; a refusal is logged once per load
//!   episode and the node keeps serving.
//!
//! Every transition prints one line with the evidence, e.g.
//!
//! ```text
//! [idle-auto] idle: foreign GPU load 87% (2304 MiB non-GPN VRAM, 2 foreign
//!   PIDs, no GPN job, GPU lock free) — stepping aside for 30 min
//! [idle-auto] resume: foreign load gone for 3 samples (util 3%) — mining and
//!   job claiming resume
//! ```

use crate::idle::{self, IdleState};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Utilisation (%) at or above which the device counts as loaded.
pub const DEFAULT_UTIL_PCT: u32 = 40;
/// Consecutive busy samples needed before stepping aside.
pub const DEFAULT_BUSY_SAMPLES: u32 = 3;
/// Consecutive quiet samples needed before resuming.
pub const DEFAULT_QUIET_SAMPLES: u32 = 3;
/// Auto-idle window, in minutes (clamped to the 6 h ceiling by `idle`).
pub const DEFAULT_MINUTES: u64 = 30;
/// After a **manual** resume, auto-idle stays disarmed this long.
pub const DEFAULT_COOLDOWN_SECS: u64 = 600;
/// Spacing between counted busy samples (3 × 5 s ≈ 15 s of sustained load).
pub const DEFAULT_BUSY_SAMPLE_SECS: u64 = 5;
/// Spacing between counted quiet samples (3 × 20 s ≈ 60 s of quiet).
pub const DEFAULT_QUIET_SAMPLE_SECS: u64 = 20;
/// Non-GPN VRAM (MiB) that must be in use before load counts as foreign.
/// Above the Xorg+compositor baseline (~275 MiB), far below a game/model.
pub const DEFAULT_MIN_FOREIGN_VRAM_MB: u64 = 512;

/// Reason prefix that marks an idle window as ours. Anything else is manual.
pub const AUTO_REASON_PREFIX: &str = "auto:";

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str) -> Option<bool> {
    std::env::var(key).ok().map(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Everything tunable, resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoIdleConfig {
    pub util_percent: u32,
    pub busy_samples: u32,
    pub quiet_samples: u32,
    pub minutes: u64,
    pub cooldown_secs: u64,
    pub busy_sample_secs: u64,
    pub quiet_sample_secs: u64,
    pub min_foreign_vram_mb: u64,
}

impl Default for AutoIdleConfig {
    fn default() -> Self {
        Self {
            util_percent: DEFAULT_UTIL_PCT,
            busy_samples: DEFAULT_BUSY_SAMPLES,
            quiet_samples: DEFAULT_QUIET_SAMPLES,
            minutes: DEFAULT_MINUTES,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
            busy_sample_secs: DEFAULT_BUSY_SAMPLE_SECS,
            quiet_sample_secs: DEFAULT_QUIET_SAMPLE_SECS,
            min_foreign_vram_mb: DEFAULT_MIN_FOREIGN_VRAM_MB,
        }
    }
}

impl AutoIdleConfig {
    /// Read `GPN_IDLE_AUTO_*`, clamped so a typo cannot make this aggressive.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            util_percent: env_u64("GPN_IDLE_AUTO_UTIL_PCT", d.util_percent as u64).clamp(10, 100)
                as u32,
            busy_samples: env_u64("GPN_IDLE_AUTO_BUSY_SAMPLES", d.busy_samples as u64).clamp(1, 20)
                as u32,
            quiet_samples: env_u64("GPN_IDLE_AUTO_QUIET_SAMPLES", d.quiet_samples as u64)
                .clamp(1, 20) as u32,
            // Config, not an operator instruction: clamp to the 6 h ceiling.
            minutes: idle::clamp_minutes(env_u64("GPN_IDLE_AUTO_MINUTES", d.minutes)),
            cooldown_secs: env_u64("GPN_IDLE_AUTO_COOLDOWN_SECS", d.cooldown_secs),
            busy_sample_secs: env_u64("GPN_IDLE_AUTO_BUSY_SAMPLE_SECS", d.busy_sample_secs).max(1),
            quiet_sample_secs: env_u64("GPN_IDLE_AUTO_QUIET_SAMPLE_SECS", d.quiet_sample_secs)
                .max(1),
            min_foreign_vram_mb: env_u64(
                "GPN_IDLE_AUTO_MIN_FOREIGN_VRAM_MB",
                d.min_foreign_vram_mb,
            ),
        }
    }
}

// ── readings ───────────────────────────────────────────────────────────────

/// One GPU process from the `nvidia-smi` table (`G` or `C`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuProcess {
    pub pid: u32,
    /// `'G'` graphics or `'C'` compute.
    pub kind: char,
    pub used_mb: u64,
}

/// One probe result: device numbers plus who is holding GPU memory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReading {
    pub util_percent: Option<u32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    /// GPN's own GPU lock is held → one of our providers is mid-job.
    pub lock_held: bool,
    /// GPU memory held by GPN processes (miner, providers, our llama-server).
    pub gpn_vram_mb: u64,
    /// GPU memory held by everything else (desktop, games, other models).
    pub foreign_vram_mb: u64,
    pub foreign_pids: usize,
}

impl LoadReading {
    /// Would this reading make us step aside?
    pub fn is_foreign(&self, cfg: &AutoIdleConfig) -> bool {
        // A job of ours is running: that load is expected, never "foreign".
        if self.lock_held {
            return false;
        }
        let Some(util) = self.util_percent else {
            // No probe (AMD/iGPU/no device access): never guess.
            return false;
        };
        if util < cfg.util_percent {
            return false;
        }
        if self.foreign_vram_mb >= cfg.min_foreign_vram_mb {
            return true;
        }
        // Nothing of ours is on the GPU at all, so nothing of ours can explain
        // a loaded device.
        self.gpn_vram_mb == 0
    }

    /// `util 87%, VRAM 3006/8188 MiB, 2304 MiB non-GPN (2 foreign PIDs), GPU lock free`
    pub fn evidence(&self) -> String {
        let util = match self.util_percent {
            Some(u) => format!("util {u}%"),
            None => "util unknown".to_string(),
        };
        let vram = match (self.vram_used_mb, self.vram_total_mb) {
            (Some(u), Some(t)) => format!("VRAM {u}/{t} MiB"),
            _ => "VRAM unknown".to_string(),
        };
        format!(
            "{util}, {vram}, {} MiB non-GPN ({} foreign PIDs), {}",
            self.foreign_vram_mb,
            self.foreign_pids,
            if self.lock_held {
                "GPN job running"
            } else {
                "GPU lock free"
            }
        )
    }
}

/// The OS-facing half, injectable so the state machine is testable (and
/// runnable) with no GPU.
pub trait GpuLoadProbe: Send {
    fn read(&mut self) -> Option<LoadReading>;
}

/// The production probe: `nvidia-smi` for the device **and** the process table.
pub struct NvidiaProbe {
    device: usize,
}

impl NvidiaProbe {
    pub fn new(device: usize) -> Self {
        Self { device }
    }
}

impl GpuLoadProbe for NvidiaProbe {
    fn read(&mut self) -> Option<LoadReading> {
        let (util, used, total) = crate::attest_idle::nvidia_smi_load(self.device)?;
        let procs = nvidia_smi_processes().unwrap_or_default();
        let (gpn, foreign, foreign_pids) = attribute_processes(&procs);
        Some(LoadReading {
            util_percent: Some(util),
            vram_used_mb: Some(used),
            vram_total_mb: Some(total),
            lock_held: crate::gpu_yield::lock_is_held(),
            gpn_vram_mb: gpn,
            foreign_vram_mb: foreign,
            foreign_pids,
        })
    }
}

/// Full `nvidia-smi` process table (graphics **and** compute).
pub fn nvidia_smi_processes() -> Option<Vec<GpuProcess>> {
    let out = std::process::Command::new("nvidia-smi").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_nvidia_smi_processes(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// Parse the human `Processes:` table. Pure, so the format is testable.
///
/// ```text
/// |    0   N/A  N/A            1191      G   /usr/lib/xorg/Xorg                      225MiB |
/// |    0   N/A  N/A         3006178      C   ...llama/lib/ollama/llama-server       1558MiB |
/// ```
pub fn parse_nvidia_smi_processes(s: &str) -> Vec<GpuProcess> {
    let mut out = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let body = line.trim_matches('|').trim();
        let toks: Vec<&str> = body.split_whitespace().collect();
        // GPU GI CI PID TYPE NAME… MEMORY
        if toks.len() < 7 {
            continue;
        }
        let Ok(pid) = toks[3].parse::<u32>() else {
            continue;
        };
        let kind = toks[4].chars().next().unwrap_or(' ');
        if kind != 'G' && kind != 'C' {
            continue;
        }
        let Some(mem) = toks.last().and_then(|t| t.strip_suffix("MiB")) else {
            continue;
        };
        let Ok(used_mb) = mem.trim().replace(',', "").parse::<u64>() else {
            continue;
        };
        out.push(GpuProcess { pid, kind, used_mb });
    }
    out
}

/// Is `pid` one of ours? `gpn-*` processes and anything under `~/.gpn/`
/// (the bundled ollama/llama-server counts: it serves *our* jobs).
pub fn is_gpn_pid(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        let comm = comm.trim();
        if comm.starts_with("gpn") {
            return true;
        }
    }
    if let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        let cmd = String::from_utf8_lossy(&cmd).to_lowercase();
        if cmd.contains("gpn_") || cmd.contains("/gpn-") || cmd.contains("/.gpn/") {
            return true;
        }
    }
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        let exe = exe.to_string_lossy().to_lowercase();
        if exe.contains("/.gpn/") || exe.contains("/gpn-") {
            return true;
        }
    }
    false
}

/// Split a process table into `(GPN MiB, foreign MiB, foreign PID count)`.
pub fn attribute_processes(procs: &[GpuProcess]) -> (u64, u64, usize) {
    let mut gpn = 0u64;
    let mut foreign = 0u64;
    let mut n = 0usize;
    for p in procs {
        if is_gpn_pid(p.pid) {
            gpn += p.used_mb;
        } else {
            foreign += p.used_mb;
            n += 1;
        }
    }
    (gpn, foreign, n)
}

// ── decisions ──────────────────────────────────────────────────────────────

/// What the supervisor should do after one sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Enter idle for `minutes`, with this reason (always [`AUTO_REASON_PREFIX`]).
    Enter {
        minutes: u64,
        reason: String,
        evidence: String,
    },
    /// Leave idle, with a human-readable why.
    Exit {
        why: String,
    },
}

/// The debounced state machine. Pure: no clock, no files, no GPU.
#[derive(Debug, Clone)]
pub struct AutoIdle {
    cfg: AutoIdleConfig,
    busy: u32,
    quiet: u32,
    last_busy_at: u64,
    last_quiet_at: u64,
    /// We believe an auto-idle window we created is in force.
    engaged: bool,
    /// Deadline of that window (detects "expired" vs "manually resumed").
    engaged_until: u64,
    cooldown_until: u64,
    /// One refusal line per load episode, not per sample.
    refusal_logged: bool,
}

impl AutoIdle {
    pub fn new(cfg: AutoIdleConfig) -> Self {
        Self {
            cfg,
            busy: 0,
            quiet: 0,
            last_busy_at: 0,
            last_quiet_at: 0,
            engaged: false,
            engaged_until: 0,
            cooldown_until: 0,
            refusal_logged: false,
        }
    }

    pub fn config(&self) -> &AutoIdleConfig {
        &self.cfg
    }

    /// Adopt an auto-idle window that is already on disk (process restart).
    pub fn adopt(&mut self, st: &IdleState) {
        self.engaged = true;
        self.engaged_until = st.until;
    }

    pub fn engaged(&self) -> bool {
        self.engaged
    }

    pub fn cooldown_until(&self) -> u64 {
        self.cooldown_until
    }

    /// One decision point.
    ///
    /// * `reading` — `None` when the probe failed (treated as quiet: we never
    ///   act on an unmeasurable device).
    /// * `idle_state` — what `idle.json` says right now (`None` = active).
    /// * `peers` — from the node's `system_health`, for the safety valve.
    pub fn step(
        &mut self,
        reading: Option<&LoadReading>,
        now: u64,
        idle_state: Option<&IdleState>,
        peers: Option<u64>,
        force: bool,
    ) -> Action {
        // ── 1. Reconcile what we think with what is on disk. ───────────────
        if self.engaged {
            match idle_state {
                Some(st) if st.reason.starts_with(AUTO_REASON_PREFIX) => {
                    // Still ours and still in force.
                    self.engaged_until = st.until;
                }
                Some(st) => {
                    // The operator replaced our window with their own idle.
                    // Theirs wins; never auto-resume theirs.
                    println!(
                        "[idle-auto] standing down: a manual idle ({:?}) replaced the auto window; \
                         it will be resumed by the operator or its own deadline",
                        st.reason
                    );
                    self.engaged = false;
                    self.engaged_until = 0;
                    self.busy = 0;
                    self.quiet = 0;
                    return Action::None;
                }
                None => {
                    self.engaged = false;
                    let until = self.engaged_until;
                    self.engaged_until = 0;
                    self.busy = 0;
                    self.quiet = 0;
                    if now >= until {
                        // idle.json pruned itself at the deadline.
                        return Action::Exit {
                            why: "auto-idle window expired".to_string(),
                        };
                    }
                    // Cleared before our deadline: a manual resume.
                    self.cooldown_until = now.saturating_add(self.cfg.cooldown_secs);
                    println!(
                        "[idle-auto] manual resume seen — auto-idle disarmed for {}s \
                         (the operator is in charge)",
                        self.cfg.cooldown_secs
                    );
                    return Action::Exit {
                        why: "manual resume".to_string(),
                    };
                }
            }
        } else if let Some(st) = idle_state {
            if st.reason.starts_with(AUTO_REASON_PREFIX) {
                // Restarted while an auto window was in force: adopt it.
                self.engaged = true;
                self.engaged_until = st.until;
            } else {
                // A manual idle is in force — nothing to do, ever.
                self.busy = 0;
                self.quiet = 0;
                return Action::None;
            }
        }

        // ── 2. If we own the idle window, decide whether to end it. ────────
        if self.engaged {
            let foreign = reading.map(|r| r.is_foreign(&self.cfg)).unwrap_or(false);
            if foreign {
                // Load came back while idling: stay idle (the window bounds it).
                self.quiet = 0;
                self.last_quiet_at = 0;
                return Action::None;
            }
            if now >= self.engaged_until {
                self.engaged = false;
                self.engaged_until = 0;
                return Action::Exit {
                    why: "auto-idle window expired".to_string(),
                };
            }
            self.count_quiet(now);
            if self.quiet >= self.cfg.quiet_samples {
                self.engaged = false;
                self.engaged_until = 0;
                let why = format!(
                    "foreign load gone for {} samples ({})",
                    self.quiet,
                    reading
                        .map(|r| r.evidence())
                        .unwrap_or_else(|| "GPU unreadable".to_string())
                );
                self.quiet = 0;
                self.last_quiet_at = 0;
                return Action::Exit { why };
            }
            return Action::None;
        }

        // ── 3. Not idle: look for sustained foreign load. ──────────────────
        let Some(r) = reading else {
            self.count_quiet(now);
            return Action::None;
        };
        if !r.is_foreign(&self.cfg) {
            self.count_quiet(now);
            self.refusal_logged = false;
            return Action::None;
        }

        self.quiet = 0;
        self.last_quiet_at = 0;
        if self.busy == 0 || now >= self.last_busy_at.saturating_add(self.cfg.busy_sample_secs) {
            self.busy += 1;
            self.last_busy_at = now;
        }
        if self.busy < self.cfg.busy_samples {
            return Action::None;
        }
        if now < self.cooldown_until {
            return Action::None;
        }

        // Safety valve: never idle the only block producer. Refuse, keep the
        // node serving, and say so once per episode.
        if let Some(why) = idle::refusal_reason(peers, force) {
            if !self.refusal_logged {
                self.refusal_logged = true;
                eprintln!(
                    "[idle-auto] NOT stepping aside despite {}: {why}",
                    r.evidence()
                );
            }
            return Action::None;
        }

        self.busy = 0;
        self.last_busy_at = 0;
        self.engaged = true;
        self.engaged_until = now.saturating_add(self.cfg.minutes * 60);
        Action::Enter {
            minutes: self.cfg.minutes,
            reason: format!(
                "{} foreign GPU load {}%",
                AUTO_REASON_PREFIX,
                r.util_percent.unwrap_or(0)
            ),
            evidence: r.evidence(),
        }
    }

    /// The idle write failed: undo the optimistic bookkeeping so the next
    /// sample can try again instead of believing we are idling.
    pub fn abort_enter(&mut self, now: u64) {
        self.engaged = false;
        self.engaged_until = 0;
        self.busy = 0;
        self.last_busy_at = 0;
        self.cooldown_until = now.saturating_add(60);
    }

    fn count_quiet(&mut self, now: u64) {
        if self.quiet == 0
            || now
                >= self
                    .last_quiet_at
                    .saturating_add(self.cfg.quiet_sample_secs)
        {
            self.quiet += 1;
            self.last_quiet_at = now;
        }
    }
}

// ── shared runtime state (dashboard reads this) ────────────────────────────

/// Toggle + last reading, shared between the sampler thread and the dashboard.
#[derive(Debug)]
pub struct AutoIdleShared {
    enabled: AtomicBool,
    engaged: AtomicBool,
    foreign: AtomicBool,
    /// −1 = unknown.
    util: AtomicI64,
    foreign_vram_mb: AtomicU64,
    cooldown_until: AtomicU64,
    /// Read by the dashboard (threshold + window are shown in the UI).
    pub cfg: AutoIdleConfig,
    state_file: PathBuf,
}

impl AutoIdleShared {
    pub fn new(cfg: AutoIdleConfig, enabled: bool, state_file: PathBuf) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            engaged: AtomicBool::new(false),
            foreign: AtomicBool::new(false),
            util: AtomicI64::new(-1),
            foreign_vram_mb: AtomicU64::new(0),
            cooldown_until: AtomicU64::new(0),
            cfg,
            state_file,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Flip the toggle and persist it (best effort — a read-only home dir must
    /// not make the dashboard lie about the in-memory value).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
        let body = json!({"enabled": on}).to_string();
        if let Some(parent) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.state_file, body) {
            eprintln!(
                "[idle-auto] could not persist {}: {e}",
                self.state_file.display()
            );
        }
    }

    pub fn publish(&self, reading: Option<&LoadReading>, engaged: bool, cooldown_until: u64) {
        self.engaged.store(engaged, Ordering::SeqCst);
        self.cooldown_until.store(cooldown_until, Ordering::SeqCst);
        match reading {
            Some(r) => {
                self.util.store(
                    r.util_percent.map(|u| u as i64).unwrap_or(-1),
                    Ordering::SeqCst,
                );
                self.foreign_vram_mb
                    .store(r.foreign_vram_mb, Ordering::SeqCst);
                self.foreign
                    .store(r.is_foreign(&self.cfg), Ordering::SeqCst);
            }
            None => {
                self.util.store(-1, Ordering::SeqCst);
                self.foreign_vram_mb.store(0, Ordering::SeqCst);
                self.foreign.store(false, Ordering::SeqCst);
            }
        }
    }

    /// The `auto_idle` block of `/api/status`.
    pub fn status_json(&self, now: u64) -> Value {
        let util = self.util.load(Ordering::SeqCst);
        let cd = self.cooldown_until.load(Ordering::SeqCst);
        json!({
            "enabled": self.enabled(),
            "engaged": self.engaged.load(Ordering::SeqCst),
            "foreign": self.foreign.load(Ordering::SeqCst),
            "util_percent": if util < 0 { Value::Null } else { json!(util) },
            "foreign_vram_mb": self.foreign_vram_mb.load(Ordering::SeqCst),
            "threshold_pct": self.cfg.util_percent,
            "min_foreign_vram_mb": self.cfg.min_foreign_vram_mb,
            "minutes": self.cfg.minutes,
            "cooldown_secs": cd.saturating_sub(now),
        })
    }
}

/// `$GPN_IDLE_AUTO_FILE`, else `~/.gpn/idle-auto.json`.
pub fn toggle_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_IDLE_AUTO_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".gpn").join("idle-auto.json")
}

/// Persisted dashboard toggle, if any.
pub fn read_toggle(path: &Path) -> Option<bool> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("enabled")?.as_bool()
}

/// Startup value: `GPN_IDLE_AUTO=1` **or** a persisted dashboard toggle.
pub fn initial_enabled() -> bool {
    env_bool("GPN_IDLE_AUTO").unwrap_or(false) || read_toggle(&toggle_path()).unwrap_or(false)
}

// ── the sampler thread ─────────────────────────────────────────────────────

/// Everything the sampler needs from the supervisor.
pub struct RunOptions {
    pub idle_path: PathBuf,
    pub force: bool,
    pub peers: Arc<dyn Fn() -> Option<u64> + Send + Sync>,
    pub probe: Box<dyn GpuLoadProbe>,
    /// Wall-clock spacing of probe reads (the state machine debounces on top).
    pub tick_secs: u64,
    pub stop: Arc<AtomicBool>,
    pub shared: Arc<AutoIdleShared>,
}

/// Spawn the sampler. It idles when disabled (a 1 s flag check, no probe).
pub fn spawn(mut opts: RunOptions) -> std::thread::JoinHandle<()> {
    let mut machine = AutoIdle::new(opts.shared.cfg.clone());
    let handle = std::thread::Builder::new()
        .name("gpn-idle-auto".into())
        .spawn(move || {
            let mut last_sample = 0u64;
            while !opts.stop.load(Ordering::SeqCst) {
                if !opts.shared.enabled() {
                    // Disabled: cheap wakeups so a dashboard toggle is instant.
                    machine = AutoIdle::new(opts.shared.cfg.clone());
                    opts.shared.publish(None, false, 0);
                    sleep_step(&opts.stop, 1);
                    last_sample = 0;
                    continue;
                }
                let now = now_unix();
                if now >= last_sample.saturating_add(opts.tick_secs.max(1)) {
                    last_sample = now;
                    let reading = opts.probe.read();
                    let idle_state = idle::state_at(&opts.idle_path, now);
                    let peers = (opts.peers)();
                    // Adopt a window that outlived the previous process.
                    if let Some(st) = idle_state.as_ref() {
                        if st.reason.starts_with(AUTO_REASON_PREFIX) && !machine.engaged() {
                            machine.adopt(st);
                        }
                    }
                    let action = machine.step(
                        reading.as_ref(),
                        now,
                        idle_state.as_ref(),
                        peers,
                        opts.force,
                    );
                    match action {
                        Action::None => {}
                        Action::Enter {
                            minutes,
                            reason,
                            evidence,
                        } => match idle::start_at(&opts.idle_path, minutes, &reason, now) {
                            Ok(st) => {
                                println!(
                                    "[idle-auto] idle: foreign GPU load ({}% util): {evidence} \
                                         — stepping aside for {minutes} min (until {})",
                                    reading.as_ref().and_then(|r| r.util_percent).unwrap_or(0),
                                    st.until
                                );
                            }
                            Err(e) => {
                                machine.abort_enter(now);
                                eprintln!("[idle-auto] cannot enter idle: {e}");
                            }
                        },
                        Action::Exit { why } => {
                            idle::clear_at(&opts.idle_path);
                            println!("[idle-auto] resume: {why} — mining and job claiming resume");
                        }
                    }
                    opts.shared.publish(
                        reading.as_ref(),
                        machine.engaged(),
                        machine.cooldown_until(),
                    );
                } else {
                    sleep_step(&opts.stop, 1);
                    continue;
                }
                sleep_step(&opts.stop, 1);
            }
        })
        .expect("spawn gpn-idle-auto");
    handle
}

fn sleep_step(stop: &Arc<AtomicBool>, secs: u64) {
    let mut left = Duration::from_secs(secs);
    while left > Duration::ZERO && !stop.load(Ordering::SeqCst) {
        let s = Duration::from_millis(100).min(left);
        std::thread::sleep(s);
        left = left.saturating_sub(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Env vars are process-global; the tests that set them must not overlap.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn cfg() -> AutoIdleConfig {
        AutoIdleConfig {
            util_percent: 40,
            busy_samples: 3,
            quiet_samples: 3,
            minutes: 30,
            cooldown_secs: 600,
            busy_sample_secs: 5,
            quiet_sample_secs: 20,
            min_foreign_vram_mb: 512,
        }
    }

    fn busy() -> LoadReading {
        LoadReading {
            util_percent: Some(87),
            vram_used_mb: Some(6000),
            vram_total_mb: Some(8188),
            lock_held: false,
            gpn_vram_mb: 1120,
            foreign_vram_mb: 2304,
            foreign_pids: 2,
        }
    }

    fn quiet() -> LoadReading {
        LoadReading {
            util_percent: Some(3),
            vram_used_mb: Some(1200),
            vram_total_mb: Some(8188),
            lock_held: false,
            gpn_vram_mb: 1120,
            foreign_vram_mb: 80,
            foreign_pids: 1,
        }
    }

    fn idle_state(reason: &str, until: u64) -> IdleState {
        IdleState {
            until,
            reason: reason.to_string(),
            started: 0,
        }
    }

    #[test]
    fn a_job_of_ours_is_never_foreign() {
        let mut r = busy();
        r.lock_held = true;
        assert!(!r.is_foreign(&cfg()));
    }

    #[test]
    fn low_utilisation_is_never_foreign() {
        let mut r = busy();
        r.util_percent = Some(39);
        assert!(!r.is_foreign(&cfg()));
        r.util_percent = Some(40);
        assert!(r.is_foreign(&cfg()), "the threshold is inclusive");
    }

    #[test]
    fn desktop_baseline_is_below_the_foreign_vram_threshold() {
        // Xorg + compositor ≈ 275 MiB, and the miner explains the rest.
        let r = LoadReading {
            util_percent: Some(99),
            vram_used_mb: Some(1600),
            vram_total_mb: Some(8188),
            lock_held: false,
            gpn_vram_mb: 1120,
            foreign_vram_mb: 275,
            foreign_pids: 2,
        };
        assert!(!r.is_foreign(&cfg()), "our own miner must not trigger idle");
    }

    #[test]
    fn foreign_vram_above_the_threshold_triggers() {
        let r = LoadReading {
            foreign_vram_mb: 512,
            ..busy()
        };
        assert!(r.is_foreign(&cfg()));
    }

    #[test]
    fn unknown_utilisation_never_triggers() {
        let r = LoadReading {
            util_percent: None,
            ..busy()
        };
        assert!(!r.is_foreign(&cfg()));
    }

    #[test]
    fn unreadable_probe_is_quiet_not_busy() {
        let mut m = AutoIdle::new(cfg());
        for i in 0..10 {
            assert_eq!(m.step(None, i * 5, None, Some(2), false), Action::None);
        }
        assert!(!m.engaged());
    }

    #[test]
    fn single_spike_does_not_idle() {
        let mut m = AutoIdle::new(cfg());
        assert_eq!(m.step(Some(&busy()), 0, None, Some(2), false), Action::None);
        // Back to quiet well before the next counted sample.
        assert_eq!(
            m.step(Some(&quiet()), 3, None, Some(2), false),
            Action::None
        );
        assert!(!m.engaged(), "one spike must never park the node");
    }

    #[test]
    fn sustained_load_idles_after_n_samples() {
        let mut m = AutoIdle::new(cfg());
        assert_eq!(m.step(Some(&busy()), 0, None, Some(2), false), Action::None);
        assert_eq!(m.step(Some(&busy()), 5, None, Some(2), false), Action::None);
        let a = m.step(Some(&busy()), 10, None, Some(2), false);
        match a {
            Action::Enter {
                minutes,
                reason,
                evidence,
            } => {
                assert_eq!(minutes, 30);
                assert_eq!(reason, "auto: foreign GPU load 87%");
                assert!(evidence.contains("2304 MiB"), "{evidence}");
            }
            other => panic!("expected Enter, got {other:?}"),
        }
        assert!(m.engaged());
    }

    #[test]
    fn samples_inside_the_spacing_window_do_not_count() {
        let mut m = AutoIdle::new(cfg());
        for _ in 0..5 {
            assert_eq!(m.step(Some(&busy()), 0, None, Some(2), false), Action::None);
        }
        assert!(!m.engaged(), "5 samples at the same instant is one sample");
    }

    #[test]
    fn auto_idle_resumes_after_m_quiet_samples() {
        let mut m = AutoIdle::new(cfg());
        m.step(Some(&busy()), 0, None, Some(2), false);
        m.step(Some(&busy()), 5, None, Some(2), false);
        m.step(Some(&busy()), 10, None, Some(2), false);
        assert!(m.engaged());

        let st = idle_state("auto: foreign GPU load 87%", 1_810);
        assert_eq!(
            m.step(Some(&quiet()), 15, Some(&st), Some(2), false),
            Action::None
        );
        assert_eq!(
            m.step(Some(&quiet()), 35, Some(&st), Some(2), false),
            Action::None
        );
        match m.step(Some(&quiet()), 55, Some(&st), Some(2), false) {
            Action::Exit { why } => assert!(why.contains("gone for 3 samples"), "{why}"),
            other => panic!("expected Exit, got {other:?}"),
        }
        assert!(!m.engaged());
        assert_eq!(m.cooldown_until(), 0, "auto-resume needs no cooldown");
    }

    #[test]
    fn deadline_expiry_resumes_without_a_cooldown() {
        let mut m = AutoIdle::new(cfg());
        m.adopt(&idle_state("auto: foreign GPU load 87%", 1_000));
        // idle.json has pruned itself at the deadline.
        match m.step(Some(&quiet()), 1_000, None, Some(2), false) {
            Action::Exit { why } => assert!(why.contains("expired"), "{why}"),
            other => panic!("expected Exit, got {other:?}"),
        }
        assert!(!m.engaged());
        assert_eq!(m.cooldown_until(), 0);
    }

    #[test]
    fn manual_resume_starts_a_cooldown() {
        let mut m = AutoIdle::new(cfg());
        m.adopt(&idle_state("auto: foreign GPU load 87%", 5_000));
        match m.step(Some(&busy()), 500, None, Some(2), false) {
            Action::Exit { why } => assert_eq!(why, "manual resume"),
            other => panic!("expected Exit, got {other:?}"),
        }
        assert_eq!(m.cooldown_until(), 1_100);

        // Foreign load, fully debounced, inside the cooldown: still no idle.
        for t in [600, 605, 610, 615, 620] {
            assert_eq!(m.step(Some(&busy()), t, None, Some(2), false), Action::None);
        }
        assert!(!m.engaged(), "auto-idle must not fight a manual resume");
        // After the cooldown it may engage again.
        let mut last = Action::None;
        for t in [1_100, 1_105, 1_110] {
            last = m.step(Some(&busy()), t, None, Some(2), false);
            if last != Action::None {
                break;
            }
        }
        assert!(matches!(last, Action::Enter { .. }), "{last:?}");
    }

    #[test]
    fn manual_idle_is_never_auto_resumed() {
        let mut m = AutoIdle::new(cfg());
        let st = idle_state("gaming", 9_999);
        for t in [0, 100, 200] {
            assert_eq!(
                m.step(Some(&quiet()), t, Some(&st), Some(2), false),
                Action::None
            );
        }
        assert!(!m.engaged());
    }

    #[test]
    fn a_manual_idle_replacing_the_auto_window_stands_us_down() {
        let mut m = AutoIdle::new(cfg());
        m.adopt(&idle_state("auto: foreign GPU load 87%", 5_000));
        let st = idle_state("dashboard", 9_000);
        assert_eq!(
            m.step(Some(&quiet()), 100, Some(&st), Some(2), false),
            Action::None
        );
        assert!(!m.engaged());
        // And when their window ends, we do not auto-resume it.
        assert_eq!(
            m.step(Some(&quiet()), 9_000, None, Some(2), false),
            Action::None
        );
        assert_eq!(m.cooldown_until(), 0);
    }

    #[test]
    fn a_lone_authority_is_never_auto_idled() {
        let mut m = AutoIdle::new(cfg());
        for t in [0, 5, 10, 15, 20] {
            assert_eq!(m.step(Some(&busy()), t, None, Some(0), false), Action::None);
            assert_eq!(m.step(Some(&busy()), t, None, None, false), Action::None);
        }
        assert!(!m.engaged(), "the safety valve must hold for auto-idle too");

        // Forced (single-box dev chain) → allowed.
        let mut f = AutoIdle::new(cfg());
        let mut last = Action::None;
        for t in [0, 5, 10] {
            last = f.step(Some(&busy()), t, None, Some(0), true);
        }
        assert!(matches!(last, Action::Enter { .. }), "{last:?}");
    }

    #[test]
    fn entering_idle_uses_the_clamped_window() {
        let mut c = cfg();
        c.minutes = 30;
        let mut m = AutoIdle::new(c);
        let mut last = Action::None;
        for t in [0, 5, 10] {
            last = m.step(Some(&busy()), t, None, Some(1), false);
        }
        match last {
            Action::Enter { minutes, .. } => assert_eq!(minutes, 30),
            other => panic!("{other:?}"),
        }
        assert_eq!(m.engaged_until, 10 + 30 * 60);
    }

    #[test]
    fn restart_adopts_a_live_auto_window() {
        let mut m = AutoIdle::new(cfg());
        let st = idle_state("auto: foreign GPU load 87%", 5_000);
        assert_eq!(
            m.step(Some(&quiet()), 100, Some(&st), Some(2), false),
            Action::None
        );
        assert!(m.engaged(), "a restart must keep the window it wrote");
        // ...and still auto-resumes after M quiet samples.
        let mut last = Action::None;
        for t in [120, 140, 160] {
            last = m.step(Some(&quiet()), t, Some(&st), Some(2), false);
            if last != Action::None {
                break;
            }
        }
        assert!(matches!(last, Action::Exit { .. }), "{last:?}");
    }

    #[test]
    fn load_returning_while_idle_keeps_the_window() {
        let mut m = AutoIdle::new(cfg());
        let st = idle_state("auto: foreign GPU load 87%", 5_000);
        m.adopt(&st);
        for t in [100, 200, 300] {
            assert_eq!(
                m.step(Some(&busy()), t, Some(&st), Some(2), false),
                Action::None
            );
        }
        assert!(m.engaged());
    }

    #[test]
    fn config_defaults_and_clamping() {
        let _g = env_lock();
        let d = AutoIdleConfig::default();
        assert_eq!(d.util_percent, 40);
        assert_eq!(d.busy_samples, 3);
        assert_eq!(d.quiet_samples, 3);
        assert_eq!(d.minutes, 30);
        assert_eq!(d.cooldown_secs, 600);
        assert_eq!(d.min_foreign_vram_mb, 512);
        // Env values are clamped, even absurd ones.
        std::env::set_var("GPN_IDLE_AUTO_MINUTES", "100000");
        std::env::set_var("GPN_IDLE_AUTO_UTIL_PCT", "1");
        std::env::set_var("GPN_IDLE_AUTO_BUSY_SAMPLES", "999");
        let c = AutoIdleConfig::from_env();
        assert_eq!(c.minutes, idle::MAX_IDLE_MINUTES);
        assert_eq!(c.util_percent, 10);
        assert_eq!(c.busy_samples, 20);
        std::env::remove_var("GPN_IDLE_AUTO_MINUTES");
        std::env::remove_var("GPN_IDLE_AUTO_UTIL_PCT");
        std::env::remove_var("GPN_IDLE_AUTO_BUSY_SAMPLES");
    }

    #[test]
    fn auto_idle_is_off_by_default() {
        let _g = env_lock();
        // No env, no persisted toggle → disabled.
        std::env::remove_var("GPN_IDLE_AUTO");
        std::env::set_var("GPN_IDLE_AUTO_FILE", "/nonexistent/gpn/idle-auto.json");
        assert!(!initial_enabled());
        std::env::set_var("GPN_IDLE_AUTO", "1");
        assert!(initial_enabled());
        std::env::set_var("GPN_IDLE_AUTO", "0");
        assert!(!initial_enabled());
        std::env::remove_var("GPN_IDLE_AUTO");
        std::env::remove_var("GPN_IDLE_AUTO_FILE");
    }

    #[test]
    fn persisted_toggle_is_respected() {
        let _g = env_lock();
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("gpn-idle-auto-toggle-{n}.json"));
        std::env::remove_var("GPN_IDLE_AUTO");
        std::env::set_var("GPN_IDLE_AUTO_FILE", &p);
        assert!(!initial_enabled());
        let shared = AutoIdleShared::new(cfg(), false, p.clone());
        shared.set_enabled(true);
        assert!(shared.enabled());
        assert_eq!(read_toggle(&p), Some(true));
        assert!(initial_enabled(), "a restart keeps the dashboard toggle");
        std::env::remove_var("GPN_IDLE_AUTO_FILE");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn process_table_parses_graphics_and_compute() {
        let table = "\
+-----------------------------------------------------------------------------------------+
| Processes:                                                                              |
|  GPU   GI   CI              PID   Type   Process name                        GPU Memory |
|=========================================================================================|
|    0   N/A  N/A            1191      G   /usr/lib/xorg/Xorg                      225MiB |
|    0   N/A  N/A         2856937      C   ...e/jeremyg/.local/bin/gpn-node       1120MiB |
|    0   N/A  N/A         3006178      C   ...llama/lib/ollama/llama-server       1,558MiB |
+-----------------------------------------------------------------------------------------+";
        let p = parse_nvidia_smi_processes(table);
        assert_eq!(p.len(), 3);
        assert_eq!(
            p[0],
            GpuProcess {
                pid: 1191,
                kind: 'G',
                used_mb: 225
            }
        );
        assert_eq!(p[1].used_mb, 1120);
        assert_eq!(p[2].used_mb, 1558, "thousands separators are stripped");
    }

    #[test]
    fn evidence_line_names_the_numbers() {
        let e = busy().evidence();
        assert!(e.contains("util 87%"), "{e}");
        assert!(e.contains("2304 MiB"), "{e}");
        assert!(e.contains("GPU lock free"), "{e}");
    }
}
