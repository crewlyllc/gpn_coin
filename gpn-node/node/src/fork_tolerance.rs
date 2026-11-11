//! Fork tolerance — **divergence is not misbehaviour** (GPN #158).
//!
//! Three forks in five days (2026-11-07, 11-09) each ended the same way: the two
//! sides saw each other's best block, one side's sync layer called it a bad
//! block, the peer was dropped/penalised, and a node that only needed to be
//! taught the canonical chain became an outsider. A peer whose **best block
//! differs from ours at the same height** has not violated anything:
//!
//! * it is *behind* on a stale branch, or
//! * it is *ahead* on a side branch that we will outrun by weight, or
//! * it is on a fork it should reorg off — which only happens if we stay
//!   connected long enough to serve it canonical blocks.
//!
//! Bans are for protocol violations: undecodable bytes, a failed signature, a
//! forged justification, a genesis mismatch (a *different network* is a real
//! mismatch, not a fork). `sc-network-sync` cannot always tell the two apart —
//! a block that fails verification is reported as `BAD_BLOCK` (`-(1 << 29)`)
//! and roughly three of those put a peer below the ban threshold — so this
//! module owns the local half of the policy:
//!
//! 1. `classify` decides, from the two tips only, whether there is anything to
//!    say at all (same height + different hash = divergence; anything else is
//!    not a verdict).
//! 2. `divergence_credit` gives reputation back for that case, **never above
//!    zero** and never more than the peer had lost, so a genuinely bad peer is
//!    not laundered — only a peer that did nothing wrong is pulled back out of
//!    the ban band, at most once per window per peer.
//!
//! We never disconnect on divergence and we never serve a forked block: the
//! node keeps its own chain selection (`design-fork-choice-heaviest.md`) and
//! answers block requests from its canonical database either way.
//!
//! Design: `shared_workspace/artifacts/project7/design-peer-priority-checkpoints.md`
//! §3/§4 and `design-self-heal-divergence.md`.

use std::collections::HashMap;

/// How often the scan runs. 30 s matches `gpn-peer-watch`: fast enough to
/// repair before a third `BAD_BLOCK` lands, cheap enough to ignore.
pub const SCAN_INTERVAL_SECS: u64 = 30;

/// Reputation given back for a detected divergence: half of one `BAD_BLOCK`
/// (`-(1 << 29)`), i.e. one repair undoes at most half of one bad-block report.
/// Kept deliberately smaller than a violation costs so divergence can never
/// out-earn misbehaviour.
pub const DIVERGENCE_CREDIT: i32 = 1 << 28;

/// At most one repair per peer per window, so a peer parked on a fork cannot
/// use divergence as a reputation pump.
pub const DIVERGENCE_WINDOW_SECS: u64 = 600;

/// What the two tips say about a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same height, same hash — the normal case, nothing to do.
    SameChain,
    /// Their tip is lower: they are behind us and will sync from us.
    PeerBehind,
    /// Their tip is higher: we are behind and will sync from them.
    PeerAhead,
    /// Same height, different hash: two chains at the same tip. This is the
    /// observed fork shape, and it is **not** a reason to ban.
    Diverged,
}

/// Classify from the two tips alone.
///
/// Deliberately a *same-height* comparison: a peer one block behind is not
/// diverged, and answering "is their hash on our chain?" for every height would
/// mean a database read per peer per tick for a case that needs no action. The
/// shape that stranded nodes was always "both nodes at #N, different hashes".
pub fn classify<H: PartialEq>(
    our_number: u32,
    our_hash: &H,
    their_number: u32,
    their_hash: &H,
) -> Verdict {
    if their_number < our_number {
        return Verdict::PeerBehind;
    }
    if their_number > our_number {
        return Verdict::PeerAhead;
    }
    if our_hash == their_hash {
        Verdict::SameChain
    } else {
        Verdict::Diverged
    }
}

/// Why a peer is being penalised. Only one of these is a ban.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Misbehaviour {
    /// Undecodable message, bad signature, forged proof, genesis mismatch.
    ProtocolViolation,
    /// A different best block at the same height — see the module docs.
    ChainDivergence,
}

