//! Heaviest-chain fork choice (owner decision 2026-11-16).
//!
//! ## The failure this closes
//!
//! On 2026-11-16 box 1 was found forked at `#306,633` while the fleet was at
//! `#309,96x`. Neither `gpn_fleet` self-heal nor `gpn_snapshot heal` could see
//! it: the newest **signed** checkpoint was `#305,244`, *below* the fork. A node
//! whose fork sits above the last anchor therefore mines its own history
//! indefinitely, because every branch looks equally canonical to a fork choice
//! that only compares **height**, and the two branches were at the same height.
//!
//! The owner rejected the two obvious alternatives (2026-11-16):
//!
//! * **scheduled/hourly checkpoint signing** — that makes a publisher a
//!   dependency, and the network must not need one to agree with itself;
//! * **a witness quorum / diversity gate** — there are two public IPs today, so
//!   any diversity requirement stops mining outright.
//!
//! So fork choice is fixed instead: **a chain with greater cumulative work
//! wins**, as in Bitcoin. Authoring stays pure proof-of-work; attestation stays
//! informational. A diversity-based *sealing* gate is deferred, not designed
//! away.
//!
//! ## The three rules
//!
//! 1. **Heaviest wins.** When a peer offers a chain with the same genesis and
//!    strictly greater cumulative work, we reorg toward it. Equal work, or a
//!    peer whose work cannot be computed (a 0.3.49 peer's `gpncs/1` claim has
//!    no work field), keeps current behaviour exactly.
//! 2. **Bound the reorg.** A reorg deeper than [`MAX_REORG_DEPTH`] is *refused*,
//!    not applied: the operator gets a loud line and the snapshot/catch-up path
//!    has to do it. The constant and its reasoning are below.
//! 3. **Never seal a losing branch.** While a heavier known chain is pending or
//!    refused-as-deep, the node pauses sealing (`gpn-forked` marker), exactly
//!    like the existing 0-peer `gpn-isolated` guard.
//!
//! ## Bootstrap stance (owner constraint, 2026-11-16)
//!
//! **The Linux host is the primary chain for now.** A single-writer bootstrap is
//! safer than a two-IP network electing itself, so a branch that does not
//! contain the primary's chain must never displace it, even if some peer
//! computes a larger number. [`PRIMARY_AUTHORITATIVE`] is the documented,
//! reversible switch for that stance; it is intended to be removed when the
//! network has independent operators. When it is what decides, we log that
//! separately ([`Decision::is_primary_override`]) so the operator can see how
//! often the bootstrap rule is doing the work — it must not become an invisible
//! special case.
//!
//! ## What "work" means here
//!
//! `pallet-gpu-pow::Difficulty` stores a **target** (higher = easier), retargeted
//! every `DifficultyAdjustmentWindow` from that branch's own timestamps. The work
//! of one block is therefore `2^256 / target`, and the cumulative work of a
//! branch is the sum over its blocks — [`block_work`] / [`cumulative`]. Height is
//! never used as a proxy for work; height only enters as the *reorg depth*.
//!
//! ## Honest limitation
//!
//! The reorg depth is computed from the two announced tips (`ours - theirs`).
//! That is a **provable floor** on how many of our blocks would be reverted: the
//! common ancestor cannot be above the shorter chain's tip. When the peer's tip
//! is at or above ours the floor is zero, and the actual depth is only known to
//! the sync engine once it has the fork's headers. The bound is therefore
//! enforced on what the claim can prove; the anchor gate
//! ([`crate::sync_source`]) independently prevents auto-crossing signed
//! history. This is deliberately not presented as a full ancestor proof.

use sp_core::U256;

/// Maximum number of our own blocks a reorg may revert without operator
/// action, before the node refuses and demands the snapshot/catch-up path.
///
/// **288 blocks** is the long-standing Bitcoin convention, and it fits GPN's
/// cadence: `TargetBlockTime` is 15 s and `DifficultyAdjustmentWindow` is 45
/// blocks, so 288 is 6.4 retarget windows (~72 minutes). That is comfortably
/// longer than any ordinary partition or restart blip that *should* heal by
/// itself, and far shorter than the multi-thousand-block private history that
/// stranded box 1 (a 4,700-block unanchored tail) — which is precisely the case
/// that must stop and say why instead of silently discarding sealed blocks.
///
/// Chosen over "any depth" because an automatic deep reorg can erase hours of
/// sealed, rewarded blocks with no operator in the loop; chosen over a small
/// number (e.g. 12) because a normal network partition can exceed that and then
/// the fleet would wedge waiting for a human.
pub const MAX_REORG_DEPTH: u64 = 288;

