//! Queue item #93 — **only attest a GFLOPS number that was measured on an idle
//! GPU**, and only from a repeatable sample set.
//!
//! ## The failure this exists to stop
//!
//! box1, 2026-10-23. An RTX 5070 Ti (16 GB) carries `compute_gflops: 188` on
//! chain, from an operator run that happened while the box's *other* AI
//! workload held the card. Every re-attest attempt since has been measured under
//! the same contention:
//!
//! ```text
//! [attest] re-attesting: 188 GFLOPS attested at block 67144, older than 7200 blocks
//! [attest] measured 26/44/45 GFLOPS vs 188 on chain (floor 3000) — keeping the existing record
//! ```
//!
//! Three readings of 26, 44 and 45 on one card is not a capability figure — it
//! is the shape of a measurement fighting another process for SMs. Because it
//! lands below `MinEligibilityGflops` (3 000) the value is never replaced, so the
//! node advertises a number that is both wrong and stale, forever, and routing
//! (`min_gflops`) gates on it.
//!
//! ## The rules
//!
//! 1. **Measure only when the GPU is idle.** Take the cross-process GPU lock
//!    (`gpu_yield`, the same one every provider takes for a job) without
//!    waiting; if someone holds it, the GPU is busy — skip, retry later. Once
//!    held, the miner's yield flag is up, so give it a moment to actually stand
//!    down (`settle`) and *then* read utilisation / VRAM. A reading that is
//!    still busy after that is someone else's workload (CUDA job, desktop
//!    compositor, another miner) and is skipped, never submitted.
//! 2. **Repeat and take the best.** `runs` (default 3) samples; a spread above
//!    [`SPREAD_THRESHOLD_PCT`] means contention or thermal throttling, and the
//!    highest sample is the least contaminated one.
//! 3. **Never let a bad value persist silently.** If the on-chain record and the
//!    idle measurement disagree by more than [`RECORD_TOLERANCE_PCT`], say so
//!    loudly, naming both numbers.
//! 4. **Report the conditions.** [`AttestState`] is persisted next to the lock
//!    (`~/.gpn/attest-state.json`) so `/status` can publish
//!    `gflops_source: "attested-idle"` with the samples and the spread instead of
//!    a bare number an operator cannot interpret.
//!
//! Everything decision-shaped here is pure and unit-tested; the OS probes live
//! behind [`AttestProbe`], so the busy path is tested by injection, never by
//! needing a real contended GPU.

use std::path::PathBuf;
use std::time::Duration;

/// Default number of timed GEMM runs per attestation measurement.
pub const DEFAULT_RUNS: usize = 3;
/// Two samples further apart than this (relative to their median) are treated
/// as "something moved during the measurement".
pub const SPREAD_THRESHOLD_PCT: f64 = 20.0;
/// How far the on-chain record may sit from an idle measurement before it is
/// called out as stale/wrong.
pub const RECORD_TOLERANCE_PCT: f64 = 30.0;
/// Default settle time after taking the lock, before reading utilisation: the
/// co-located miner polls its yield flag, and utilisation decays over a few
/// hundred ms after it stops. Too short and we skip on our own miner's tail.
pub const DEFAULT_SETTLE_MS: u64 = 2_000;
/// Utilisation (%) above which the device counts as busy.
pub const DEFAULT_MAX_UTIL_PCT: u32 = 10;
/// Fraction of VRAM in use above which the device counts as busy even when
/// utilisation reads low (a model that is loaded but idle still leaves no room).
pub const DEFAULT_MAX_VRAM_FRACTION: f64 = 0.5;

// ── The device-load picture ────────────────────────────────────────────────

/// What the box looks like at the moment we want to measure. Every field is
/// optional because every field can be *unreadable* — and "unknown" must never
/// be silently treated as "idle" or as "busy" (see [`assess`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuLoad {
    /// `nvidia-smi utilization.gpu`, percent (None = unreadable/no such probe).
    pub util_percent: Option<u32>,
    /// VRAM in use, MiB (None = unreadable).
    pub vram_used_mb: Option<u64>,
    /// Total VRAM, MiB (None = unreadable).
    pub vram_total_mb: Option<u64>,
    /// Another job (provider or anything else) holds the GPU lock.
    pub lock_held: bool,
    /// Our own provider's in-flight GPU jobs.
    pub in_flight_jobs: u32,
}

impl GpuLoad {
    /// Fraction of VRAM in use, if both numbers are known and sane.
    pub fn vram_fraction(&self) -> Option<f64> {
        match (self.vram_used_mb, self.vram_total_mb) {
            (Some(used), Some(total)) if total > 0 => Some(used as f64 / total as f64),
            _ => None,
        }
    }
}

/// Thresholds for "idle enough to attest".
#[derive(Debug, Clone, PartialEq)]
pub struct IdlePolicy {
    pub max_util_percent: u32,
    pub max_vram_fraction: f64,
}

