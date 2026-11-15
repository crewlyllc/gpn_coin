//! Who may be a source of truth — peer *progress* and the signed anchor.
//!
//! On 2026-11-15 13:00 a brand-new peer `12D3KooWS3…` appeared at block 69,295
//! while the network tip was ~307,300. Box 1 — itself behind — latched onto it
//! and dropped the canonical fleet node, so both crawled:
//!
//! ```text
//! 13:00:49 chain-state: peer WP1JAF is behind
//! 13:00:50 sync: peer WP1JAF abandoned a request mid-flight
//! 13:00:51 substream closed
//! ```
//!
//! Two separate mistakes are visible in those three lines, and neither is about
//! one box:
//!
//! 1. A laggard preferred *another laggard* because that is the peer that
//!    answered first. Preference must be by **canonical progress**: the peer
//!    whose best block is furthest along, with a chain matching a signed anchor,
//!    is the one worth a slot. This module is the ranking and the verdict, so
//!    `peer_priority` can order dials and the download gate can refuse a peer
//!    that cannot be a source.
//! 2. A peer is never rejected *for being behind*. A behind-but-canonical peer
//!    is a future peer; the only peers that must not be used as a source are
//!    ones that **disagree with the signed anchor** (a fork) or come from a
//!    different genesis (a different network).
//!
//! ## What this module owns
//!
//! * [`Anchors`] — a set of `(height, hash)` checkpoints a canonical chain must
//!   contain, read from the *verified* `snapshot.json` (written by
//!   `gpn_fleet`'s self-heal after it checks the ed25519 signature). An absent
//!   file is not an error: with no anchors every peer is `Unknown`, which is
//!   the safe direction — nothing is rejected on a guess.
//! * [`decide`] — one claim + our tip + the anchors → a [`Use`] verdict.
//! * [`rank_sources`] — the same comparison for many peers, best first.
//! * [`DecisionLog`] — the INFO line, **once per peer per state change**, as
//!   2026-11-15's incident made necessary: the operator must be able to read
//!    *why* a peer was used, and read it once, not once per 30 s tick.
//! * the bootstrap decision — snapshot-first or crawl — with the threshold that
//!   actually decides it, and the one line logged when the snapshot is
//!   unreadable. It lives in `gpn_agent::selfheal` (`should_catch_up`), in the
//!   crate that holds the signer key.
//!
//! Snapshot-first bootstrap lives with the supervisor that can perform it:
//! `gpn_agent::selfheal` (`should_catch_up` / `SNAPSHOT_BEHIND_BLOCKS`). The node
//! binary cannot verify the signed document (it carries no signer key), so one
//! copy of that policy — in the crate that has the key — beats two.
//!
//! Pure: no libp2p, no client, no clock. The wiring lives in `service.rs`
//! (chain-state task) and `block_serving.rs` (the gate that refuses a source).

use std::collections::HashMap;

use crate::chain_state::ChainState;

/// How a peer's claimed chain relates to the signed anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnchorClass {
    /// The peer claimed a block at a signed anchor height and it is a different
    /// block. A fork: never a source of truth.
    Mismatch = 0,
    /// Nothing to compare (no anchor at that height, no anchors at all, or the
    /// peer is above every anchor we hold).
    Unknown = 1,
    /// Its claimed block at a signed anchor height is the signed block.
    Match = 2,
}

impl AnchorClass {
    /// `true` / `false` for the log line; `None` when there was nothing to
    /// compare. Deliberately three-state: an unknown is not a match.
    pub fn as_opt(self) -> Option<bool> {
        match self {
            AnchorClass::Match => Some(true),
            AnchorClass::Mismatch => Some(false),
            AnchorClass::Unknown => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            AnchorClass::Match => "match",
            AnchorClass::Mismatch => "mismatch",
            AnchorClass::Unknown => "unknown",
        }
    }
}

/// The signed `(height, hash)` checkpoints a canonical chain must contain.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Anchors {
    /// Sorted ascending by height, hashes normalised `0x` + lowercase hex.
    pairs: Vec<(u64, String)>,
}

impl Anchors {
    pub fn empty() -> Self {
        Self { pairs: Vec::new() }
    }

