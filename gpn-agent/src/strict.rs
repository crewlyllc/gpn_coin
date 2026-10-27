//! `Strict<T>` — a read you could not complete is **`Unknown`**, never a value.
//!
//! # The bug class this exists to kill (items #89, #100, #105, #108, #110, #111)
//!
//! Every one of those defects had the same shape: a *failed read* was
//! collapsed into a perfectly ordinary value, and a decision was then taken on
//! that value as if the chain had reported it.
//!
//! ```ignore
//! let balance = rpc_read().unwrap_or(0);        // "0 GPN" — the node may be down
//! let feed    = read_feed().ok().flatten();     // "no feed" — or a dead socket
//! let nonce   = get_account_nonce(addr).await;  // 0 — then we signed with it
//! ```
//!
//! `None`/`0`/`false` are all *legitimate answers* from a chain that was read
//! successfully (an unfunded account, a fresh chain with no feed posted, an
//! unattested provider). That is precisely why using them for failure too is
//! unrecoverable: the caller cannot tell "the chain says no" from "we never
//! asked the chain", and both the log and the charge look normal.
//!
//! # The rule
//!
//! * A read you cannot complete is [`Strict::Unknown`], never `0`.
//! * A publication path publishes **nothing** (or `null`) on `Unknown`
//!   instead of a stale or invented value.
//! * A dead connection is *"not yet"*, not *"never"* — see [`retry_read`].
//!
//! `Strict` deliberately has **no** `unwrap_or`/`unwrap_or_default`/
//! `unwrap_or_else`: those are the exact API shape that made the bug possible.
//! A caller must either handle `Unknown`, or spell out that the value is only
//! going somewhere cosmetic by calling [`Strict::or_default_for_display_only`].

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// A value that is either the real answer (`Known`) or an explanation of why
/// we could not get it (`Unknown`). `Unknown` always carries a human-readable
/// reason so a log line, a JSON field or a test can say what happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Strict<T> {
    /// The read succeeded. This — and only this — is a fact about the chain.
    Known(T),
    /// The read failed. Nothing can be concluded; the string says why.
    Unknown(String),
}

impl<T> Strict<T> {
    /// A completed read.
    pub fn known(value: T) -> Self {
        Strict::Known(value)
    }

    /// A read that failed. Logs **once per `(site, reason)`** at `WARN` so a
    /// 1-second poll loop cannot turn one broken read into a log flood, but the
    /// first occurrence is never silent.
    pub fn unknown(site: &str, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        warn_once(site, &reason);
        Strict::Unknown(reason)
    }

    /// Adapt a `Result<_, String>` from a read path.
    pub fn from_result(site: &str, result: Result<T, String>) -> Self {
        match result {
            Ok(v) => Strict::Known(v),
            Err(e) => Strict::unknown(site, e),
        }
    }

    /// `Some(v)` → `Known(v)`; `None` → `Unknown(reason)`.
    ///
    /// Use this only where `None` really does mean *unreadable*. If `None` is a
    /// legitimate answer ("the key is empty on chain"), keep it inside the
    /// `Known` payload, i.e. `Strict<Option<U>>`.
    pub fn from_option(site: &str, option: Option<T>, reason: &str) -> Self {
        match option {
            Some(v) => Strict::Known(v),
            None => Strict::unknown(site, reason),
        }
    }

    pub fn is_known(&self) -> bool {
        matches!(self, Strict::Known(_))
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Strict::Unknown(_))
    }

    /// Why the read failed, if it did.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Strict::Known(_) => None,
            Strict::Unknown(r) => Some(r.as_str()),
        }
    }

    pub fn as_ref(&self) -> Strict<&T> {
        match self {
            Strict::Known(v) => Strict::Known(v),
            Strict::Unknown(r) => Strict::Unknown(r.clone()),
        }
    }

    /// Transform the value. `Unknown` passes through untouched (no reason is
    /// invented, none is dropped).
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Strict<U> {
        match self {
            Strict::Known(v) => Strict::Known(f(v)),
            Strict::Unknown(r) => Strict::Unknown(r),
        }
    }

    pub fn and_then<U, F: FnOnce(T) -> Strict<U>>(self, f: F) -> Strict<U> {
        match self {
            Strict::Known(v) => f(v),
            Strict::Unknown(r) => Strict::Unknown(r),
        }
    }

    /// `Known(v)` → `Some(v)`, `Unknown` → `None`. The failure is still logged
    /// (once) at construction, so this does not lose the reason — but prefer
    /// matching on `Strict` when the value is decision-bearing.
    pub fn into_option(self) -> Option<T> {
        match self {
            Strict::Known(v) => Some(v),
            Strict::Unknown(_) => None,
        }
    }

    /// **For display/logging only.** A UI needs *something* to print, and a
    /// blank field is worse than a dash; a charge, a publish or a route must
    /// never come through here. The name is the fence: if it reads wrong at a
    /// decision site, it is wrong.
    pub fn or_default_for_display_only(self) -> T
    where
        T: Default,
    {
        self.or_for_display_only(T::default())
    }

    /// [`Strict::or_default_for_display_only`] for values with no `Default`
    /// (e.g. `"Unknown"` instead of `"Unattested"`).
    pub fn or_for_display_only(self, placeholder: T) -> T {
        match self {
            Strict::Known(v) => v,
            Strict::Unknown(r) => {
                log::debug!("display placeholder used for an unknown read: {r}");
                placeholder
            }
        }
    }
}