impl Default for IdlePolicy {
    fn default() -> Self {
        Self {
            max_util_percent: DEFAULT_MAX_UTIL_PCT,
            max_vram_fraction: DEFAULT_MAX_VRAM_FRACTION,
        }
    }
}

impl IdlePolicy {
    /// `GPN_ATTEST_MAX_UTIL_PCT` / `GPN_ATTEST_MAX_VRAM_FRACTION`, clamped so a
    /// typo can neither disable the gate (>100 %) nor make it impossible.
    pub fn from_env() -> Self {
        let d = Self::default();
        let util = std::env::var("GPN_ATTEST_MAX_UTIL_PCT")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .map(|v| v.min(100))
            .unwrap_or(d.max_util_percent);
        let vram = std::env::var("GPN_ATTEST_MAX_VRAM_FRACTION")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v.min(1.0))
            .unwrap_or(d.max_vram_fraction);
        Self {
            max_util_percent: util,
            max_vram_fraction: vram,
        }
    }
}

/// Verdict of [`assess`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Idle,
    Busy(String),
}

impl Gate {
    pub fn is_idle(&self) -> bool {
        matches!(self, Gate::Idle)
    }
}

/// Is this device idle enough that a measurement would describe the *hardware*
/// rather than whatever else is running on it?
///
/// The lock and our own in-flight jobs are decisive (we know them exactly).
/// Utilisation and VRAM are advisory: when unreadable they do not by themselves
/// make the device idle — the caller only reaches here while holding the lock,
/// and an unknown probe (no `nvidia-smi`) usually means no device to measure at
/// all, in which case the measurement fails on its own and nothing is posted.
pub fn assess(load: &GpuLoad, policy: &IdlePolicy) -> Gate {
    if load.lock_held {
        return Gate::Busy("another job holds the GPU lock".to_string());
    }
    if load.in_flight_jobs > 0 {
        return Gate::Busy(format!(
            "{} of our own job(s) are in flight",
            load.in_flight_jobs
        ));
    }
    if let Some(util) = load.util_percent {
        if util > policy.max_util_percent {
            return Gate::Busy(format!(
                "utilization {util}% > {}%",
                policy.max_util_percent
            ));
        }
    }
    if let Some(frac) = load.vram_fraction() {
        if frac > policy.max_vram_fraction {
            return Gate::Busy(format!(
                "VRAM {:.0}% in use > {:.0}%",
                frac * 100.0,
                policy.max_vram_fraction * 100.0
            ));
        }
    }
    Gate::Idle
}

// ── Sample handling: spread, median, best ──────────────────────────────────

/// A set of timed GEMM runs from one idle sitting.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleSet {
    /// Non-zero samples, in run order.
    pub samples: Vec<u64>,
    /// Highest sample (least contaminated run).
    pub best: u64,
    /// Median sample (robust central value).
    pub median: u64,
    /// `(max - min) / median * 100`.
    pub spread_pct: f64,
    /// `spread_pct > SPREAD_THRESHOLD_PCT`: the measurement moved under us.
    pub unstable: bool,
    /// Runs that returned 0 GFLOPS (dropped from `samples`; a zero is a failed
    /// launch, not a slow GPU).
    pub zero_samples: usize,
}

impl SampleSet {
    /// The number to attest: the **best** run when the samples disagree (spread
    /// is contention or throttling, and every disturbance only removes
    /// throughput), the **median** when they agree (a single lucky run is not
    /// evidence of capability either).
    pub fn chosen(&self) -> u64 {
        if self.unstable {
            self.best
        } else {
            self.median
        }
    }

    /// `12/13/45 GFLOPS (median 13, spread 254%)` — the log/summary fragment.
    pub fn describe(&self) -> String {
        let list = self
            .samples
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("/");
        format!(
            "{list} GFLOPS (median {}, spread {:.0}%{})",
            self.median,
            self.spread_pct,
            if self.unstable { ", unstable" } else { "" }
        )
    }
}

/// Summarise raw per-run GFLOPS. `None` when no run produced a real number (all
/// zeros / empty) — that is a failed measurement, never "0 GFLOPS capability".
pub fn summarise(samples: &[u64]) -> Option<SampleSet> {
    let zero_samples = samples.iter().filter(|s| **s == 0).count();
    let mut vals: Vec<u64> = samples.iter().copied().filter(|s| *s > 0).collect();
    if vals.is_empty() {
        return None;
    }
    let best = *vals.iter().max().unwrap();
    let min = *vals.iter().min().unwrap();
    vals.sort_unstable();
    let n = vals.len();
    let median = if n % 2 == 1 {
        vals[n / 2]
    } else {
        (vals[n / 2 - 1] + vals[n / 2]) / 2
    };
    let spread_pct = if median == 0 {
        0.0
    } else {
        (best.saturating_sub(min)) as f64 / median as f64 * 100.0
    };
    Some(SampleSet {
        samples: vals,
        best,
        median,
        spread_pct,
        unstable: spread_pct > SPREAD_THRESHOLD_PCT,
        zero_samples,
    })
}

// ── Stale-record detection ────────────────────────────────────────────────

