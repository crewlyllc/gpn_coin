//! Idle mode — "let me play a game without turning my node off".
//!
//! Idle is **not** a power switch. While idle the node:
//!
//! - **keeps** syncing, peering, and serving the chain (it is still a node),
//! - **stops mining** (the miner thread parks; see `gpn-node` `service.rs`),
//! - **stops claiming new AI jobs** (providers skip the claim boundary).
//!
//! Everything resumes automatically at `until`, or immediately on
//! `POST /api/resume` / `gpn_node resume`.
//!
//! ## Why a file
//!
//! The supervisor (`gpn_node`), the chain node/miner (`gpn-node`) and every
//! provider are **separate processes** (fault isolation is deliberate). The
//! cheapest cross-process, cross-language, reboot-safe channel is a single
//! JSON file — the same mechanism already used for the GPU-yield flag
//! (`gpu_yield.rs`). No IPC, no sockets, no shared memory.
//!
//! Path: `$GPN_IDLE_FILE`, else `~/.gpn/idle.json`, mode 0600.
//!
//! ```json
//! {"until": 1760000000, "reason": "gaming", "started": 1759978400}
//! ```
//!
//! A file whose `until` is in the past is **expired**, never "idle forever":
//! every reader treats it as absent (and removes it), so a node that was down
//! past the deadline starts **active**.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard ceiling for one idle window: 6 hours.
pub const MAX_IDLE_MINUTES: u64 = 6 * 60;
/// Same ceiling in seconds (what the file actually stores).
pub const MAX_IDLE_SECS: u64 = MAX_IDLE_MINUTES * 60;

/// Persisted idle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleState {
    /// Unix seconds at which idle ends and the node resumes by itself.
    pub until: u64,
    /// Free-form operator reason ("gaming", "rendering", "quiet hours").
    #[serde(default)]
    pub reason: String,
    /// Unix seconds at which idle began.
    pub started: u64,
}

/// Why an idle request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleError {
    /// `minutes` was 0 or not a number.
    Zero,
    /// Longer than [`MAX_IDLE_MINUTES`]; the message states the ceiling.
    TooLong { requested: u64, max: u64 },
    /// The state file could not be written.
    Io(String),
}

impl std::fmt::Display for IdleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdleError::Zero => write!(f, "idle duration must be at least 1 minute"),
            IdleError::TooLong { requested, max } => write!(
                f,
                "idle duration {requested} min is longer than the {max} min (6 h) maximum"
            ),
            IdleError::Io(e) => write!(f, "cannot write the idle state file: {e}"),
        }
    }
}

impl std::error::Error for IdleError {}

/// Unix seconds now (0 if the clock is before the epoch — never panic).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Path of the idle state file: `$GPN_IDLE_FILE`, else `~/.gpn/idle.json`.
pub fn idle_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_IDLE_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".gpn").join("idle.json")
}

// ── pure helpers (unit-tested without touching the filesystem) ──────────────

/// Is `state` still in force at `now`?
///
/// The boundary is **inclusive of the deadline**: at exactly `until` idle is
/// over. (`until > now` ⇒ idle.) This makes `now == until` deterministic and
/// matches "resume at 18:00" meaning 18:00 is active again.
pub fn is_active_at(state: &IdleState, now: u64) -> bool {
    now < state.until
}

/// Whole seconds left, or `None` when not idle.
pub fn remaining_secs_at(state: &IdleState, now: u64) -> Option<u64> {
    if is_active_at(state, now) {
        Some(state.until - now)
    } else {
        None
    }
}

/// Validate an operator-requested duration.
///
/// Zero is rejected rather than clamped (an accidental "idle 0" should not
/// silently do nothing), and anything above 6 h is **rejected with a clear
/// message** — the GUI only offers 30 m … 6 h, so a longer value can only come
/// from a script and deserves a loud answer, not a silent trim.
pub fn validate_minutes(minutes: u64) -> Result<u64, IdleError> {
    if minutes == 0 {
        return Err(IdleError::Zero);
    }
    if minutes > MAX_IDLE_MINUTES {
        return Err(IdleError::TooLong {
            requested: minutes,
            max: MAX_IDLE_MINUTES,
        });
    }
    Ok(minutes)
}

/// Clamp a duration taken from *config/env* (never from an operator request).
///
/// Config is a default, not an instruction, so a typo'd `GPN_IDLE_MINUTES=1440`
/// should idle for the maximum rather than refuse to start.
pub fn clamp_minutes(minutes: u64) -> u64 {
    minutes.clamp(1, MAX_IDLE_MINUTES)
}