    /// Build from `(height, hash)` pairs. Invalid hashes are dropped (never
    /// compared against), duplicates collapse to one entry, and the result is
    /// sorted so [`Self::newest_at_or_below`] is a binary search.
    pub fn new(pairs: impl IntoIterator<Item = (u64, String)>) -> Self {
        let mut out: Vec<(u64, String)> = Vec::new();
        for (h, hash) in pairs {
            let Some(hash) = normalise(&hash) else {
                continue;
            };
            match out.iter_mut().find(|(existing, _)| *existing == h) {
                Some(entry) => entry.1 = hash,
                None => out.push((h, hash)),
            }
        }
        out.sort_by_key(|(h, _)| *h);
        Self { pairs: out }
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Read the anchors from the JSON a supervisor writes after verifying the
    /// signed `snapshot.json`:
    ///
    /// ```json
    /// {"genesis":"0x…","anchors":[[200000,"0x…"],[100000,"0x…"]]}
    /// ```
    ///
    /// Tolerant by design: anything unparseable yields no anchors (no
    /// rejections) instead of a hard failure — a node must start.
    pub fn from_json(text: &str) -> Self {
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(text) else {
            return Self::empty();
        };
        let Some(list) = doc.get("anchors").and_then(|v| v.as_array()) else {
            return Self::empty();
        };
        Self::new(list.iter().filter_map(|entry| {
            let pair = entry.as_array()?;
            let height = pair.first()?.as_u64()?;
            let hash = pair.get(1)?.as_str()?.to_string();
            Some((height, hash))
        }))
    }

    /// The canonical text [`Self::from_json`] reads, for the writer's tests.
    pub fn to_json(&self, genesis: &str) -> String {
        let anchors: Vec<serde_json::Value> = self
            .pairs
            .iter()
            .map(|(h, hash)| serde_json::json!([h, hash]))
            .collect();
        serde_json::json!({ "genesis": genesis, "anchors": anchors }).to_string()
    }

    /// The newest anchor at or below `height` — the checkpoint a chain at that
    /// height must contain.
    pub fn newest_at_or_below(&self, height: u64) -> Option<&(u64, String)> {
        let idx = self.pairs.partition_point(|(h, _)| *h <= height);
        if idx == 0 {
            None
        } else {
            self.pairs.get(idx - 1)
        }
    }

    /// Classify a claim. Only an anchor at **exactly** the claimed height can be
    /// compared with the claimed best hash — a peer's hash at some other height
    /// is not on the wire, and guessing is how a canonical peer gets rejected.
    pub fn classify(&self, claim: &ChainState) -> AnchorClass {
        match self.pairs.iter().find(|(h, _)| *h == claim.height) {
            None => AnchorClass::Unknown,
            Some((_, want)) => match normalise(&claim.best_hash) {
                Some(got) if &got == want => AnchorClass::Match,
                Some(_) => AnchorClass::Mismatch,
                None => AnchorClass::Unknown,
            },
        }
    }

    /// Compare *our own* chain against the newest anchor we have reached.
    ///
    /// `hash_at` reads our block hash at a height (`None` = we do not have it,
    /// which is never evidence of a fork). `Some((height, expected, local))` is
    /// a proven fork of ours: the caller stops treating itself as canonical.
    pub fn self_check<F>(&self, our_height: u64, hash_at: F) -> Option<(u64, String, String)>
    where
        F: Fn(u64) -> Option<String>,
    {
        let (height, expected) = self.newest_at_or_below(our_height)?;
        let local = normalise(&hash_at(*height)?)?;
        if &local == expected {
            None
        } else {
            Some((*height, expected.clone(), local))
        }
    }
}

fn normalise(hash: &str) -> Option<String> {
    let s = hash.trim();
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    if body.len() != 64 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", body.to_ascii_lowercase()))
}

/// May this peer be a source of truth, and why not?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// Same genesis, anchor-consistent, and ahead of us (or level): use it.
    Source,
    /// Same genesis and anchor-consistent, but behind us: keep the link, serve
    /// it canonical blocks, do not sync from it. **Not a rejection.**
    BehindButCanonical,
    /// Contradicts a signed anchor: a fork. Never a source of truth, keep the
    /// peer so it can be healed.
    Fork,
    /// Different genesis: a different network, no amount of syncing fixes it.
    WrongNetwork,
    /// No usable claim (no claim yet, or an unreadable one).
    Unknown,
}

impl Use {
    /// The one question the download gate asks.
    pub fn is_source(self) -> bool {
        matches!(self, Use::Source)
    }

    /// Is this a state we should shout about? A fork and a wrong network are
    /// actionable; "behind" is normal and stays INFO.
    pub fn is_notable(self) -> bool {
        matches!(self, Use::Fork | Use::WrongNetwork)
    }