/// How far the chain's record sits from a fresh idle measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordDivergence {
    pub on_chain: u64,
    pub measured: u64,
    /// `|on_chain - measured| / measured * 100`.
    pub diff_pct: f64,
    /// `"above"` when the record overstates the card, `"below"` when it
    /// understates it.
    pub direction: &'static str,
}

impl std::fmt::Display for RecordDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "on-chain record {} GFLOPS is {:.0}% {} this card's idle measurement of {} GFLOPS",
            self.on_chain, self.diff_pct, self.direction, self.measured
        )
    }
}

/// `Some` when the record and an idle measurement disagree by more than
/// `tolerance_pct`. A missing/zero record is not "divergence" (there is nothing
/// to be wrong about) — that case is handled by the attest plan.
pub fn record_divergence(
    on_chain: Option<u64>,
    measured: u64,
    tolerance_pct: f64,
) -> Option<RecordDivergence> {
    let on_chain = on_chain.filter(|g| *g > 0)?;
    if measured == 0 {
        return None;
    }
    let diff_pct = (on_chain as f64 - measured as f64).abs() / measured as f64 * 100.0;
    if diff_pct <= tolerance_pct {
        return None;
    }
    Some(RecordDivergence {
        on_chain,
        measured,
        diff_pct,
        direction: if on_chain > measured { "above" } else { "below" },
    })
}

// ── Retry scheduling ──────────────────────────────────────────────────────

/// Exponential backoff for "the GPU was busy, come back later". Pure: it counts
/// attempts and hands out delays, the caller owns the clock.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrySchedule {
    base_secs: u64,
    max_secs: u64,
    attempts: u32,
}

impl RetrySchedule {
    pub fn new(base_secs: u64, max_secs: u64) -> Self {
        Self {
            base_secs: base_secs.max(1),
            max_secs: max_secs.max(base_secs.max(1)),
            attempts: 0,
        }
    }

    /// `GPN_ATTEST_RETRY_SECS` (default 300) doubling to
    /// `GPN_ATTEST_RETRY_MAX_SECS` (default 3 600).
    pub fn from_env() -> Self {
        let num = |k: &str, d: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(d)
        };
        Self::new(
            num("GPN_ATTEST_RETRY_SECS", 300),
            num("GPN_ATTEST_RETRY_MAX_SECS", 3_600),
        )
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Record a skipped attempt and return the delay before the next one.
    pub fn on_busy(&mut self) -> u64 {
        self.attempts = self.attempts.saturating_add(1);
        self.delay_secs()
    }

    /// The delay the *next* busy outcome would produce, without recording one.
    pub fn delay_secs(&self) -> u64 {
        let exp = self.attempts.saturating_sub(1).min(16);
        self.base_secs
            .saturating_mul(1u64 << exp)
            .min(self.max_secs)
    }

    /// A successful (or no-longer-needed) attempt resets the backoff.
    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

// ── Measurement orchestration ─────────────────────────────────────────────

/// The OS-facing half. Injected so the busy path is testable without a GPU and
/// without sleeping.
pub trait AttestProbe {
    /// Take exclusive ownership of the GPU **without waiting**. `Err(reason)`
    /// when another job has it.
    fn begin(&mut self) -> Result<(), String>;
    /// Read the device's current load (utilisation / VRAM).
    fn load(&self) -> GpuLoad;
    /// Run the benchmark `runs` times; one GFLOPS figure per run.
    fn measure(&mut self, runs: usize) -> Result<Vec<u64>, String>;
    /// Release ownership (idempotent, also called on the skip paths).
    fn end(&mut self);
}

/// Result of one guarded measurement attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum IdleMeasurement {
    /// The GPU was busy: nothing was measured, nothing may be submitted.
    Busy {
        reason: String,
        load: GpuLoad,
        /// Set when the probe managed to take the lock but the device was still
        /// loaded by something else after the settle period.
        after_settle: bool,
    },
    /// Measured on an idle device.
    Measured { set: SampleSet, load: GpuLoad },
    /// The device looked idle but the benchmark could not run (no OpenCL
    /// device, kernel error, all-zero runs).
    Failed { reason: String },
}

impl IdleMeasurement {
    pub fn is_measured(&self) -> bool {
        matches!(self, IdleMeasurement::Measured { .. })
    }
}