impl<T: Default> Default for Strict<T> {
    /// `Unknown` — the honest default. Constructing `Strict::default()` can
    /// never fabricate a `Known` value.
    fn default() -> Self {
        Strict::Unknown("no read was performed".to_string())
    }
}

/// Emit one `WARN` per `(site, reason)` for the life of the process.
///
/// Log-once matters for the same reason `notice_once` does elsewhere in this
/// tree: the sites this guards are polled every few seconds, and a warn-per-tick
/// would bury the first (actionable) line under thousands of copies. Deduping
/// on the *reason* too means a new failure mode still speaks.
fn warn_once(site: &str, reason: &str) {
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first = match seen.lock() {
        Ok(mut set) => set.insert((site.to_string(), reason.to_string())),
        // A poisoned lock must not silence a warning for the rest of the
        // process: log this one and keep going.
        Err(_) => true,
    };
    if first {
        log::warn!("[strict] {site}: UNKNOWN — {reason}");
    }
}

/// How hard to try before declaring a remote read `Unknown`.
///
/// The shape of #105/#111: a connection that was never established, or one that
/// died, is **"not yet"** — a node restart takes seconds and the correct
/// response is to try again, not to latch the failure as a fact about the
/// chain for the life of the process.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// Total attempts, including the first. `1` = no retry.
    pub attempts: u32,
    /// Delay before the 2nd attempt; doubles each time.
    pub base: Duration,
    /// Ceiling for the doubling.
    pub max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            attempts: 3,
            base: Duration::from_millis(250),
            max: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// One attempt, no backoff — for reads where the caller already has its own
    /// cadence and a retry would only stack latency.
    pub fn once() -> Self {
        RetryPolicy {
            attempts: 1,
            base: Duration::ZERO,
            max: Duration::ZERO,
        }
    }

    /// The delay *before* attempt `n` (1-based; attempt 1 has none).
    fn delay_before(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let shift = attempt - 2;
        let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
        self.base.saturating_mul(factor).min(self.max)
    }
}

/// Run a fallible remote read, retrying transient failures with exponential
/// backoff, and return [`Strict::Unknown`] only when every attempt failed.
///
/// This is the shared helper for the #105/#111 shape — **do not** write a
/// per-site retry loop; the per-site variants were exactly what drifted.
///
/// `site` is the log/`Unknown` label (e.g. `"chain.feed"`). The closure must be
/// idempotent and side-effect free: it may run up to `policy.attempts` times.
pub async fn retry_read<T, F, Fut>(site: &str, policy: RetryPolicy, mut op: F) -> Strict<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let attempts = policy.attempts.max(1);
    let mut last = String::from("no attempt was made");
    for attempt in 1..=attempts {
        let delay = policy.delay_before(attempt);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        match op().await {
            Ok(v) => return Strict::Known(v),
            Err(e) => {
                if attempt < attempts {
                    log::debug!(
                        "[strict] {site}: attempt {attempt}/{attempts} failed — {e}; retrying (not yet a failure)"
                    );
                }
                last = e;
            }
        }
    }
    Strict::unknown(site, last)
}