/// The safety valve: would idling this node stop the chain?
///
/// Idling is only safe when **another** block producer exists. The only cheap,
/// always-available signal is the node's peer count from its own
/// `system_health` RPC:
///
/// - `Some(n)` with `n >= 1` → at least one peer holds a different peer id.
///   We cannot prove that peer is an authority, but a lone authority has no
///   peers at all, so this rules the dangerous case out.
/// - `Some(0)`, or `None` (RPC unreadable — the node may be down/starting) →
///   refuse: "I cannot see any other node" is exactly the state in which
///   idling halts block production.
///
/// `force` (`--force-idle` / `GPN_IDLE_FORCE=1`) is the documented override for
/// single-box dev chains and for operators who know the peers are lying.
pub fn refusal_reason(peers: Option<u64>, force: bool) -> Option<String> {
    if force {
        return None;
    }
    match peers {
        Some(n) if n >= 1 => None,
        Some(_) => Some(
            "this node has no peers, so it may be the only block producer — idling would stop \
             the chain. Start another authority (or pass --force-idle / GPN_IDLE_FORCE=1 for a \
             single-box dev chain)."
                .to_string(),
        ),
        None => Some(
            "cannot read the node's peer count (its RPC is unreachable), so idle cannot be \
             proven safe. Wait for the node to start, or pass --force-idle / GPN_IDLE_FORCE=1."
                .to_string(),
        ),
    }
}

// ── filesystem ──────────────────────────────────────────────────────────────

/// Read the state file at `path`, returning the parsed state **even if
/// expired** (callers decide). `None` for a missing/corrupt file.
pub fn read_state_at(path: &Path) -> Option<IdleState> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<IdleState>(&raw).ok()
}

/// The state in force right now at `path`, pruning an expired file.
pub fn state_at(path: &Path, now: u64) -> Option<IdleState> {
    let st = read_state_at(path)?;
    if is_active_at(&st, now) {
        Some(st)
    } else {
        // Expired while the node was down (or between polls): drop it so the
        // next reader sees "active" without re-parsing a stale file, and so an
        // operator can see on disk that idle is over.
        let _ = std::fs::remove_file(path);
        None
    }
}

/// Current idle state (the process-wide convenience wrapper).
pub fn current() -> Option<IdleState> {
    state_at(&idle_path(), now_unix())
}

/// True while idle is in force. Cheap enough for a poll loop (one small read).
pub fn is_idle() -> bool {
    current().is_some()
}

/// **The claim gate every provider calls at its claim boundary.**
///
/// Deliberately named as an *allow* predicate so a caller cannot accidentally
/// invert it, and deliberately separate from [`is_idle`] so the reason a
/// provider skipped stays readable at the call site ("idle", not "not idle").
pub fn claims_allowed() -> bool {
    !is_idle()
}

/// Seconds left, if idle.
pub fn remaining_secs() -> Option<u64> {
    let now = now_unix();
    remaining_secs_at(&state_at(&idle_path(), now)?, now)
}

/// Enter idle for `minutes` (**validated**, max 6 h).
pub fn start_at(path: &Path, minutes: u64, reason: &str, now: u64) -> Result<IdleState, IdleError> {
    let minutes = validate_minutes(minutes)?;
    let st = IdleState {
        until: now.saturating_add(minutes * 60),
        reason: reason.to_string(),
        started: now,
    };
    write_state(path, &st)?;
    Ok(st)
}

/// Write the state file with 0600 permissions (the reason may be personal —
/// "gaming", "doctor's appointment" — and it is nobody else's business).
pub fn write_state(path: &Path, st: &IdleState) -> Result<(), IdleError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| IdleError::Io(e.to_string()))?;
    }
    let body = serde_json::to_string(st).map_err(|e| IdleError::Io(e.to_string()))?;
    std::fs::write(path, body).map_err(|e| IdleError::Io(e.to_string()))?;
    restrict_permissions(path);
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {
    // Windows: the file inherits the user's profile ACL, which is already
    // per-user. Nothing to tighten without a Win32 call.
}

/// Leave idle now. Returns `true` if a state file was actually removed.
pub fn clear_at(path: &Path) -> bool {
    std::fs::remove_file(path).is_ok()
}