/// Gate → settle → re-check → measure → release.
///
/// `settle` is called (once) between taking the lock and reading utilisation;
/// the real caller sleeps [`DEFAULT_SETTLE_MS`], tests pass a no-op.
///
/// The **second** load check is the important one: taking the lock only stops
/// our own miner, and the flag takes a moment to reach it. Reading utilisation
/// immediately would report the miner's tail as "busy" forever.
pub fn measure_when_idle<P: AttestProbe + ?Sized>(
    probe: &mut P,
    policy: &IdlePolicy,
    runs: usize,
    settle: impl FnOnce(),
) -> IdleMeasurement {
    if let Err(reason) = probe.begin() {
        return IdleMeasurement::Busy {
            reason,
            load: GpuLoad {
                lock_held: true,
                ..Default::default()
            },
            after_settle: false,
        };
    }
    settle();
    // `begin()` succeeded, so we own the lock; `load.lock_held` describes
    // *other* holders only and is false on the real probe at this point.
    let load = probe.load();
    if let Gate::Busy(reason) = assess(&load, policy) {
        probe.end();
        return IdleMeasurement::Busy {
            reason,
            load,
            after_settle: true,
        };
    }
    let samples = probe.measure(runs.max(1));
    probe.end();
    match samples {
        Ok(s) => match summarise(&s) {
            Some(set) => IdleMeasurement::Measured { set, load },
            None => IdleMeasurement::Failed {
                reason: format!("no non-zero sample ({s:?}) — the GEMM never ran"),
            },
        },
        Err(e) => IdleMeasurement::Failed { reason: e },
    }
}

/// Operator-facing line for a skipped attestation. Pure so the wording (and the
/// numbers in it) is testable without a GPU:
///
/// ```text
/// [attest] skipping: GPU busy (util 97%, 3004/8188 MiB VRAM, utilization 97% > 10%) — will retry later; NO measurement submitted (a contended reading is not capability)
/// ```
pub fn busy_summary(load: &GpuLoad, reason: &str, after_settle: bool, settle: Duration) -> String {
    let util = match load.util_percent {
        Some(u) => format!("{u}%"),
        None => "unknown".to_string(),
    };
    let vram = match (load.vram_used_mb, load.vram_total_mb) {
        (Some(u), Some(t)) => format!("{u}/{t} MiB VRAM"),
        _ => "VRAM unknown".to_string(),
    };
    let tail = if after_settle {
        format!(
            " — still busy after the {}ms settle window, so this is another workload on the \
             card (an operator job, a desktop session), not our miner",
            settle.as_millis()
        )
    } else {
        String::new()
    };
    format!(
        "[attest] skipping: GPU busy (util {util}, {vram}, {reason}) — will retry later; NO \
         measurement submitted (a contended reading is not capability){tail}"
    )
}

// ── Persisted conditions (read by /status) ────────────────────────────────

/// Where the last attestation measurement came from, and what it looked like.
/// Written by the provider next to the GPU lock; read by `gpn_provider_status`
/// so an operator can tell a real number from a stale one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttestState {
    /// The value this process measured **on an idle GPU** and would attest.
    pub measured_gflops: u64,
    /// Per-run samples (GFLOPS), in run order.
    pub samples: Vec<u64>,
    pub median_gflops: u64,
    pub spread_pct: f64,
    /// `"idle"` when every gate passed — the only value a new record may carry.
    pub conditions: String,
    pub util_percent: Option<u32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub runs: usize,
    /// Unix seconds.
    pub at_unix: u64,
    /// Whether this measurement was actually submitted on chain.
    pub submitted: bool,
    /// The record that was on chain before this attempt (None = none/unknown).
    pub on_chain_before: Option<u64>,
}

/// `$GPN_ATTEST_STATE`, else `$HOME/.gpn/attest-state.json` (next to the lock).
pub fn state_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_ATTEST_STATE") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join(".gpn/attest-state.json")
}

/// Best-effort: a failed write must never fail an attestation.
pub fn write_state(state: &AttestState) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(state) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&path, s) {
                eprintln!("[attest] could not write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("[attest] could not serialise attest state: {e}"),
    }
}