/// Blocking twin of [`retry_read`], for the small sync call sites (the provider
/// heartbeat's `state_getStorage` probe) that cannot be made `async` without
/// restructuring their HTTP handler.
pub fn retry_read_blocking<T, F>(site: &str, policy: RetryPolicy, mut op: F) -> Strict<T>
where
    F: FnMut() -> Result<T, String>,
{
    let attempts = policy.attempts.max(1);
    let mut last = String::from("no attempt was made");
    for attempt in 1..=attempts {
        let delay = policy.delay_before(attempt);
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        match op() {
            Ok(v) => return Strict::Known(v),
            Err(e) => {
                if attempt < attempts {
                    log::debug!(
                        "[strict] {site}: attempt {attempt}/{attempts} failed — {e}; retrying (not yet a failure)"
                    );
                }
                last = e;
            }
        }
    }
    Strict::unknown(site, last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn unknown_keeps_the_reason_and_is_not_a_value() {
        let s: Strict<u64> = Strict::unknown("test.site", "node unreachable");
        assert!(s.is_unknown());
        assert_eq!(s.reason(), Some("node unreachable"));
        // The only way out is explicit.
        assert_eq!(s.into_option(), None);
    }

    #[test]
    fn known_passes_through_map_and_and_then() {
        let s = Strict::known(2u64).map(|v| v * 3).and_then(Strict::known);
        assert_eq!(s, Strict::Known(6));
    }

    #[test]
    fn unknown_passes_through_map_without_inventing_a_value() {
        let s: Strict<u64> = Strict::unknown("test.map", "dead socket");
        let mapped = s.map(|v| v * 3);
        assert_eq!(mapped, Strict::Unknown("dead socket".to_string()));
    }

    #[test]
    fn default_is_unknown_never_a_fabricated_value() {
        let s: Strict<u64> = Strict::default();
        assert!(s.is_unknown());
    }

    #[test]
    fn from_result_keeps_the_error_text() {
        let s: Strict<u64> = Strict::from_result("test.res", Err("boom".to_string()));
        assert_eq!(s.reason(), Some("boom"));
        let ok: Strict<u64> = Strict::from_result("test.res", Ok(7));
        assert_eq!(ok, Strict::Known(7));
    }

    #[test]
    fn from_option_none_is_unknown_not_zero() {
        let s: Strict<u64> = Strict::from_option("test.opt", None, "key unreadable");
        assert_eq!(s.reason(), Some("key unreadable"));
    }

    #[test]
    fn display_only_placeholder_is_explicit() {
        assert_eq!(
            Strict::<u64>::unknown("t", "x").or_default_for_display_only(),
            0
        );
        assert_eq!(
            Strict::<String>::unknown("t", "x").or_for_display_only("Unknown".into()),
            "Unknown"
        );
    }

    #[tokio::test]
    async fn retry_read_recovers_a_dead_connection_instead_of_latching() {
        // The #105/#111 shape: the first two reads fail because the node is not
        // up yet; the third succeeds. The result must be Known, not a latched
        // failure.
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out: Strict<u64> = retry_read(
            "test.retry",
            RetryPolicy {
                attempts: 3,
                base: Duration::from_millis(1),
                max: Duration::from_millis(2),
            },
            move || {
                let c = c.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst);
                    if n < 2 {
                        Err("connection closed".to_string())
                    } else {
                        Ok(42)
                    }
                }
            },
        )
        .await;
        assert_eq!(out, Strict::Known(42));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_read_gives_up_as_unknown_and_names_the_last_error() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out: Strict<u64> = retry_read(
            "test.retry.fail",
            RetryPolicy {
                attempts: 2,
                base: Duration::from_millis(1),
                max: Duration::from_millis(1),
            },
            move || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err("no route to host".to_string())
                }
            },
        )
        .await;
        assert!(out.is_unknown());
        assert_eq!(out.reason(), Some("no route to host"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retry_policy_backoff_doubles_and_caps() {
        let p = RetryPolicy {
            attempts: 5,
            base: Duration::from_millis(100),
            max: Duration::from_millis(250),
        };
        assert_eq!(p.delay_before(1), Duration::ZERO);
        assert_eq!(p.delay_before(2), Duration::from_millis(100));
        assert_eq!(p.delay_before(3), Duration::from_millis(200));
        assert_eq!(p.delay_before(4), Duration::from_millis(250));
    }

    #[test]
    fn retry_read_blocking_retries_then_succeeds() {
        let mut n = 0;
        let out: Strict<u32> = retry_read_blocking(
            "test.blocking",
            RetryPolicy {
                attempts: 3,
                base: Duration::from_millis(1),
                max: Duration::from_millis(1),
            },
            || {
                n += 1;
                if n < 2 {
                    Err("dead socket".to_string())
                } else {
                    Ok(9)
                }
            },
        );
        assert_eq!(out, Strict::Known(9));
    }
}