/// The bootstrap primary (owner constraint 2026-11-16): the Linux host's libp2p
/// identity. See [`PRIMARY_AUTHORITATIVE`].
pub const PRIMARY_PEER_ID: &str = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";

/// Is the primary authoritative while the network is a single-writer bootstrap?
///
/// **Default `true`.** While `true`, a node that is itself the primary (or that
/// holds the primary's tip) will not reorg onto a heavier *non-primary* branch.
/// Set to `false` to make fork choice purely heaviest-chain — the intended state
/// once there are independent operators. This is a compile-time constant so the
/// stance is explicit, greppable, and reviewable rather than a runtime
/// special-case nobody can find.
pub const PRIMARY_AUTHORITATIVE: bool = true;

/// The proof-of-work value of one block sealed against `target`.
///
/// GPN's difficulty is a **target** (higher = easier), so a block's work is
/// `2^256 / target`. Targets are bounded by the runtime (`MinDifficulty` =
/// 2^204 .. `MaxDifficulty`), so a single block's work is at most 2^52 and the
/// sum for any real chain fits comfortably in `u128` (2^116 at 2^64 blocks).
/// Saturates rather than wraps: a nonsense target must never look like free
/// work.
pub fn block_work(target: U256) -> u128 {
    if target.is_zero() {
        return u128::MAX;
    }
    // `U256::MAX / target` is `floor(2^256-1 / target)`; the +1 keeps the
    // ideal 2^256/target without overflowing (target >= MinDifficulty here).
    let work = (U256::MAX / target).saturating_add(U256::one());
    if work > U256::from(u128::MAX) {
        u128::MAX
    } else {
        work.low_u128()
    }
}

/// Sum of [`block_work`] over a branch. Saturating: absurd input degrades to
/// "very heavy", never to a wrapped small number that would lose a fork.
pub fn cumulative<I: IntoIterator<Item = U256>>(targets: I) -> u128 {
    targets
        .into_iter()
        .fold(0u128, |acc, t| acc.saturating_add(block_work(t)))
}

/// One node's announced tip, as far as fork choice is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tip {
    pub genesis: String,
    pub height: u64,
    pub hash: String,
    /// Cumulative work, when the peer speaks `gpncs/2`. `None` = a 0.3.49
    /// `gpncs/1` claim: work unavailable, and fork choice keeps current
    /// behaviour (owner rule).
    pub work: Option<u128>,
}

/// What to do about one peer's claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Leave chain selection alone (and why).
    Keep(KeepReason),
    /// Follow the peer's heavier branch. `depth` is the provable reverted-block
    /// floor (`ours.height - theirs.height`).
    Reorg { depth: u64 },
    /// A heavier chain exists but must not be applied automatically.
    Refused { reason: RefuseReason, depth: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// A different genesis is a different network, never a heavier branch.
    DifferentGenesis,
    /// One side did not announce work (or announced zero): not comparable.
    WorkUnavailable,
    /// The peer is strictly ahead: that is ordinary forward sync by height, not
    /// a fork being resolved. The sync engine handles it as it always has.
    PeerAhead,
    /// Our chain is at least as heavy (or is the same tip). Ties keep the tip.
    NotHeavier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
    /// Deeper than [`MAX_REORG_DEPTH`]: needs the snapshot/catch-up path.
    DeepFork,
    /// The bootstrap primary is authoritative (see [`PRIMARY_AUTHORITATIVE`]).
    PrimaryAuthoritative,
}

impl KeepReason {
    pub fn slug(self) -> &'static str {
        match self {
            KeepReason::DifferentGenesis => "different-genesis",
            KeepReason::WorkUnavailable => "work-unavailable",
            KeepReason::PeerAhead => "peer-ahead",
            KeepReason::NotHeavier => "not-heavier",
        }
    }
}

