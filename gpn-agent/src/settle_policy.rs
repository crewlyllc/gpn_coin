//! Settlement policy — **which** payment path a request takes.
//!
//! Two paths exist for a chat/generation job:
//!
//! * **on-chain settle** — `create_job` … `complete_job`, i.e. per-job escrow.
//!   Costs 2 extrinsics from the access point (plus 3 from the provider) and a
//!   block wait for the *settlement*, but spec-43's optimistic claim means the
//!   AI answer is returned as soon as the provider produces it, so the block
//!   wait never delays the caller.
//! * **channel** — an off-chain, mutually-signed balance between *this access
//!   point* and *one provider*, opened once and closed occasionally. Zero
//!   extrinsics per job, but the AP must hold a working deposit with that
//!   provider and the money moves only at close.
//!
//! The policy is **value based**: a job whose price is small compared with the
//! cost of settling it on-chain is not worth 5 extrinsics, so it batches over a
//! channel; an expensive job is worth the settlement and gets its own on-chain
//! receipt. There is deliberately no client-identity input here — we do not
//! guess who is calling, only what the job is worth.
//!
//! Note on wording (docs/site): a channel is between an **AP and a provider**,
//! not between an end user and the AP. Batching is therefore the *AP operator's*
//! decision; there are no per-user tiers.

/// GPN has 12 decimals: 1 GPN = 1e12 base units.
pub const GPN_UNITS: u128 = 1_000_000_000_000;

/// Measured on chain 2026-10-22 (blocks 175411 / 175413 of the live testnet) by
/// reading `TransactionPayment::TransactionFeePaid` for one full per-job
/// settlement:
///
/// | extrinsic                | payer    | actual_fee (base units) |
/// |--------------------------|----------|-------------------------|
/// | `create_job`             | AP       | 108_157_199             |
/// | `accept_job`             | provider | 108_157_116             |
/// | `start_job`              | provider | 108_157_116             |
/// | provider result → complete | provider | 108_157_280           |
/// | AP `complete_job` (escrow release) | AP | 108_157_116          |
///
/// The AP's own per-job settlement cost is therefore
/// `create_job` + `complete_job` = **216_314_315 base units ≈ 0.000216 GPN**.
/// (The provider spends a similar 3× on accept/start/submit; that is *its*
/// overhead, not the buyer's, and it is why a channel still pays off at volume.)
pub const MEASURED_CREATE_PLUS_COMPLETE_FEE: u128 = 216_314_315;

/// How many times the measured settlement cost a job must be worth before we
/// are willing to settle it on chain. 10× keeps the fee below ~10 % of the
/// value transferred — above that, paying 5 extrinsics to move a rounding
/// error is just donating fees to the validator set.
pub const SETTLE_COST_MULTIPLE: u128 = 10;

/// Default threshold: 2_163_143_150 base units ≈ 0.00216 GPN.
///
/// Sanity check against live pricing (2026-10-22): a `max_tokens: 8` chat
/// quotes ~3.2e7 base units (→ channel) while a `max_tokens: 2000` chat quotes
/// ~5.07e9 base units (→ on-chain settle). The threshold sits between them,
/// which is exactly the split we want.
pub const DEFAULT_SETTLE_MIN_GPN: u128 = MEASURED_CREATE_PLUS_COMPLETE_FEE * SETTLE_COST_MULTIPLE;

/// Operator-facing settlement mode (`GPN_ESCROW_MODE`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EscrowMode {
    /// Value-based routing (the default since 2026-10-22).
    Settle,
    /// Always batch over a channel when one is usable; on-chain only if not.
    Channel,
    /// Never use channels.
    Off,
}

impl EscrowMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "settle" | "auto" | "" => Some(EscrowMode::Settle),
            "channel" | "offchain" | "off-chain" => Some(EscrowMode::Channel),
            "onchain" | "on-chain" | "off" | "disabled" => Some(EscrowMode::Off),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            EscrowMode::Settle => "settle",
            EscrowMode::Channel => "channel",
            EscrowMode::Off => "off",
        }
    }
}

/// The path a single request takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// Free/unbilled direct inference (`mode: "passthrough"`).
    Passthrough,
    /// Off-chain channel settlement (batched).
    Channel,
    /// Per-job on-chain escrow.
    OnChain,
}

impl Route {
    pub fn as_str(&self) -> &'static str {
        match self {
            Route::Passthrough => "passthrough",
            Route::Channel => "channel",
            Route::OnChain => "onchain",
        }
    }
}