/// Leave idle now (convenience wrapper on the default path).
pub fn clear() -> bool {
    clear_at(&idle_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("gpn-idle-test-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p.join("idle.json")
    }

    // ── duration validation / clamp ─────────────────────────────────────────

    #[test]
    fn six_hours_is_the_ceiling_and_longer_is_rejected() {
        assert_eq!(validate_minutes(360), Ok(360));
        assert_eq!(validate_minutes(30), Ok(30));
        assert_eq!(
            validate_minutes(361),
            Err(IdleError::TooLong {
                requested: 361,
                max: 360
            })
        );
        assert_eq!(
            validate_minutes(60 * 24),
            Err(IdleError::TooLong {
                requested: 1440,
                max: 360
            })
        );
        assert_eq!(validate_minutes(0), Err(IdleError::Zero));
    }

    #[test]
    fn rejected_durations_say_what_the_maximum_is() {
        let msg = validate_minutes(600).unwrap_err().to_string();
        assert!(msg.contains("360"), "message must state the ceiling: {msg}");
        assert!(msg.contains("6 h"), "message must say 6 h: {msg}");
    }

    #[test]
    fn config_durations_are_clamped_not_rejected() {
        assert_eq!(clamp_minutes(0), 1);
        assert_eq!(clamp_minutes(45), 45);
        assert_eq!(clamp_minutes(1440), MAX_IDLE_MINUTES);
    }

    // ── time boundaries ─────────────────────────────────────────────────────

    #[test]
    fn idle_is_over_exactly_at_the_deadline() {
        let st = IdleState {
            until: 1_000,
            reason: "gaming".into(),
            started: 400,
        };
        // Just before → idle, with 1s left.
        assert!(is_active_at(&st, 999));
        assert_eq!(remaining_secs_at(&st, 999), Some(1));
        // At the deadline → active again (inclusive boundary).
        assert!(!is_active_at(&st, 1_000));
        assert_eq!(remaining_secs_at(&st, 1_000), None);
        // After → active.
        assert!(!is_active_at(&st, 1_001));
        assert_eq!(remaining_secs_at(&st, 100_000), None);
    }

    // ── file round-trip / persistence ───────────────────────────────────────

    #[test]
    fn state_file_round_trips() {
        let p = tmp("roundtrip");
        let st = start_at(&p, 90, "gaming", 5_000).unwrap();
        assert_eq!(st.until, 5_000 + 90 * 60);

        let back = read_state_at(&p).expect("file must parse");
        assert_eq!(back, st);
        assert_eq!(back.reason, "gaming");

        // And it is in force at the moment it was written.
        assert_eq!(state_at(&p, 5_001).map(|s| s.until), Some(st.until));
    }

    #[test]
    fn restart_with_an_expired_file_starts_active() {
        // The node was down past `until`: the file is still on disk, but every
        // reader must treat the node as ACTIVE and prune the stale file.
        let p = tmp("expired-restart");
        write_state(
            &p,
            &IdleState {
                until: 2_000,
                reason: "was gaming".into(),
                started: 1_000,
            },
        )
        .unwrap();
        assert!(
            state_at(&p, 1_500).is_some(),
            "still idle before the deadline"
        );
        assert!(
            state_at(&p, 2_000).is_none(),
            "at the deadline the node must start active"
        );
        assert!(!p.exists(), "an expired state file is pruned, not kept");
    }

    #[test]
    fn corrupt_state_file_is_ignored_not_fatal() {
        let p = tmp("corrupt");
        std::fs::write(&p, b"{not json").unwrap();
        assert!(state_at(&p, 1).is_none());
        assert!(read_state_at(&p).is_none());
    }

    #[test]
    fn missing_state_file_means_active() {
        let p = tmp("missing");
        assert!(!p.exists());
        assert!(state_at(&p, 1).is_none());
    }

    #[test]
    fn clear_removes_the_file_and_reports_it() {
        let p = tmp("clear");
        start_at(&p, 60, "gaming", 1_000).unwrap();
        assert!(p.exists());
        assert!(clear_at(&p));
        assert!(state_at(&p, 1_001).is_none());
        // Clearing twice is not an error, just "nothing to do".
        assert!(!clear_at(&p));
    }

    #[test]
    fn start_refuses_an_over_long_window_and_leaves_no_file() {
        let p = tmp("toolong");
        let err = start_at(&p, MAX_IDLE_MINUTES + 1, "gaming", 1_000).unwrap_err();
        assert!(matches!(err, IdleError::TooLong { .. }));
        assert!(!p.exists(), "a rejected request must not write a file");
    }

    #[test]
    fn state_file_is_not_world_readable_on_unix() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = tmp("mode");
            start_at(&p, 15, "gaming", 1_000).unwrap();
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "idle.json must be 0600, got {mode:o}");
        }
    }

    // ── safety valve ────────────────────────────────────────────────────────

    #[test]
    fn sole_authority_refuses_idle_unless_forced() {
        // No peers → this may be the only block producer.
        let msg = refusal_reason(Some(0), false).expect("must refuse");
        assert!(msg.contains("only block producer"), "{msg}");
        // RPC unreadable → cannot prove safety.
        assert!(refusal_reason(None, false).is_some());
        // Force override wins in both cases.
        assert!(refusal_reason(Some(0), true).is_none());
        assert!(refusal_reason(None, true).is_none());
        // Any peer at all → allowed.
        assert!(refusal_reason(Some(1), false).is_none());
        assert!(refusal_reason(Some(42), false).is_none());
    }

    #[test]
    fn the_claim_gate_follows_the_state_file() {
        let key = "GPN_IDLE_FILE";
        let old = std::env::var(key).ok();
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("gpn-idle-gate-{n}.json"));
        std::env::set_var(key, &p);

        assert!(claims_allowed(), "no state file → providers may claim");
        start_at(&p, 5, "gaming", now_unix()).unwrap();
        assert!(!claims_allowed(), "idle → providers must not claim");
        assert!(is_idle());
        clear_at(&p);
        assert!(claims_allowed(), "resume → claims allowed again");

        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn idle_file_env_override_wins() {
        // Not a global env mutation test (tests run in parallel): just assert
        // the resolver prefers a non-empty GPN_IDLE_FILE when it is set.
        let key = "GPN_IDLE_FILE";
        let old = std::env::var(key).ok();
        std::env::set_var(key, "/tmp/gpn-idle-env-override.json");
        assert_eq!(
            idle_path(),
            PathBuf::from("/tmp/gpn-idle-env-override.json")
        );
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