impl RefuseReason {
    pub fn slug(self) -> &'static str {
        match self {
            RefuseReason::DeepFork => "deep-fork",
            RefuseReason::PrimaryAuthoritative => "primary-authoritative",
        }
    }
}

impl Decision {
    /// Must sealing stop? True while a heavier branch is known but not (yet)
    /// ours — pending apply, or refused as too deep. A primary-authoritative
    /// refusal does **not** pause: our chain is the canonical one by decree and
    /// the network needs it to keep producing blocks.
    pub fn pauses_sealing(self) -> bool {
        matches!(
            self,
            Decision::Reorg { .. }
                | Decision::Refused {
                    reason: RefuseReason::DeepFork,
                    ..
                }
        )
    }

    /// Did the bootstrap stance decide against an otherwise-heavier peer? Logged
    /// separately so the rule is visible, per the owner constraint.
    pub fn is_primary_override(self) -> bool {
        matches!(
            self,
            Decision::Refused {
                reason: RefuseReason::PrimaryAuthoritative,
                ..
            }
        )
    }

    /// Stable text for the log line and tests.
    pub fn result_slug(self) -> &'static str {
        match self {
            Decision::Keep(r) => r.slug(),
            Decision::Reorg { .. } => "accepted",
            Decision::Refused { reason, .. } => reason.slug(),
        }
    }
}

/// Inputs to the decision, kept explicit so tests exercise the same code the
/// node runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// Our node's identity is the bootstrap primary.
    pub is_primary_node: bool,
    /// Our chain contains the primary's currently announced tip (so we are on
    /// the primary's chain even if we are not the primary).
    pub primary_tip_held: bool,
    /// The peer whose claim this is, is the primary.
    pub peer_is_primary: bool,
}

/// The whole fork-choice decision, in one pure place.
pub fn evaluate(ours: &Tip, theirs: &Tip, ctx: Context) -> Decision {
    if ours.genesis != theirs.genesis {
        return Decision::Keep(KeepReason::DifferentGenesis);
    }
    let (Some(our_work), Some(their_work)) = (ours.work, theirs.work) else {
        return Decision::Keep(KeepReason::WorkUnavailable);
    };
    if ours.height == theirs.height && ours.hash == theirs.hash {
        // Same tip: nothing to choose, whatever small difference a windowed
        // work metric shows between two nodes looking at the same block.
        return Decision::Keep(KeepReason::NotHeavier);
    }
    if their_work <= our_work {
        return Decision::Keep(KeepReason::NotHeavier);
    }
    // A peer that is strictly ahead is ordinary forward sync, not a fork this
    // rule resolves: the height-based sync engine already follows it.
    if theirs.height > ours.height {
        return Decision::Keep(KeepReason::PeerAhead);
    }
    // A provable floor on reverted blocks: the common ancestor is at or below
    // the shorter tip.
    let depth = ours.height.saturating_sub(theirs.height);
    if PRIMARY_AUTHORITATIVE
        && !ctx.peer_is_primary
        && (ctx.is_primary_node || ctx.primary_tip_held)
    {
        return Decision::Refused {
            reason: RefuseReason::PrimaryAuthoritative,
            depth,
        };
    }
    if depth > MAX_REORG_DEPTH {
        return Decision::Refused {
            reason: RefuseReason::DeepFork,
            depth,
        };
    }
    Decision::Reorg { depth }
}

/// The one INFO line per state change, shape fixed by the owner:
/// `work ours=X theirs=Y peer=P; reorg=<accepted/refused:reason>`.
pub fn line(ours: &Tip, theirs: &Tip, peer: &str, decision: Decision) -> String {
    let show = |w: Option<u128>| match w {
        Some(w) => w.to_string(),
        None => "unavailable".to_string(),
    };
    let outcome = match decision {
        Decision::Keep(r) => format!("keep:{}", r.slug()),
        Decision::Reorg { depth } => format!("accepted depth={depth}"),
        Decision::Refused { reason, depth } => {
            format!("refused:{} depth={depth}", reason.slug())
        }
    };
    format!(
        "work ours={} theirs={} peer={peer}; reorg={outcome}",
        show(ours.work),
        show(theirs.work)
    )
}