/// How a routing decision was reached (for the per-request debug log and the
/// tests' decision table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    ExplicitMode,
    AboveThreshold,
    BelowThreshold,
    NoChannel,
    Passthrough,
}

impl Reason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Reason::ExplicitMode => "explicit mode",
            Reason::AboveThreshold => "value >= settle threshold",
            Reason::BelowThreshold => "value < settle threshold",
            Reason::NoChannel => "no channel available",
            Reason::Passthrough => "passthrough (unbilled)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettlePolicy {
    pub mode: EscrowMode,
    pub channel_available: bool,
    /// Jobs worth at least this much (base units) settle on chain.
    /// `0` disables the threshold (everything on chain).
    pub min_gpn: u128,
}

impl Default for SettlePolicy {
    fn default() -> Self {
        Self {
            mode: EscrowMode::Settle,
            channel_available: true,
            min_gpn: DEFAULT_SETTLE_MIN_GPN,
        }
    }
}

impl SettlePolicy {
    /// Build from the process environment.
    ///
    /// `GPN_ESCROW_MODE` — `settle` (default) | `channel` | `onchain`/`off`.
    /// `GPN_SETTLE_MIN_GPN` — threshold in **base units** (not whole GPN; the
    ///                        rest of the chain config is base units too).
    pub fn from_env(channel_available: bool) -> Self {
        Self::from_lookup(channel_available, |k| std::env::var(k).ok())
    }

    /// Testable constructor: `lookup` returns the raw env value for a key.
    pub fn from_lookup<F>(channel_available: bool, lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let mode = lookup("GPN_ESCROW_MODE")
            .as_deref()
            .and_then(EscrowMode::parse)
            .unwrap_or(EscrowMode::Settle);
        let min_gpn = match lookup("GPN_SETTLE_MIN_GPN").and_then(|s| s.trim().parse::<u128>().ok())
        {
            Some(v) => v,
            None => DEFAULT_SETTLE_MIN_GPN,
        };
        Self {
            mode,
            channel_available,
            min_gpn,
        }
    }

    /// Routing decision. `explicit` is the request's `mode` field, if any —
    /// an explicit request always wins over the operator policy.
    pub fn decide(&self, explicit: Option<&str>, job_value: u128) -> (Route, Reason) {
        if let Some(m) = explicit.map(|s| s.trim().to_ascii_lowercase()) {
            match m.as_str() {
                "passthrough" => return (Route::Passthrough, Reason::Passthrough),
                "settle" | "onchain" | "on-chain" => return (Route::OnChain, Reason::ExplicitMode),
                "channel" | "offchain" | "off-chain" => {
                    // An explicit channel request is a client preference, not an
                    // override of the operator's kill switch.
                    if self.channel_available && self.mode != EscrowMode::Off {
                        return (Route::Channel, Reason::ExplicitMode);
                    }
                    return (Route::OnChain, Reason::NoChannel);
                }
                // Unknown explicit value: fall through to policy rather than
                // 400 — it was probably a client passing a UI string.
                _ => {}
            }
        }
        match self.mode {
            EscrowMode::Off => (Route::OnChain, Reason::NoChannel),
            EscrowMode::Channel => {
                if self.channel_available {
                    (Route::Channel, Reason::ExplicitMode)
                } else {
                    (Route::OnChain, Reason::NoChannel)
                }
            }
            EscrowMode::Settle => {
                if self.min_gpn == 0 || job_value >= self.min_gpn {
                    return (Route::OnChain, Reason::AboveThreshold);
                }
                if self.channel_available {
                    (Route::Channel, Reason::BelowThreshold)
                } else {
                    (Route::OnChain, Reason::NoChannel)
                }
            }
        }
    }