/// The policy, in one place so it can be tested: divergence never bans.
pub fn bans_peer(m: Misbehaviour) -> bool {
    matches!(m, Misbehaviour::ProtocolViolation)
}

/// Reputation to give back after a divergence. Never positive (a peer's
/// reputation is not a reward), never more than it lost, and zero when the peer
/// is already at or above zero.
pub fn divergence_credit(current: i32) -> i32 {
    if current >= 0 {
        return 0;
    }
    // `saturating_neg` because `i32::MIN` has no positive counterpart.
    DIVERGENCE_CREDIT.min(current.saturating_neg())
}

/// Per-peer rate limit for the repair, with an injected clock so it is testable.
#[derive(Debug, Default)]
pub struct DivergenceGuard {
    last_repair: HashMap<String, u64>,
}

impl DivergenceGuard {
    /// True when this peer may be repaired now (and records the attempt).
    /// A caller that decides not to act after `true` must not call again in the
    /// same window, which is the point: the window is consumed by *detection*,
    /// not by a successful write.
    pub fn should_repair(&mut self, peer: &str, now_secs: u64) -> bool {
        match self.last_repair.get(peer) {
            Some(&last) if now_secs.saturating_sub(last) < DIVERGENCE_WINDOW_SECS => false,
            _ => {
                self.last_repair.insert(peer.to_string(), now_secs);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u32 = 0xaaaa_aaaa;
    const B: u32 = 0xbbbb_bbbb;

    #[test]
    fn same_tip_is_same_chain() {
        assert_eq!(classify(100, &A, 100, &A), Verdict::SameChain);
    }

    #[test]
    fn a_lower_tip_is_behind_not_a_fork() {
        assert_eq!(classify(100, &A, 99, &B), Verdict::PeerBehind);
    }

    #[test]
    fn a_higher_tip_is_ahead_not_a_fork() {
        assert_eq!(classify(100, &A, 101, &B), Verdict::PeerAhead);
    }

    #[test]
    fn same_height_different_hash_is_divergence() {
        // The exact shape that stranded box 1: both at #N, different hashes.
        assert_eq!(classify(117_000, &A, 117_000, &B), Verdict::Diverged);
    }

    #[test]
    fn divergence_never_bans_and_a_protocol_violation_does() {
        assert!(!bans_peer(Misbehaviour::ChainDivergence));
        assert!(bans_peer(Misbehaviour::ProtocolViolation));
    }

    #[test]
    fn credit_is_bounded_by_the_loss_and_never_rewards() {
        // Already fine: nothing to give back.
        assert_eq!(divergence_credit(0), 0);
        assert_eq!(divergence_credit(500), 0);
        // A small loss is fully repaid, not overshot.
        assert_eq!(divergence_credit(-10), 10);
        // A large loss is repaid by at most one credit unit.
        assert_eq!(divergence_credit(-(1 << 29)), DIVERGENCE_CREDIT);
        assert_eq!(divergence_credit(i32::MIN), DIVERGENCE_CREDIT);
    }

    #[test]
    fn one_repair_per_window_per_peer() {
        let mut g = DivergenceGuard::default();
        assert!(g.should_repair("12D3KooW-a", 1_000));
        assert!(!g.should_repair("12D3KooW-a", 1_001));
        assert!(!g.should_repair("12D3KooW-a", 1_000 + DIVERGENCE_WINDOW_SECS - 1));
        assert!(g.should_repair("12D3KooW-a", 1_000 + DIVERGENCE_WINDOW_SECS));
        // A different peer has its own window.
        assert!(g.should_repair("12D3KooW-b", 1_000));
    }

    #[test]
    fn a_backwards_clock_does_not_reopen_the_window() {
        let mut g = DivergenceGuard::default();
        assert!(g.should_repair("p", 10_000));
        // saturating_sub gives 0 for a clock that jumped back: still the same window.
        assert!(!g.should_repair("p", 9_000));
    }
}