/// The separate, explicit line for the bootstrap stance. Kept apart from
/// [`line`] so "how often the primary did the work" is a distinct greppable
/// signal (`bootstrap: primary-override …`).
pub fn primary_override_line(ours: &Tip, theirs: &Tip, peer: &str) -> String {
    format!(
        "bootstrap: primary-override — peer {peer} announced work {} > ours {} but \
         the primary is authoritative ({}); refusing to displace the primary's chain",
        theirs
            .work
            .map(|w| w.to_string())
            .unwrap_or_else(|| "?".into()),
        ours.work
            .map(|w| w.to_string())
            .unwrap_or_else(|| "?".into()),
        PRIMARY_PEER_ID
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENESIS: &str = "0xb8e4636a00000000000000000000000000000000000000000000000000000001";
    const OTHER: &str = "0xdeadbeef00000000000000000000000000000000000000000000000000000002";
    const H1: &str = "0x1111111100000000000000000000000000000000000000000000000000000001";
    const H2: &str = "0x2222222200000000000000000000000000000000000000000000000000000002";

    fn tip(genesis: &str, height: u64, hash: &str, work: Option<u128>) -> Tip {
        Tip {
            genesis: genesis.into(),
            height,
            hash: hash.into(),
            work,
        }
    }

    fn ctx() -> Context {
        Context {
            is_primary_node: false,
            primary_tip_held: false,
            peer_is_primary: false,
        }
    }

    #[test]
    fn a_heavier_chain_wins_and_is_accepted() {
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        let theirs = tip(GENESIS, 100, H2, Some(1_001));
        let d = evaluate(&ours, &theirs, ctx());
        assert_eq!(d, Decision::Reorg { depth: 0 });
        assert!(d.pauses_sealing());
        assert!(line(&ours, &theirs, "peer-A", d).contains("reorg=accepted"));
        assert!(line(&ours, &theirs, "peer-A", d)
            .starts_with("work ours=1000 theirs=1001 peer=peer-A;"));
    }

    #[test]
    fn equal_work_changes_nothing() {
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        let theirs = tip(GENESIS, 120, H2, Some(1_000)); // taller, same work
        assert_eq!(
            evaluate(&ours, &theirs, ctx()),
            Decision::Keep(KeepReason::NotHeavier)
        );
        // And a lighter one obviously does nothing.
        let lighter = tip(GENESIS, 130, H2, Some(999));
        assert_eq!(
            evaluate(&ours, &lighter, ctx()),
            Decision::Keep(KeepReason::NotHeavier)
        );
    }

    #[test]
    fn work_that_cannot_be_computed_changes_nothing() {
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        let no_work = tip(GENESIS, 200, H2, None);
        assert_eq!(
            evaluate(&ours, &no_work, ctx()),
            Decision::Keep(KeepReason::WorkUnavailable)
        );
        let ours_unknown = tip(GENESIS, 100, H1, None);
        let heavy = tip(GENESIS, 100, H2, Some(9_999));
        assert_eq!(
            evaluate(&ours_unknown, &heavy, ctx()),
            Decision::Keep(KeepReason::WorkUnavailable)
        );
    }

    #[test]
    fn a_different_genesis_is_never_heavier() {
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        let theirs = tip(OTHER, 1_000, H2, Some(u128::MAX));
        assert_eq!(
            evaluate(&ours, &theirs, ctx()),
            Decision::Keep(KeepReason::DifferentGenesis)
        );
        assert!(!evaluate(&ours, &theirs, ctx()).pauses_sealing());
    }

    #[test]
    fn a_reorg_deeper_than_the_bound_is_refused_and_pauses_sealing() {
        let ours = tip(GENESIS, MAX_REORG_DEPTH + 1 + 50, H1, Some(1_000));
        let theirs = tip(GENESIS, 50, H2, Some(5_000));
        let d = evaluate(&ours, &theirs, ctx());
        assert_eq!(
            d,
            Decision::Refused {
                reason: RefuseReason::DeepFork,
                depth: MAX_REORG_DEPTH + 1,
            }
        );
        assert!(d.pauses_sealing());
        assert!(!d.is_primary_override());
        let text = line(&ours, &theirs, "peer-B", d);
        assert!(text.contains("reorg=refused:deep-fork"), "{text}");
    }

    #[test]
    fn the_bound_itself_is_still_accepted() {
        // Exactly MAX_REORG_DEPTH is shallow by the rule; MAX+1 is not.
        let ours = tip(GENESIS, 500, H1, Some(1_000));
        let theirs = tip(GENESIS, 500 - MAX_REORG_DEPTH, H2, Some(2_000));
        assert_eq!(
            evaluate(&ours, &theirs, ctx()),
            Decision::Reorg {
                depth: MAX_REORG_DEPTH
            }
        );
    }

    #[test]
    fn the_primary_overrides_a_heavier_non_primary_peer() {
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        let theirs = tip(GENESIS, 100, H2, Some(99_999));
        let mut c = ctx();
        c.is_primary_node = true;
        let d = evaluate(&ours, &theirs, c);
        assert_eq!(
            d,
            Decision::Refused {
                reason: RefuseReason::PrimaryAuthoritative,
                depth: 0
            }
        );
        assert!(d.is_primary_override());
        assert!(
            !d.pauses_sealing(),
            "primary keeps sealing: its chain is canonical by decree"
        );
        let text = primary_override_line(&ours, &theirs, "peer-C");
        assert!(text.contains("bootstrap: primary-override"));
        assert!(text.contains("99999"));
    }

    #[test]
    fn holding_the_primary_tip_is_enough_to_protect_it() {
        // A non-primary node that is on the primary's chain obeys the same
        // stance: a heavier branch that does not contain the primary's chain
        // must not displace it.
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        let theirs = tip(GENESIS, 100, H2, Some(99_999));
        let mut c = ctx();
        c.primary_tip_held = true;
        assert!(evaluate(&ours, &theirs, c).is_primary_override());
    }

    #[test]
    fn the_primary_does_not_override_normal_forward_sync() {
        // A claim from the primary (or any peer) strictly ahead is ordinary
        // sync, not a reorg; the height-based engine handles it.
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        let theirs = tip(GENESIS, 120, H2, Some(2_000));
        let mut c = ctx();
        c.is_primary_node = true;
        c.peer_is_primary = true;
        assert_eq!(
            evaluate(&ours, &theirs, c),
            Decision::Keep(KeepReason::PeerAhead)
        );
    }

    #[test]
    fn the_same_tip_is_never_a_reorg_however_the_window_shifts() {
        let ours = tip(GENESIS, 100, H1, Some(1_000));
        // Same block, slightly different windowed work figure (e.g. the two
        // nodes' windows end one block apart in time).
        let theirs = tip(GENESIS, 100, H1, Some(1_003));
        assert_eq!(
            evaluate(&ours, &theirs, ctx()),
            Decision::Keep(KeepReason::NotHeavier)
        );
    }

    #[test]
    fn deeper_targets_are_more_work_and_work_sums() {
        // Higher target = easier = less work; lower target = harder.
        let easy = U256::one() << 232usize;
        let hard = U256::one() << 231usize;
        assert!(block_work(hard) > block_work(easy));
        assert_eq!(block_work(easy), 1u128 << 24); // 2^256 / 2^232
        assert_eq!(block_work(U256::zero()), u128::MAX);
        // 3 easy blocks + 2 hard blocks = 3*2^24 + 2*2^25.
        let total = cumulative([easy, easy, easy, hard, hard]);
        assert_eq!(total, 3 * (1u128 << 24) + 2 * (1u128 << 25));
    }

    #[test]
    fn cumulative_saturates_instead_of_wrapping() {
        let zero = U256::zero();
        assert_eq!(cumulative([zero, zero]), u128::MAX);
    }

    #[test]
    fn constants_are_pinned() {
        assert_eq!(MAX_REORG_DEPTH, 288);
        // A constant assertion belongs at compile time, not in a test body —
        // `assertions_on_constants` is denied in CI (W58, again in 0.3.50).
        const _: () = assert!(PRIMARY_AUTHORITATIVE);
        assert_eq!(PRIMARY_PEER_ID.len(), 52); // 12D3KooW + 44 base58
    }
}