    pub fn slug(self) -> &'static str {
        match self {
            Use::Source => "source",
            Use::BehindButCanonical => "behind-canonical",
            Use::Fork => "fork",
            Use::WrongNetwork => "wrong-network",
            Use::Unknown => "unknown",
        }
    }
}

/// The whole per-peer decision, in one pure place.
///
/// Preference order, in words: a peer that agrees with the signed anchor and is
/// furthest along is best; a peer that agrees but is behind us is fine to keep
/// and useless to sync from; a peer that disagrees with the anchor is never a
/// source even when it claims the highest block in the network.
pub fn decide(
    our_best: u64,
    our_genesis: &str,
    claim: Option<&ChainState>,
    anchors: &Anchors,
) -> (Use, AnchorClass) {
    decide_with_tip(our_best, our_genesis, claim, anchors, None)
}

/// Margin below our best inside which an unknown tip is still "maybe arriving".
/// Same 32 blocks the download gate uses, for the same reason: two chains a few
/// blocks apart are normal and worth waiting for.
pub const TIP_UNKNOWN_MARGIN: u64 = 32;

/// [`decide`] plus the one fact the numbers cannot carry: **is the peer's
/// claimed tip a block we hold?**
///
/// For a claim at or below our best, "not a block we hold" means "not on the
/// chain we call canonical" — that peer is on a different branch and must never
/// be our source of truth, however plausible its height. `None` (no answer from
/// the database) never promotes or demotes: an unanswered question is not
/// evidence.
pub fn decide_with_tip(
    our_best: u64,
    our_genesis: &str,
    claim: Option<&ChainState>,
    anchors: &Anchors,
    tip_known: Option<bool>,
) -> (Use, AnchorClass) {
    let Some(claim) = claim else {
        return (Use::Unknown, AnchorClass::Unknown);
    };
    if claim.genesis != our_genesis {
        return (Use::WrongNetwork, AnchorClass::Mismatch);
    }
    let mut anchor = anchors.classify(claim);
    // A tip we hold is on our (canonical) chain; a tip we do not hold, at a
    // height we have already passed, is a branch we are not on.
    if anchor == AnchorClass::Unknown {
        match tip_known {
            Some(true) => anchor = AnchorClass::Match,
            Some(false) if claim.height.saturating_add(TIP_UNKNOWN_MARGIN) <= our_best => {
                anchor = AnchorClass::Mismatch
            }
            _ => {}
        }
    }
    if anchor == AnchorClass::Mismatch {
        return (Use::Fork, anchor);
    }
    if claim.height > our_best {
        (Use::Source, anchor)
    } else if claim.height < our_best {
        (Use::BehindButCanonical, anchor)
    } else {
        // Level: same height, same block is the normal case; same height with a
        // different hash is divergence, which #158 says is not a rejection —
        // keep the link, do not take blocks from it.
        (Use::Source, anchor)
    }
}

/// The one INFO line per peer per state change.
///
/// Required shape (owner, 2026-11-15):
/// `peer X best=#N vs ours #M; canonical/anchor-match=<bool>; using/not using as
/// sync source`.
pub fn decision_line(
    peer: &str,
    our_best: u64,
    claim: &ChainState,
    use_: Use,
    anchor: AnchorClass,
) -> String {
    let using = if use_.is_source() {
        "using as sync source"
    } else {
        match use_ {
            Use::BehindButCanonical => "not using as sync source (behind us, canonical)",
            Use::Fork => "not using as sync source (FORK: its tip is not on our canonical chain)",
            Use::WrongNetwork => "not using as sync source (DIFFERENT NETWORK: genesis differs)",
            Use::Unknown => "not using as sync source (no usable claim)",
            Use::Source => unreachable!(),
        }
    };
    let anchor_text = match anchor.as_opt() {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    };
    format!(
        "peer {peer} best=#{} vs ours #{our_best}; canonical/anchor-match={anchor_text}; {using}",
        claim.height
    )
}

