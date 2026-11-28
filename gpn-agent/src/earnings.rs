//! "Earnings today" (item #212) — a chain-observed figure that states its
//! **source** and its **window**, and says so when it cannot compute one.
//!
//! The dashboard used to show `null` with "not tracked locally", which was
//! honest but useless: the operator asked "how much did this box earn today?"
//! and nothing could answer.
//!
//! How the number is built: the chain's own `System::Account` free balance at
//! the first sample after 00:00 UTC is the baseline, and the figure is
//! `balance_now − baseline`. Every credit in that window (block rewards and
//! settled job payments) is included exactly once, because the balance is
//! cumulative: missed samples cannot lose a reward, and a restart can neither
//! reset nor double-count it (the baseline is persisted).
//!
//! Why not read the balance *at* the midnight block: this node prunes state
//! (measured 2026-11-20: ~4,350 blocks ≈ 10 min retained on the testnet's
//! 132 ms blocks), so a day-start block has no readable state.
//!
//! Two cases where a balance delta stops being a reward figure are named, not
//! hidden:
//! - the account **sent** a transaction in the window (a nonce change);
//! - the balance **fell** (a spend larger than the credits, or a slash).
//!
//! Both return an error, which the panel renders as a dash with the reason.
//! It never subtracts spends and calls the remainder "earnings".
//!
//! **What is deliberately not here (row 294/295):** a second coin's rewards and
//! Solana staking rewards. Those are paid to the operator's own addresses, not
//! to this node's GPN account, so the chain free-balance delta above cannot see
//! them — by construction, not by a filter. The dashboard shows them in a
//! separate "Other earnings" panel fed by `secondary-status.json` and the stake
//! account, and nothing in this module or the airdrop export reads either.

use std::path::{Path, PathBuf};

/// The window: the current UTC day. The node has no timezone database, and a
/// guessed local midnight is worse than an explicitly stated UTC one, because
/// the panel says which it is.
pub fn utc_day_start_ms(now_ms: u64) -> u64 {
    now_ms / 86_400_000 * 86_400_000
}

/// What the panel prints as the window, next to the number.
pub const WINDOW_LABEL: &str = "00:00 UTC → now";

/// What the number measures, next to the number.
pub const SOURCE_LABEL: &str = "chain free-balance growth (block rewards + settled job payments)";

/// The day's baseline, persisted so a restart cannot reset the figure.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DayLedger {
    /// UTC-day start this baseline belongs to (ms).
    pub day_start_ms: u64,
    /// Free balance (GPN) at the first sample of the day.
    pub baseline_gpn: f64,
    /// Account nonce at that sample; a later nonce means money was sent.
    pub baseline_nonce: u64,
    /// When the baseline was taken (ms) — the panel needs it: the day starts at
    /// 00:00 UTC but the baseline is the first sample after that.
    pub baseline_at_ms: u64,
    /// Last sample that has been folded into this ledger.
    pub last_sample_ms: u64,
}

/// Fold one chain observation into the day's ledger.
///
/// Returns the ledger to persist and `(figure, note)`. `figure = None` means
/// "show a dash", never zero — the note carries the reason.
pub fn fold_sample(
    prev: Option<DayLedger>,
    now_ms: u64,
    balance_gpn: f64,
    nonce: u64,
) -> (DayLedger, Option<f64>, String) {
    let day = utc_day_start_ms(now_ms);
    let prev = match prev {
        Some(p) if p.day_start_ms == day => p,
        _ => {
            let fresh = DayLedger {
                day_start_ms: day,
                baseline_gpn: balance_gpn,
                baseline_nonce: nonce,
                baseline_at_ms: now_ms,
                last_sample_ms: now_ms,
            };
            return (
                fresh,
                Some(0.0),
                format!("{SOURCE_LABEL} · {WINDOW_LABEL} (baseline taken now)"),
            );
        }
    };

    let mut out = prev.clone();
    out.last_sample_ms = now_ms;

    if nonce > prev.baseline_nonce {
        return (
            out,
            None,
            format!(
                "no figure: the account sent {} transaction(s) today, so balance growth is not \
                 reward-only — see the explorer for the per-transaction view",
                nonce - prev.baseline_nonce
            ),
        );
    }
    let delta = balance_gpn - prev.baseline_gpn;
    if delta < -0.000_000_5 {
        return (
            out,
            None,
            format!(
                "no figure: the balance fell by {:.6} GPN today (spend or slash), so it is not a \
                 reward figure",
                -delta
            ),
        );
    }
    (
        out,
        Some(delta.max(0.0)),
        format!("{SOURCE_LABEL} · {WINDOW_LABEL}"),
    )
}

/// `<base>/earnings-today.json` — one small file beside the node's other state.
pub fn ledger_path(base: &Path) -> PathBuf {
    base.join("earnings-today.json")
}