/// `None` when absent/unreadable — `/status` then simply omits the fields.
pub fn read_state() -> Option<AttestState> {
    let raw = std::fs::read_to_string(state_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The `/status` fragment for a recorded measurement: `("attested-idle", {...})`
/// when the advertised value is the one this box measured on an idle GPU,
/// otherwise the plain source string with the state attached for context.
pub fn status_gflops_source(
    advertised: u64,
    chain: Option<u64>,
    state: Option<&AttestState>,
) -> (&'static str, Option<String>) {
    let Some(st) = state else {
        return ("", None);
    };
    let matches = st.submitted && st.measured_gflops == advertised;
    let warning = chain
        .filter(|c| *c > 0)
        .and_then(|c| record_divergence(Some(c), st.measured_gflops, RECORD_TOLERANCE_PCT))
        .map(|d| d.to_string());
    if matches {
        ("attested-idle", warning)
    } else {
        ("", warning)
    }
}

// ── The real probe ────────────────────────────────────────────────────────

/// Utilisation / VRAM of `device` from `nvidia-smi`, if it answers.
/// `(util_percent, vram_used_mb, vram_total_mb)`.
pub fn nvidia_smi_load(device: usize) -> Option<(u32, u64, u64)> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
            "-i",
            &device.to_string(),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_nvidia_smi_load(&String::from_utf8_lossy(&out.stdout))
}

/// `"7, 512, 16303"` → `(7, 512, 16303)`. Pure, so the parse is testable.
pub fn parse_nvidia_smi_load(s: &str) -> Option<(u32, u64, u64)> {
    let line = s.lines().find(|l| !l.trim().is_empty())?;
    let mut it = line.split(',');
    let util = it.next()?.trim().parse::<u32>().ok()?;
    let used = it.next()?.trim().parse::<u64>().ok()?;
    let total = it.next()?.trim().parse::<u64>().ok()?;
    Some((util, used, total))
}

/// The production probe: real lock, real `nvidia-smi`, real GEMM.
pub struct RealProbe {
    device: usize,
    guard: Option<crate::gpu_yield::GpuYieldGuard>,
}

impl RealProbe {
    pub fn new(device: usize) -> Self {
        Self {
            device,
            guard: None,
        }
    }
}

impl AttestProbe for RealProbe {
    fn begin(&mut self) -> Result<(), String> {
        match crate::gpu_yield::GpuYieldGuard::try_acquire() {
            Some(g) => {
                self.guard = Some(g);
                Ok(())
            }
            None => {
                let holder = std::fs::read_to_string(crate::gpu_yield::lock_path())
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                Err(match holder {
                    Some(h) => format!("another job holds the GPU lock ({h})"),
                    None => "another job holds the GPU lock".to_string(),
                })
            }
        }
    }

    fn load(&self) -> GpuLoad {
        match nvidia_smi_load(self.device) {
            Some((util, used, total)) => GpuLoad {
                util_percent: Some(util),
                vram_used_mb: Some(used),
                vram_total_mb: Some(total),
                ..Default::default()
            },
            // No `nvidia-smi` (AMD, iGPU, or a container without device
            // access): utilisation is unknowable. The lock is still decisive,
            // and a card we cannot even query will fail the GEMM anyway.
            None => GpuLoad::default(),
        }
    }

    fn measure(&mut self, runs: usize) -> Result<Vec<u64>, String> {
        crate::gpu::measure_gflops_samples_locked(self.device, runs)
    }

    fn end(&mut self) {
        // Drop order matters: the guard clears the yield flag, then releases
        // the lock (see gpu_yield), letting the miner resume.
        self.guard = None;
    }
}

/// `GPN_ATTEST_SAMPLES` (default [`DEFAULT_RUNS`]), clamped to 1..=10.
pub fn runs_from_env() -> usize {
    std::env::var("GPN_ATTEST_SAMPLES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|v| v.clamp(1, 10))
        .unwrap_or(DEFAULT_RUNS)
}

/// `GPN_ATTEST_SETTLE_MS` (default [`DEFAULT_SETTLE_MS`]), clamped to 0..=30 000.
pub fn settle_from_env() -> Duration {
    let ms = std::env::var("GPN_ATTEST_SETTLE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.min(30_000))
        .unwrap_or(DEFAULT_SETTLE_MS);
    Duration::from_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    // ── injectable probe ──────────────────────────────────────────────────

    /// Records what the gate did, so a test can assert that a busy GPU never
    /// reached the benchmark.
    struct FakeProbe {
        begin: Result<(), String>,
        load: GpuLoad,
        samples: Result<Vec<u64>, String>,
        measured_runs: RefCell<Vec<usize>>,
        begun: Rc<Cell<usize>>,
        ended: Rc<Cell<usize>>,
        settled: Rc<Cell<usize>>,
    }

    impl FakeProbe {
        fn ok(load: GpuLoad, samples: Vec<u64>) -> Self {
            Self {
                begin: Ok(()),
                load,
                samples: Ok(samples),
                measured_runs: RefCell::new(vec![]),
                begun: Rc::new(Cell::new(0)),
                ended: Rc::new(Cell::new(0)),
                settled: Rc::new(Cell::new(0)),
            }
        }
    }

    impl AttestProbe for FakeProbe {
        fn begin(&mut self) -> Result<(), String> {
            self.begun.set(self.begun.get() + 1);
            self.begin.clone()
        }
        fn load(&self) -> GpuLoad {
            self.load.clone()
        }
        fn measure(&mut self, runs: usize) -> Result<Vec<u64>, String> {
            self.measured_runs.borrow_mut().push(runs);
            self.samples.clone()
        }
        fn end(&mut self) {
            self.ended.set(self.ended.get() + 1);
        }
    }

    fn idle_load() -> GpuLoad {
        GpuLoad {
            util_percent: Some(3),
            vram_used_mb: Some(700),
            vram_total_mb: Some(16_303),
            lock_held: false,
            in_flight_jobs: 0,
        }
    }

    fn run(probe: &mut FakeProbe, runs: usize) -> IdleMeasurement {
        let policy = IdlePolicy::default();
        let settled = probe.settled.clone();
        measure_when_idle(probe, &policy, runs, || settled.set(settled.get() + 1))
    }

    // ── 1. skip when busy ─────────────────────────────────────────────────

    #[test]
    fn busy_lock_skips_before_measuring() {
        let mut p = FakeProbe {
            begin: Err("another job holds the GPU lock (pid=4242 since=1)".to_string()),
            ..FakeProbe::ok(idle_load(), vec![1_300, 1_310, 1_290])
        };
        let out = run(&mut p, 3);
        match out {
            IdleMeasurement::Busy {
                reason,
                after_settle,
                ..
            } => {
                assert!(reason.contains("holds the GPU lock"), "{reason}");
                assert!(!after_settle, "lock-busy is decided before settling");
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        // The measurement must never have been run: a contended sample is not
        // a capability figure.
        assert!(p.measured_runs.borrow().is_empty(), "benchmark ran while busy");
        // No settle sleep either — we know immediately.
        assert_eq!(p.settled.get(), 0);
        // begin() failed, so there is nothing to release.
        assert_eq!(p.ended.get(), 0);
    }

    #[test]
    fn busy_utilisation_after_settle_skips_and_releases() {
        let load = GpuLoad {
            util_percent: Some(97),
            ..idle_load()
        };
        let mut p = FakeProbe::ok(load, vec![26, 44, 45]);
        let out = run(&mut p, 3);
        match out {
            IdleMeasurement::Busy {
                reason,
                after_settle,
                load,
            } => {
                assert!(reason.contains("97%"), "{reason}");
                assert!(after_settle, "the busy verdict came after the settle");
                assert_eq!(load.util_percent, Some(97));
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        assert!(p.measured_runs.borrow().is_empty());
        // Settle happened once (the miner gets its chance to stand down) and
        // the lock is released even on the skip path.
        assert_eq!(p.settled.get(), 1);
        assert_eq!(p.ended.get(), 1);
    }

    #[test]
    fn busy_vram_skips_even_when_utilisation_reads_low() {
        // A loaded model that is momentarily idle still leaves no room for a
        // stable measurement.
        let load = GpuLoad {
            util_percent: Some(2),
            vram_used_mb: Some(15_000),
            vram_total_mb: Some(16_303),
            ..idle_load()
        };
        let mut p = FakeProbe::ok(load, vec![1_300]);
        match run(&mut p, 3) {
            IdleMeasurement::Busy { reason, .. } => {
                assert!(reason.contains("VRAM"), "{reason}")
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        assert!(p.measured_runs.borrow().is_empty());
    }

    #[test]
    fn in_flight_job_counts_as_busy() {
        let load = GpuLoad {
            in_flight_jobs: 1,
            ..idle_load()
        };
        let mut p = FakeProbe::ok(load, vec![1_300]);
        match run(&mut p, 3) {
            IdleMeasurement::Busy { reason, .. } => {
                assert!(reason.contains("in flight"), "{reason}")
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        assert!(p.measured_runs.borrow().is_empty());
    }

    #[test]
    fn unknown_utilisation_does_not_block_an_otherwise_idle_device() {
        // No `nvidia-smi` (AMD / container). The lock is the decisive signal.
        let load = GpuLoad::default();
        let mut p = FakeProbe::ok(load, vec![1_280, 1_300, 1_290]);
        let out = run(&mut p, 3);
        assert!(out.is_measured(), "{out:?}");
    }

    // ── 2. spread handling ────────────────────────────────────────────────

    #[test]
    fn stable_samples_use_the_median() {
        let mut p = FakeProbe::ok(idle_load(), vec![1_300, 1_320, 1_280]);
        match run(&mut p, 3) {
            IdleMeasurement::Measured { set, .. } => {
                assert!(!set.unstable, "spread {:.1}% should be stable", set.spread_pct);
                assert_eq!(set.chosen(), 1_300, "median of 1280/1300/1320");
                assert_eq!(set.best, 1_320);
            }
            other => panic!("expected Measured, got {other:?}"),
        }
        assert_eq!(p.measured_runs.borrow().as_slice(), &[3usize]);
    }

    #[test]
    fn unstable_samples_take_the_best_and_report_the_spread() {
        // The box1 shape, scaled to a healthy card: 900/1100/1300.
        let mut p = FakeProbe::ok(idle_load(), vec![900, 1_100, 1_300]);
        match run(&mut p, 3) {
            IdleMeasurement::Measured { set, .. } => {
                assert!(set.unstable, "spread {:.1}% should be unstable", set.spread_pct);
                assert_eq!(set.chosen(), 1_300, "best sample wins when spread is high");
                assert!((set.spread_pct - 36.36).abs() < 0.1, "{}", set.spread_pct);
                assert!(set.describe().contains("unstable"));
            }
            other => panic!("expected Measured, got {other:?}"),
        }
    }

    #[test]
    fn spread_threshold_is_inclusive_at_the_boundary() {
        // 90 vs 110 around a median of 100 = exactly 20.0% → not "over" 20%.
        let set = summarise(&[90, 100, 110]).unwrap();
        assert!((set.spread_pct - 20.0).abs() < 1e-9, "{}", set.spread_pct);
        assert!(!set.unstable);
        let set = summarise(&[90, 100, 111]).unwrap();
        assert!(set.unstable, "{} should be unstable", set.spread_pct);
    }

    #[test]
    fn zero_runs_are_dropped_and_all_zero_is_a_failure() {
        let set = summarise(&[0, 1_300, 0]).unwrap();
        assert_eq!(set.samples, vec![1_300]);
        assert_eq!(set.zero_samples, 2);
        assert_eq!(set.chosen(), 1_300);

        let mut p = FakeProbe::ok(idle_load(), vec![0, 0, 0]);
        match run(&mut p, 3) {
            IdleMeasurement::Failed { reason } => {
                assert!(reason.contains("no non-zero sample"), "{reason}")
            }
            other => panic!("expected Failed (never 0 GFLOPS), got {other:?}"),
        }
        assert!(summarise(&[]).is_none());
    }

    #[test]
    fn a_single_sample_is_its_own_best_and_median() {
        let set = summarise(&[1_234]).unwrap();
        assert_eq!(set.chosen(), 1_234);
        assert_eq!(set.spread_pct, 0.0);
        assert!(!set.unstable);
    }

    #[test]
    fn measurement_errors_are_reported_not_zeroed() {
        let mut p = FakeProbe {
            samples: Err("open GPU matmul: no device".to_string()),
            ..FakeProbe::ok(idle_load(), vec![])
        };
        match run(&mut p, 3) {
            IdleMeasurement::Failed { reason } => {
                assert!(reason.contains("no device"), "{reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // Even a failure releases the lock.
        assert_eq!(p.ended.get(), 1);
    }

    // ── 3. stale-vs-measured warning ──────────────────────────────────────

    #[test]
    fn record_far_above_measurement_warns() {
        // box1 exactly: 188 on chain, ~45 measured under contention.
        let d = record_divergence(Some(188), 45, RECORD_TOLERANCE_PCT).unwrap();
        assert_eq!(d.direction, "above");
        assert!((d.diff_pct - 317.78).abs() < 0.1, "{}", d.diff_pct);
        let msg = d.to_string();
        assert!(msg.contains("188"), "{msg}");
        assert!(msg.contains("45"), "{msg}");
    }

    #[test]
    fn record_far_below_measurement_warns_with_the_other_direction() {
        let d = record_divergence(Some(900), 1_300, RECORD_TOLERANCE_PCT).unwrap();
        assert_eq!(d.direction, "below");
        assert!(d.to_string().contains("below"));
    }

    #[test]
    fn record_within_tolerance_is_quiet() {
        assert!(record_divergence(Some(1_300), 1_300, RECORD_TOLERANCE_PCT).is_none());
        // 30% exactly is not "more than".
        assert!(record_divergence(Some(1_300), 1_000, RECORD_TOLERANCE_PCT).is_none());
        assert!(record_divergence(Some(1_000), 1_300, RECORD_TOLERANCE_PCT).is_none());
    }

    #[test]
    fn missing_or_zero_record_is_not_divergence() {
        assert!(record_divergence(None, 1_300, RECORD_TOLERANCE_PCT).is_none());
        assert!(record_divergence(Some(0), 1_300, RECORD_TOLERANCE_PCT).is_none());
        assert!(record_divergence(Some(1_300), 0, RECORD_TOLERANCE_PCT).is_none());
    }

    // ── 4. retry scheduling ───────────────────────────────────────────────

    #[test]
    fn retry_backoff_doubles_from_the_base_and_caps() {
        let mut s = RetrySchedule::new(300, 1_000);
        assert_eq!(s.attempts(), 0);
        assert_eq!(s.on_busy(), 300);
        assert_eq!(s.attempts(), 1);
        assert_eq!(s.on_busy(), 600);
        assert_eq!(s.on_busy(), 1_000, "capped at max");
        assert_eq!(s.on_busy(), 1_000);
        assert_eq!(s.attempts(), 4);
    }

    #[test]
    fn retry_resets_after_a_success() {
        let mut s = RetrySchedule::new(60, 600);
        s.on_busy();
        s.on_busy();
        assert_eq!(s.attempts(), 2);
        s.reset();
        assert_eq!(s.attempts(), 0);
        assert_eq!(s.on_busy(), 60, "backoff starts over");
    }

    #[test]
    fn retry_schedule_tolerates_absurd_env_values() {
        // base > max is clamped, never zero-delay (a busy GPU must not be
        // hammered in a spin loop).
        let s = RetrySchedule::new(600, 60);
        assert_eq!(s.delay_secs(), 600);
        let s = RetrySchedule::new(0, 0);
        assert_eq!(s.delay_secs(), 1);
    }

    // ── 5. /status reporting ──────────────────────────────────────────────

    fn state(measured: u64, submitted: bool) -> AttestState {
        AttestState {
            measured_gflops: measured,
            samples: vec![measured - 10, measured, measured - 5],
            median_gflops: measured - 5,
            spread_pct: 0.8,
            conditions: "idle".to_string(),
            util_percent: Some(1),
            vram_used_mb: Some(400),
            vram_total_mb: Some(16_303),
            runs: 3,
            at_unix: 1_760_000_000,
            submitted,
            on_chain_before: Some(188),
        }
    }

    #[test]
    fn status_reports_attested_idle_when_the_advertised_value_is_ours() {
        let st = state(1_300, true);
        let (src, warn) = status_gflops_source(1_300, Some(1_300), Some(&st));
        assert_eq!(src, "attested-idle");
        assert!(warn.is_none());
    }

    #[test]
    fn status_stays_quiet_about_provenance_when_the_record_differs() {
        let st = state(1_300, true);
        // Advertising the chain's stale 188 while our idle measurement says
        // 1300: not "attested-idle", and the divergence is named.
        let (src, warn) = status_gflops_source(188, Some(188), Some(&st));
        assert_eq!(src, "");
        let warn = warn.expect("divergence must be reported");
        assert!(warn.contains("188") && warn.contains("1300"), "{warn}");
    }

    #[test]
    fn status_never_claims_a_measurement_that_was_not_submitted() {
        let st = state(1_300, false);
        let (src, _) = status_gflops_source(1_300, Some(1_300), Some(&st));
        assert_eq!(src, "");
    }

    #[test]
    fn state_round_trips_through_json() {
        let st = state(1_300, true);
        let json = serde_json::to_string(&st).unwrap();
        let back: AttestState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, st);
    }

    // ── 6. OS parsing / defaults ──────────────────────────────────────────

    #[test]
    fn nvidia_smi_csv_parses() {
        assert_eq!(
            parse_nvidia_smi_load("7, 512, 16303\n"),
            Some((7, 512, 16_303))
        );
        assert_eq!(parse_nvidia_smi_load("  0, 3 , 24564 "), Some((0, 3, 24_564)));
        assert_eq!(parse_nvidia_smi_load(""), None);
        assert_eq!(parse_nvidia_smi_load("N/A, 3, 4"), None);
    }

    #[test]
    fn busy_summary_names_the_conditions_and_the_absence_of_a_submission() {
        let load = GpuLoad {
            util_percent: Some(97),
            vram_used_mb: Some(3_004),
            vram_total_mb: Some(8_188),
            ..Default::default()
        };
        let line = busy_summary(&load, "utilization 97% > 10%", true, Duration::from_millis(2_000));
        assert!(line.contains("[attest] skipping: GPU busy"), "{line}");
        assert!(line.contains("util 97%"), "{line}");
        assert!(line.contains("3004/8188 MiB VRAM"), "{line}");
        assert!(line.contains("NO measurement submitted"), "{line}");
        assert!(line.contains("2000ms settle"), "{line}");
        // Lock-busy (no settle yet): no misleading settle note.
        let line = busy_summary(&GpuLoad::default(), "another job holds the GPU lock", false, Duration::ZERO);
        assert!(line.contains("util unknown"), "{line}");
        assert!(!line.contains("settle"), "{line}");
    }

    #[test]
    fn vram_fraction_needs_both_numbers() {
        let mut l = GpuLoad::default();
        assert_eq!(l.vram_fraction(), None);
        l.vram_total_mb = Some(0);
        assert_eq!(l.vram_fraction(), None);
        l.vram_total_mb = Some(1_000);
        l.vram_used_mb = Some(250);
        assert_eq!(l.vram_fraction(), Some(0.25));
    }

    // ── 7. the real probe, against this box (manual runs) ─────────────────
    //
    // Ignored by default: they touch the process-global GPU lock path, so they
    // must not race the `gpu_yield` tests (which mutate `GPN_GPU_LOCK`) in the
    // same process. Run with:
    //   cargo test --lib attest_idle::tests::real_ -- --ignored --test-threads=1 --nocapture

    #[test]
    #[ignore]
    fn real_probe_busy_skips_before_measuring() {
        let held = crate::gpu_yield::GpuYieldGuard::acquire_with_timeout(Duration::from_secs(5))
            .expect("could not take the GPU lock for the test");
        let mut probe = RealProbe::new(0);
        let out = measure_when_idle(&mut probe, &IdlePolicy::default(), 3, || {});
        println!("busy-path outcome: {out:?}");
        match &out {
            IdleMeasurement::Busy { reason, .. } => {
                assert!(reason.contains("lock"), "{reason}");
            }
            other => panic!("expected Busy while the lock is held, got {other:?}"),
        }
        drop(held);
    }

    #[test]
    #[ignore]
    fn real_probe_measures_an_idle_device() {
        let mut probe = RealProbe::new(0);
        let settle = Duration::from_millis(2_000);
        let out = measure_when_idle(&mut probe, &IdlePolicy::default(), 3, || {
            std::thread::sleep(settle)
        });
        println!("idle-path outcome: {out:?}");
        match out {
            IdleMeasurement::Measured { set, load } => {
                println!(
                    "measured {} on an idle GPU (util {:?}%, vram {:?}/{:?} MiB)",
                    set.describe(),
                    load.util_percent,
                    load.vram_used_mb,
                    load.vram_total_mb
                );
                assert!(set.chosen() > 0);
            }
            // A genuinely busy box is the *correct* outcome here, not a failure.
            other => println!("device not idle: {other:?}"),
        }
    }
}