/// Rank peers as sync sources: anchor-matching and furthest along first.
///
/// Ties break on peer id so two nodes that see the same set agree on the order
/// (the same determinism rule `peer_priority` uses).
pub fn rank_sources(
    our_best: u64,
    our_genesis: &str,
    claims: &[(String, ChainState)],
    anchors: &Anchors,
) -> Vec<(String, Use, AnchorClass)> {
    let mut out: Vec<(String, Use, AnchorClass)> = claims
        .iter()
        .map(|(peer, claim)| {
            let (use_, anchor) = decide(our_best, our_genesis, Some(claim), anchors);
            (peer.clone(), use_, anchor)
        })
        .collect();
    out.sort_by(|a, b| {
        b.2.cmp(&a.2) // Match before Unknown before Mismatch
            .then_with(|| {
                let ha = claims
                    .iter()
                    .find(|(p, _)| p == &a.0)
                    .map(|(_, c)| c.height);
                let hb = claims
                    .iter()
                    .find(|(p, _)| p == &b.0)
                    .map(|(_, c)| c.height);
                hb.cmp(&ha)
            })
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// The best source, if any can be a source at all.
pub fn best_source(
    our_best: u64,
    our_genesis: &str,
    claims: &[(String, ChainState)],
    anchors: &Anchors,
) -> Option<(String, ChainState)> {
    let ranked = rank_sources(our_best, our_genesis, claims, anchors);
    let (peer, _use, _anchor) = ranked.into_iter().find(|(_, u, _)| u.is_source())?;
    claims
        .iter()
        .find(|(p, _)| p == &peer)
        .map(|(p, c)| (p.clone(), c.clone()))
}

/// Edge-triggered per-peer reporting: the log line appears when a peer's
/// verdict, height or hash changes, and not on the 30 s tick in between.
#[derive(Debug, Default)]
pub struct DecisionLog {
    seen: HashMap<String, (Use, AnchorClass, u64, String)>,
}

impl DecisionLog {
    /// Record one decision. `Some((use, anchor))` when this is a state change
    /// worth logging; `None` when nothing changed since the last look.
    pub fn record(
        &mut self,
        peer: &str,
        use_: Use,
        anchor: AnchorClass,
        claim: &ChainState,
    ) -> Option<(Use, AnchorClass)> {
        let entry = (use_, anchor, claim.height, claim.best_hash.clone());
        if self.seen.get(peer) == Some(&entry) {
            return None;
        }
        self.seen.insert(peer.to_string(), entry);
        Some((use_, anchor))
    }

    /// A reconnect is a new session: log its state again.
    pub fn forget(&mut self, peer: &str) {
        self.seen.remove(peer);
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENESIS: &str = "0xb8e4636a00000000000000000000000000000000000000000000000000000001";
    const OTHER: &str = "0xdeadbeef00000000000000000000000000000000000000000000000000000002";
    const A1: &str = "0x1111111100000000000000000000000000000000000000000000000000000001";
    const A2: &str = "0x2222222200000000000000000000000000000000000000000000000000000002";
    const A3: &str = "0x3333333300000000000000000000000000000000000000000000000000000003";

    fn claim(genesis: &str, hash: &str, height: u64) -> ChainState {
        ChainState::new(genesis, hash, height).expect("valid hash")
    }

    fn anchors() -> Anchors {
        Anchors::new(vec![(100_000, A1.into()), (200_000, A2.into())])
    }

    // ---- the anchor set -------------------------------------------------

    #[test]
    fn anchors_are_sorted_deduplicated_and_hashes_are_strict() {
        let a = Anchors::new(vec![
            (200_000, A2.to_ascii_uppercase()),
            (100_000, A1.into()),
            (200_000, A3.into()), // duplicate height replaces
            (5, "not-a-hash".into()),
        ]);
        assert_eq!(a.len(), 2);
        assert_eq!(
            a.newest_at_or_below(150_000),
            Some(&(100_000, A1.to_string()))
        );
        assert_eq!(
            a.newest_at_or_below(999_999),
            Some(&(200_000, A3.to_string()))
        );
        assert_eq!(a.newest_at_or_below(99_999), None);
        assert!(Anchors::empty().newest_at_or_below(10).is_none());
    }

    #[test]
    fn anchors_round_trip_through_the_supervisor_json() {
        let a = anchors();
        let text = a.to_json(GENESIS);
        assert_eq!(Anchors::from_json(&text), a);
        // Hostile / absent input yields no anchors, never a panic.
        assert!(Anchors::from_json("").is_empty());
        assert!(Anchors::from_json("not json").is_empty());
        assert!(Anchors::from_json(r#"{"anchors":"nope"}"#).is_empty());
        let partial = Anchors::from_json(r#"{"anchors":[[5],["x","0x00"],[7,"0x"]]}"#);
        assert!(partial.is_empty(), "an incomplete entry is dropped");
    }

    #[test]
    fn only_an_anchor_at_the_claimed_height_can_be_compared() {
        let a = anchors();
        // Exactly at an anchor, matching → Match.
        assert_eq!(a.classify(&claim(GENESIS, A1, 100_000)), AnchorClass::Match);
        // Exactly at an anchor, different block → Mismatch.
        assert_eq!(
            a.classify(&claim(GENESIS, A3, 100_000)),
            AnchorClass::Mismatch
        );
        // Anywhere else nothing is claimed, so nothing is inferred.
        assert_eq!(
            a.classify(&claim(GENESIS, A3, 150_000)),
            AnchorClass::Unknown
        );
        assert_eq!(
            Anchors::empty().classify(&claim(GENESIS, A3, 100_000)),
            AnchorClass::Unknown
        );
    }

    // ---- task 2/3: who is a source --------------------------------------

    #[test]
    fn a_peer_that_mismatches_the_signed_anchor_is_never_a_source() {
        let a = anchors();
        // It claims the highest block in the network AND contradicts the anchor.
        let fork = claim(GENESIS, A3, 100_000);
        let (use_, anchor) = decide(10, GENESIS, Some(&fork), &a);
        assert_eq!(use_, Use::Fork);
        assert!(!use_.is_source());
        assert!(use_.is_notable());
        assert_eq!(anchor.as_opt(), Some(false));
    }

    #[test]
    fn a_behind_peer_is_kept_not_rejected() {
        let a = anchors();
        let behind = claim(GENESIS, A1, 69_295);
        let (use_, anchor) = decide(307_300, GENESIS, Some(&behind), &a);
        assert_eq!(use_, Use::BehindButCanonical);
        assert!(!use_.is_source());
        assert!(!use_.is_notable(), "being behind is not news");
        assert_eq!(anchor, AnchorClass::Unknown);
    }

    #[test]
    fn a_different_genesis_is_not_a_fork() {
        let (use_, _) = decide(10, GENESIS, Some(&claim(OTHER, A1, 50)), &anchors());
        assert_eq!(use_, Use::WrongNetwork);
        assert!(!use_.is_source());
    }

    #[test]
    fn the_furthest_anchor_matching_peer_wins_even_when_a_laggard_answered_first() {
        let a = anchors();
        let claims = vec![
            // Arrival order: the laggard first, exactly as on 2026-11-15.
            ("12D3KooWS3newcomer".to_string(), claim(GENESIS, A1, 69_295)),
            ("WP1JAFbox1".to_string(), claim(GENESIS, A1, 180_000)),
            ("12D3KooWcanonical".to_string(), claim(GENESIS, A2, 307_300)),
            ("12D3KooWfork".to_string(), claim(GENESIS, A3, 100_000)),
        ];
        let ranked = rank_sources(307_300, GENESIS, &claims, &a);
        assert_eq!(ranked[0].0, "12D3KooWcanonical");
        assert_eq!(ranked[0].1, Use::Source);
        // The fork sorts last, behind every honest peer.
        assert_eq!(ranked.last().unwrap().0, "12D3KooWfork");
        let best = best_source(307_300, GENESIS, &claims, &a).expect("a source exists");
        assert_eq!(best.0, "12D3KooWcanonical");
    }

    #[test]
    fn a_laggard_prefers_a_canonical_seed_over_a_further_lagging_newcomer() {
        // Our node is the laggard (box 1 at #180k).
        let a = anchors();
        let claims = vec![
            ("12D3KooWS3newcomer".to_string(), claim(GENESIS, A1, 69_295)),
            ("12D3KooWseed".to_string(), claim(GENESIS, A2, 307_300)),
        ];
        let best = best_source(180_000, GENESIS, &claims, &a).expect("seed usable");
        assert_eq!(best.0, "12D3KooWseed");
        assert_eq!(best.1.height, 307_300);
        // With only the further-lagging peer reachable, it is still *usable*
        // (behind us is fine to keep) but not a source — the laggard keeps
        // dialling and waits for the seed, exactly what `rank_sources` shows.
        let only_laggard = vec![claims[0].clone()];
        assert!(best_source(180_000, GENESIS, &only_laggard, &a).is_none());
        let (use_, _) = decide(180_000, GENESIS, Some(&claims[0].1), &a);
        assert_eq!(use_, Use::BehindButCanonical);
    }

    #[test]
    fn a_tip_we_do_not_hold_below_our_best_is_a_fork_even_without_an_anchor() {
        let a = Anchors::empty();
        let on_branch = claim(GENESIS, A3, 306_633);
        // 2026-11-15: box 1 at #306,633 on its own branch, ours #307,403.
        // A tip we do not hold below our best is a fork even without an anchor.
        let (use_, anchor) = decide_with_tip(307_403, GENESIS, Some(&on_branch), &a, Some(false));
        assert_eq!(use_, Use::Fork);
        assert_eq!(anchor.as_opt(), Some(false));
        // A few blocks apart: the block may simply not have arrived yet.
        let near = claim(GENESIS, A3, 307_390);
        let (use_, anchor) = decide_with_tip(307_403, GENESIS, Some(&near), &a, Some(false));
        assert_eq!(use_, Use::BehindButCanonical);
        assert_eq!(anchor, AnchorClass::Unknown);
        // A tip we hold is canonical, whatever the height.
        let (use_, anchor) = decide_with_tip(307_403, GENESIS, Some(&near), &a, Some(true));
        assert_eq!(use_, Use::BehindButCanonical);
        assert_eq!(anchor, AnchorClass::Match);
        // An unanswered lookup changes nothing.
        let (use_, anchor) = decide_with_tip(307_403, GENESIS, Some(&on_branch), &a, None);
        assert_eq!(use_, Use::BehindButCanonical);
        assert_eq!(anchor, AnchorClass::Unknown);
    }

    #[test]
    fn no_claim_is_unknown_and_never_a_source() {
        let (use_, anchor) = decide(10, GENESIS, None, &anchors());
        assert_eq!(use_, Use::Unknown);
        assert_eq!(anchor, AnchorClass::Unknown);
        assert!(!use_.is_source());
    }

    // ---- task 5: the log line ------------------------------------------

    #[test]
    fn the_line_carries_the_four_facts_and_appears_once_per_state_change() {
        let a = anchors();
        // A claim at an anchor height: the one case where the anchor half of
        // the line can be decided from the wire.
        let canonical = claim(GENESIS, A2, 200_000);
        let (use_, anchor) = decide(180_000, GENESIS, Some(&canonical), &a);
        let line = decision_line("12D3KooWseed", 180_000, &canonical, use_, anchor);
        assert!(line.contains("best=#200000 vs ours #180000"), "{line}");
        assert!(line.contains("canonical/anchor-match=true"), "{line}");
        assert!(line.contains("using as sync source"), "{line}");

        let mut log = DecisionLog::default();
        assert_eq!(
            log.record("p", use_, anchor, &canonical),
            Some((use_, anchor))
        );
        // The same state on the next tick is not news.
        assert_eq!(log.record("p", use_, anchor, &canonical), None);
        // A new tip is a state change.
        let moved = claim(GENESIS, A2, 307_301);
        assert_eq!(
            log.record("p", Use::Source, anchor, &moved),
            Some((Use::Source, anchor))
        );
        // A verdict change (fork) is a state change.
        let fork = claim(GENESIS, A3, 100_000);
        let (fu, fa) = decide(307_300, GENESIS, Some(&fork), &a);
        assert_eq!(log.record("p", fu, fa, &fork), Some((fu, fa)));
        // A reconnect logs again rather than inheriting silence.
        log.forget("p");
        assert_eq!(log.record("p", fu, fa, &fork), Some((fu, fa)));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn a_fork_line_says_false_and_not_using() {
        let a = anchors();
        let fork = claim(GENESIS, A3, 100_000);
        let (use_, anchor) = decide(307_300, GENESIS, Some(&fork), &a);
        let line = decision_line("12D3KooWfork", 307_300, &fork, use_, anchor);
        assert!(line.contains("canonical/anchor-match=false"), "{line}");
        assert!(line.contains("not using as sync source"), "{line}");
        assert!(line.contains("FORK"), "{line}");
    }

    // ---- ours, against the anchor ---------------------------------------

    #[test]
    fn our_own_chain_is_checked_against_the_newest_anchor_we_reached() {
        let a = anchors();
        // At #250k the newest anchor is #200k.
        assert!(a
            .self_check(250_000, |h| if h == 200_000 {
                Some(A2.into())
            } else {
                None
            })
            .is_none());
        let fork = a
            .self_check(
                250_000,
                |h| if h == 200_000 { Some(A3.into()) } else { None },
            )
            .expect("a fork");
        assert_eq!(fork, (200_000, A2.to_string(), A3.to_string()));
        // Below every anchor: nothing to compare, nothing to conclude.
        assert!(a.self_check(50, |_| Some(A3.into())).is_none());
        // We do not have the anchor height: not evidence.
        assert!(a.self_check(250_000, |_| None).is_none());
    }
}