    /// One-line startup banner (required by the operator runbook).
    pub fn banner(&self) -> String {
        format!(
            "[pricing] settle threshold={} base units ({} GPN) = {}x measured per-job settle cost \
             ({} base units); mode={}, channel={}",
            self.min_gpn,
            self.min_gpn as f64 / GPN_UNITS as f64,
            SETTLE_COST_MULTIPLE,
            MEASURED_CREATE_PLUS_COMPLETE_FEE,
            self.mode.as_str(),
            if self.channel_available { "on" } else { "off" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> SettlePolicy {
        SettlePolicy::default()
    }

    #[test]
    fn threshold_is_ten_times_measured_fee() {
        assert_eq!(
            DEFAULT_SETTLE_MIN_GPN,
            MEASURED_CREATE_PLUS_COMPLETE_FEE * 10
        );
        // 0.00216 GPN, i.e. small but far above a fee.
        assert_eq!(DEFAULT_SETTLE_MIN_GPN, 2_163_143_150);
    }

    #[test]
    fn expensive_job_settles_on_chain() {
        // max_tokens: 2000 quotes ~5.07e9 base units on the live testnet.
        assert_eq!(
            pol().decide(None, 5_072_750_734),
            (Route::OnChain, Reason::AboveThreshold)
        );
    }

    #[test]
    fn cheap_job_batches_over_channel() {
        // max_tokens: 8 quotes ~3.2e7 base units.
        assert_eq!(
            pol().decide(None, 32_010_000),
            (Route::Channel, Reason::BelowThreshold)
        );
    }

    #[test]
    fn boundary_is_inclusive_on_chain() {
        let p = pol();
        assert_eq!(p.decide(None, p.min_gpn).0, Route::OnChain);
        assert_eq!(p.decide(None, p.min_gpn - 1).0, Route::Channel);
    }

    #[test]
    fn explicit_mode_beats_policy() {
        // Cheap but explicitly on-chain.
        assert_eq!(
            pol().decide(Some("settle"), 1),
            (Route::OnChain, Reason::ExplicitMode)
        );
        // Expensive but explicitly channel.
        assert_eq!(
            pol().decide(Some("channel"), u128::MAX / 2),
            (Route::Channel, Reason::ExplicitMode)
        );
        assert_eq!(
            pol().decide(Some("passthrough"), 0),
            (Route::Passthrough, Reason::Passthrough)
        );
        // Case/whitespace tolerant.
        assert_eq!(pol().decide(Some(" ONCHAIN "), 1).0, Route::OnChain);
    }

    #[test]
    fn unknown_explicit_mode_falls_back_to_policy() {
        assert_eq!(pol().decide(Some("fast"), 32_010_000).0, Route::Channel);
        assert_eq!(pol().decide(Some("fast"), 5_072_750_734).0, Route::OnChain);
    }

    #[test]
    fn no_channel_capable_provider_always_settles_on_chain() {
        let mut p = pol();
        p.channel_available = false;
        assert_eq!(p.decide(None, 1), (Route::OnChain, Reason::NoChannel));
        // An explicit channel request cannot be honoured either.
        assert_eq!(
            p.decide(Some("channel"), 1),
            (Route::OnChain, Reason::NoChannel)
        );
    }

    #[test]
    fn operator_channel_mode_batches_everything() {
        let mut p = pol();
        p.mode = EscrowMode::Channel;
        assert_eq!(p.decide(None, u128::MAX / 2).0, Route::Channel);
        // ...but an explicit settle still wins.
        assert_eq!(p.decide(Some("settle"), 1).0, Route::OnChain);
    }

    #[test]
    fn operator_off_mode_never_uses_channels() {
        let mut p = pol();
        p.mode = EscrowMode::Off;
        assert_eq!(p.decide(None, 1).0, Route::OnChain);
        assert_eq!(p.decide(Some("channel"), 1).0, Route::OnChain);
    }

    #[test]
    fn zero_threshold_puts_everything_on_chain() {
        let mut p = pol();
        p.min_gpn = 0;
        assert_eq!(p.decide(None, 0).0, Route::OnChain);
    }

    #[test]
    fn env_parsing() {
        let env = |k: &str| match k {
            "GPN_ESCROW_MODE" => Some("channel".to_string()),
            "GPN_SETTLE_MIN_GPN" => Some("12345".to_string()),
            _ => None,
        };
        let p = SettlePolicy::from_lookup(true, env);
        assert_eq!(p.mode, EscrowMode::Channel);
        assert_eq!(p.min_gpn, 12_345);

        // Missing / garbage env → documented defaults.
        let p = SettlePolicy::from_lookup(true, |_| None);
        assert_eq!(p.mode, EscrowMode::Settle);
        assert_eq!(p.min_gpn, DEFAULT_SETTLE_MIN_GPN);

        let p = SettlePolicy::from_lookup(true, |k| {
            if k == "GPN_SETTLE_MIN_GPN" {
                Some("not-a-number".into())
            } else {
                Some("nonsense-mode".into())
            }
        });
        assert_eq!(p.mode, EscrowMode::Settle);
        assert_eq!(p.min_gpn, DEFAULT_SETTLE_MIN_GPN);
    }

    #[test]
    fn banner_mentions_both_numbers() {
        let b = pol().banner();
        assert!(b.contains("2163143150"), "{b}");
        assert!(b.contains("216314315"), "{b}");
        assert!(b.contains("10x"), "{b}");
    }
}