pub fn load(base: &Path) -> Option<DayLedger> {
    let text = std::fs::read_to_string(ledger_path(base)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist the ledger. A failure is not fatal: the figure is still returned for
/// this sample, and the next sample recomputes from the chain.
pub fn store(base: &Path, ledger: &DayLedger) {
    let path = ledger_path(base);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(ledger) {
        let _ = std::fs::write(path, text);
    }
}

/// Wall clock in milliseconds since the unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sample the ledger for a balance the caller read.
pub fn sample(base: &Path, now: u64, balance_gpn: f64, nonce: u64) -> (Option<f64>, String) {
    let prev = load(base);
    let last_written = prev.as_ref().map(|p| p.last_sample_ms).unwrap_or(0);
    let fresh_day = prev
        .as_ref()
        .is_none_or(|p| p.day_start_ms != utc_day_start_ms(now));
    let (ledger, figure, note) = fold_sample(prev, now, balance_gpn, nonce);
    // Only write when the day rolled over or the sample interval has passed, so
    // a page polling every second does not rewrite the file every second.
    let due = fresh_day || now.saturating_sub(last_written) >= 5_000;
    if due {
        store(base, &ledger);
    }
    (figure, note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(day: u64, base: f64, nonce: u64) -> DayLedger {
        DayLedger {
            day_start_ms: day,
            baseline_gpn: base,
            baseline_nonce: nonce,
            baseline_at_ms: day,
            last_sample_ms: day,
        }
    }

    #[test]
    fn the_window_is_the_utc_day_and_says_so() {
        let now = 1_795_133_760_000u64; // 2026-11-20T00:16:00Z
        assert_eq!(utc_day_start_ms(now), 1_795_132_800_000);
        assert_eq!(utc_day_start_ms(1_795_132_800_000), 1_795_132_800_000);
        assert!(WINDOW_LABEL.contains("UTC"), "{WINDOW_LABEL}");
    }

    #[test]
    fn a_new_day_takes_a_baseline_and_starts_at_zero() {
        let day = 1_795_132_800_000;
        let (l, fig, note) = fold_sample(None, day + 60_000, 10.0, 3);
        assert_eq!(fig, Some(0.0));
        assert_eq!(l.baseline_gpn, 10.0);
        assert_eq!(l.baseline_nonce, 3);
        assert!(note.contains("baseline taken now"), "{note}");
    }

    #[test]
    fn growth_without_a_spend_is_the_reward_figure() {
        let day = 1_795_132_800_000;
        let prev = ledger(day, 100.0, 3);
        let (l, fig, note) = fold_sample(Some(prev), day + 3_600_000, 101.25, 3);
        assert_eq!(fig, Some(1.25));
        assert_eq!(l.baseline_gpn, 100.0);
        assert_eq!(l.last_sample_ms, day + 3_600_000);
        assert!(note.contains("block rewards"), "{note}");
    }

    #[test]
    fn a_restart_mid_window_neither_resets_nor_double_counts() {
        let day = 1_795_132_800_000;
        let prev = ledger(day, 50.0, 1);
        // Same day, reloaded from disk: baseline preserved, figure is the delta.
        let (_l, fig, _n) = fold_sample(Some(prev.clone()), day + 7_200_000, 52.5, 1);
        assert_eq!(fig, Some(2.5));
        // A second sample at the same balance does not add anything again.
        let (_l2, fig2, _n2) = fold_sample(Some(prev), day + 7_260_000, 52.5, 1);
        assert_eq!(fig2, Some(2.5));
    }

    #[test]
    fn a_new_day_resets_the_baseline() {
        let day = 1_795_132_800_000;
        let prev = ledger(day, 50.0, 1);
        let next_day = day + 86_400_000;
        let (l, fig, note) = fold_sample(Some(prev), next_day + 1_000, 90.0, 1);
        assert_eq!(fig, Some(0.0));
        assert_eq!(l.baseline_gpn, 90.0);
        assert_eq!(l.day_start_ms, next_day);
        assert!(note.contains("baseline taken now"));
    }

    #[test]
    fn an_outgoing_transaction_is_a_named_error_not_a_wrong_number() {
        let day = 1_795_132_800_000;
        let prev = ledger(day, 10.0, 7);
        let (_l, fig, note) = fold_sample(Some(prev), day + 60_000, 15.0, 9);
        assert_eq!(fig, None);
        assert!(note.contains("sent 2 transaction(s)"), "{note}");
    }

    #[test]
    fn a_falling_balance_is_a_named_error() {
        let day = 1_795_132_800_000;
        let prev = ledger(day, 10.0, 4);
        let (_l, fig, note) = fold_sample(Some(prev), day + 60_000, 2.0, 4);
        assert_eq!(fig, None);
        assert!(note.contains("fell by 8.000000"), "{note}");
    }

    #[test]
    fn the_ledger_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("gpn-earnings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let l = ledger(1_795_132_800_000, 1.5, 2);
        store(&dir, &l);
        assert_eq!(load(&dir).as_ref(), Some(&l));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
