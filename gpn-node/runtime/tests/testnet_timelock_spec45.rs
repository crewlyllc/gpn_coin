//! Spec 45 — the testnet governance timelock is 10 blocks; **mainnet stays 3 days**.
//!
//! `GovernanceTimelockPeriod` is a compiled runtime constant, so shortening it
//! for testnet iteration must not be able to leak into a mainnet build. It is
//! gated on the `testnet-fast-timelock` cargo feature:
//!
//!   mainnet / default build   cargo build -p gpn-runtime --release
//!                             → 3 * DAYS == 17 280 blocks
//!   testnet build             cargo build -p gpn-runtime --release \
//!                                 --features testnet-fast-timelock
//!                             → 10 blocks
//!
//! The tests below are the guard: with the feature off (the default, and the
//! only way mainnet is built) the timelock *must* be exactly 17 280 blocks.
//! A stray `--features testnet-fast-timelock` in a mainnet pipeline therefore
//! fails the suite instead of silently shipping a 10-block upgrade door.

use gpn_runtime::{GovernanceProposalExpiry, GovernanceTimelockPeriod, DAYS};

/// The default (feature-off) build is the mainnet build. It must never inherit
/// the testnet value — this is the assertion that makes that mechanical.
#[cfg(not(feature = "testnet-fast-timelock"))]
#[test]
fn mainnet_build_keeps_the_three_day_timelock() {
    assert_eq!(DAYS, 5_760, "15 s blocks: one day is 5 760 blocks");
    assert_eq!(
        GovernanceTimelockPeriod::get(),
        3 * DAYS,
        "feature off ⇒ 3 days, unconditionally"
    );
    assert_eq!(
        GovernanceTimelockPeriod::get(),
        17_280,
        "a mainnet build must not inherit the 10-block testnet timelock"
    );
}

/// With the feature on, the same constant is 10 blocks (~2–2.5 min).
#[cfg(feature = "testnet-fast-timelock")]
#[test]
fn testnet_build_shortens_the_timelock_to_ten_blocks() {
    assert_eq!(GovernanceTimelockPeriod::get(), 10);
}

/// `ProposalExpiry` (14 days) is deliberately untouched: every proposal must
/// outlive its own timelock, or it could expire before it is executable.
#[test]
fn proposal_expiry_outlives_the_timelock() {
    assert_eq!(
        GovernanceProposalExpiry::get(),
        14 * DAYS,
        "14 days at 15 s blocks — unchanged by spec 45"
    );
    assert!(
        GovernanceProposalExpiry::get() > GovernanceTimelockPeriod::get(),
        "expiry {} must exceed the timelock {}",
        GovernanceProposalExpiry::get(),
        GovernanceTimelockPeriod::get()
    );
}
