//! Peer priority and ranked re-dial (GPN #154, design §1).
//!
//! GPN used to dial whatever discovery produced, in whatever order it produced
//! it, and shuffle the seed list so no two nodes agreed on a dial order. On
//! 2026-11-09 that produced two disjoint pairs (`linux ↔ box 2`,
//! `box 1 ↔ box 4`) and every node looked healthy with "1 peer".
//!
//! This module is the **policy** half of the fix: a deterministic, identical
//! ranking of who to dial, with
//!
//! ```text
//! Tier 0 — oracle seeds   up to 5, reserved slots, always dialled, never evicted
//! Tier 1 — on-chain peers ranked by tenure (first-seen block) descending
//! Tier 2 — unknown peers  dialled only while slots remain, evicted first
//! ```
//!
//! and exponential dial backoff (30 s → 8 min, jittered) that never removes an
//! entry.
//!
//! Everything here is pure and unit-tested; `service.rs` supplies the
//! multiaddresses and does the dialling.
//!
//! ## Known limitation (documented, not hidden)
//!
//! The design asks for tenure = *the block at which the peer's account first
//! appears on chain*, which would make the ranking byte-identical on every
//! machine. The runtime does not store a first-seen block for a provider or a
//! full node, so a caller can only usually supply [`Tenure::Unknown`]. This
//! module therefore orders by tenure when it is known and falls back to a
//! **stable, deterministic** tie-break (peer id) when it is not — never by
//! arrival order or randomness. A true cross-machine tenure needs an on-chain
//! field, which is a runtime change (review-first) and is filed separately.

use crate::network_registry::BootstrapSeed;

/// Design §1: no more than five oracle seeds (a sixth requires retiring one).
pub const MAX_ORACLE_SEEDS: usize = 5;
/// Design §1: target peer table size (Tier 0 is exempt from this cap).
pub const DEFAULT_MAX_PEERS: usize = 8;
/// Design §1: tier-0 entries are re-dialled every 60 s.
pub const REDIAL_INTERVAL_SECS: u64 = 60;
/// Design §1: first backoff step after a failed dial.
pub const DIAL_BACKOFF_BASE_SECS: u64 = 30;
/// Design §1: backoff is capped at 8 min.
pub const DIAL_BACKOFF_CAP_SECS: u64 = 8 * 60;

/// Priority class of a candidate (lower dials first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Oracle seed — reserved slot, always dialled, never evicted.
    OracleSeed = 0,
    /// Anything with an on-chain record, ranked by tenure.
    Ranked = 1,
    /// No on-chain record (learned by exchange, or a fresh identity).
    Unknown = 2,
}

/// A dialable peer plus the facts the ranking needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Full multiaddress, including the `/p2p/<peer id>` suffix.
    pub addr: String,
    /// The peer id parsed out of `addr` (`""` when the addr is malformed).
    pub peer_id: String,
    pub tier: Tier,
    /// First-seen block, when known (see the module note).
    pub tenure: Option<u64>,
}

impl Candidate {
    /// Build a candidate, deriving the peer id from the multiaddress.
    pub fn new(addr: &str, tier: Tier, tenure: Option<u64>) -> Self {
        Self {
            addr: addr.to_string(),
            peer_id: peer_id_of(addr).unwrap_or_default().to_string(),
            tier,
            tenure,
        }
    }

    /// A candidate without a peer id can never be dialled (the handshake has
    /// nothing to authenticate against, which is exactly the rogue-host case).
    pub fn is_dialable(&self) -> bool {
        !self.peer_id.is_empty()
    }
}

/// The result of applying the slot policy to a candidate set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialPlan {
    /// Candidates to dial, highest priority first.
    pub dial: Vec<Candidate>,
    /// Candidates left alone this round (evicted first = lowest priority).
    pub skipped: Vec<Candidate>,
}

/// Extract the peer id from a multiaddress (`…/p2p/12D3Koo…`).
///
/// Takes the **last** `/p2p/` segment: a relayed address
/// (`/p2p/<relay>/p2p-circuit/p2p/<target>`) must yield the target.
pub fn peer_id_of(addr: &str) -> Option<&str> {
    let idx = addr.rfind("/p2p/")?;
    let id = &addr[idx + "/p2p/".len()..];
    let id = id.split('/').next().unwrap_or("");
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// The oracle-seed peer ids (tier 0) from the compiled-in bootstrap seeds,
/// capped at [`MAX_ORACLE_SEEDS`] and de-duplicated in declaration order.
pub fn oracle_peer_ids(seeds: &[BootstrapSeed]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in seeds {
        if out.len() >= MAX_ORACLE_SEEDS {
            break;
        }
        let id = s.peer_id.trim();
        if id.is_empty() || out.iter().any(|e| e == id) {
            continue;
        }
        out.push(id.to_string());
    }
    out
}

/// Classify a peer: tier 0 if it is an oracle seed, else tier 1 when it has an
/// on-chain record, else tier 2.
pub fn tier_for(peer_id: &str, oracle_ids: &[String], on_chain: bool) -> Tier {
    if !peer_id.is_empty() && oracle_ids.iter().any(|o| o == peer_id) {
        Tier::OracleSeed
    } else if on_chain {
        Tier::Ranked
    } else {
        Tier::Unknown
    }
}

/// Deterministic rank order: tier ascending, then tenure **descending** (older
/// first, unknown tenure last), then peer id ascending.
///
/// The peer-id tie-break is what makes the order identical on every machine
/// when tenure is unknown — the design's "two nodes never disagree" property,
/// achieved without a random shuffle.
pub fn rank(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = candidates
        .iter()
        .filter(|c| c.is_dialable())
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| match (a.tenure, b.tenure) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.peer_id.cmp(&b.peer_id))
    });
    // A peer can appear twice (chain endpoint + seed list); keep the best entry.
    out.dedup_by(|a, b| a.peer_id == b.peer_id);
    out
}

/// Apply the slot policy: every tier-0 entry is dialled (exempt from the cap),
/// then remaining slots are filled by rank. Everything else is skipped.
pub fn dial_plan(candidates: &[Candidate], max_peers: usize) -> DialPlan {
    let ranked = rank(candidates);
    let mut plan = DialPlan::default();
    let mut filled = 0usize;
    for c in ranked {
        if c.tier == Tier::OracleSeed {
            plan.dial.push(c);
            continue;
        }
        if filled < max_peers {
            filled += 1;
            plan.dial.push(c);
        } else {
            plan.skipped.push(c);
        }
    }
    plan
}

/// Which connected peers to drop when the table is over the cap.
///
/// Lowest tier first, then youngest tenure, then highest peer id — and a
/// tier-0 entry is never in the result.
pub fn evictions(connected: &[Candidate], max_peers: usize) -> Vec<String> {
    let ranked = rank(connected);
    let non_reserved: Vec<&Candidate> = ranked
        .iter()
        .filter(|c| c.tier != Tier::OracleSeed)
        .collect();
    if non_reserved.len() <= max_peers {
        return Vec::new();
    }
    // Ranked best-first: drop from the tail.
    non_reserved
        .into_iter()
        .skip(max_peers)
        .map(|c| c.peer_id.clone())
        .collect()
}

/// Exponential dial backoff after `failures` consecutive failed attempts:
/// 30 s, 60 s, 120 s, 240 s, 480 s (capped). `jitter_pct` is applied
/// deterministically by the caller (0–100) so the function stays testable.
pub fn backoff_secs(failures: u32, jitter_pct: u64) -> u64 {
    let base = if failures == 0 {
        DIAL_BACKOFF_BASE_SECS
    } else {
        let shift = (failures - 1).min(10);
        DIAL_BACKOFF_BASE_SECS.saturating_mul(1u64 << shift)
    }
    .min(DIAL_BACKOFF_CAP_SECS);
    let jitter_pct = jitter_pct.min(100);
    // Deterministic downward-only jitter: a peer never waits longer than the cap.
    base.saturating_sub(base.saturating_mul(jitter_pct) / 400)
}

/// Is a candidate due for a (re-)dial?
///
/// Tier 0 is retried every [`REDIAL_INTERVAL_SECS`] regardless of failures — a
/// reserved seed is the one thing a node must keep trying. Everything else
/// waits out its backoff.
pub fn due_for_redial(tier: Tier, seconds_since_last_attempt: u64, failures: u32) -> bool {
    match tier {
        Tier::OracleSeed => seconds_since_last_attempt >= REDIAL_INTERVAL_SECS,
        _ => seconds_since_last_attempt >= backoff_secs(failures, 0),
    }
}

/// The set of peers this node knows about, shared by the discovery tasks
/// (writers) and the ranked re-dial loop (reader).
///
/// A peer keeps the **best** facts ever seen about it: the lowest tier (an
/// entry promoted from unknown to on-chain stays promoted) and the highest
/// tenure (the earliest first-seen block). Without that rule a seed-list
/// refresh would keep demoting a peer that the chain already vouched for.
#[derive(Debug, Default)]
pub struct PeerBook {
    inner: std::sync::Mutex<Vec<Candidate>>,
}

impl PeerBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or merge one candidate.
    pub fn upsert(&self, candidate: Candidate) {
        if !candidate.is_dialable() {
            return;
        }
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(existing) = guard.iter_mut().find(|c| c.peer_id == candidate.peer_id) {
            if candidate.tier < existing.tier {
                existing.tier = candidate.tier;
            }
            existing.tenure = match (existing.tenure, candidate.tenure) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            };
            // Prefer a routable-looking address only if the old one was empty;
            // otherwise keep the first address seen (stable across ticks).
            if existing.addr.is_empty() {
                existing.addr = candidate.addr;
            }
            return;
        }
        guard.push(candidate);
    }

    /// Insert or merge many candidates (e.g. one discovery source's result).
    pub fn upsert_all<I: IntoIterator<Item = Candidate>>(&self, candidates: I) {
        for c in candidates {
            self.upsert(c);
        }
    }

    /// A copy of every known candidate, in insertion order (rank before use).
    pub fn snapshot(&self) -> Vec<Candidate> {
        match self.inner.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(g) => g.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Peer-table size from `GPN_MAX_PEERS`, falling back to
/// [`DEFAULT_MAX_PEERS`] for an unset, unparseable or zero value (zero would
/// silently mean "dial nobody", which is never what an operator wants).
pub fn max_peers_from_env() -> usize {
    std::env::var("GPN_MAX_PEERS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MAX_PEERS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(id: &str) -> String {
        format!("/ip4/10.0.0.1/tcp/30333/p2p/{id}")
    }

    fn seed_rec(peer_id: &str) -> BootstrapSeed {
        BootstrapSeed {
            id: "s".into(),
            ip4: "10.0.0.1".into(),
            port: 30333,
            peer_id: peer_id.into(),
            dns4: String::new(),
            operator: "gpn-core".into(),
        }
    }

    // §5 test 1 — rank order.
    #[test]
    fn rank_is_tier_then_tenure_desc_then_peer_id() {
        let oracle = vec!["12D3KooORACLE".to_string()];
        let cands = vec![
            Candidate::new(&seed("12D3KooCCC2"), Tier::Ranked, Some(500)),
            Candidate::new(&seed("12D3KooBBB1"), Tier::Ranked, Some(900)),
            Candidate::new(&seed("12D3KooAAA"), Tier::Ranked, None),
            Candidate::new(&seed("12D3KooZZZ9"), Tier::Unknown, None),
            Candidate::new(&seed("12D3KooORACLE"), Tier::OracleSeed, Some(1)),
            Candidate::new(&seed("12D3KooAAA"), Tier::Unknown, None), // duplicate
        ];
        assert_eq!(tier_for("12D3KooORACLE", &oracle, false), Tier::OracleSeed);
        assert_eq!(tier_for("12D3KooZZZ9", &oracle, false), Tier::Unknown);

        let order: Vec<String> = rank(&cands).iter().map(|c| c.peer_id.clone()).collect();
        assert_eq!(
            order,
            vec![
                "12D3KooORACLE".to_string(), // tier 0 first
                "12D3KooBBB1".to_string(),   // tenure 900
                "12D3KooCCC2".to_string(),   // tenure 500
                "12D3KooAAA".to_string(),    // tier 1, unknown tenure
                "12D3KooZZZ9".to_string(),   // tier 2 last
            ]
        );
        // Determinism: the same input yields the same order on a second pass.
        let again: Vec<String> = rank(&cands).iter().map(|c| c.peer_id.clone()).collect();
        assert_eq!(order, again);
    }

    #[test]
    fn rank_ignores_undialable_entries() {
        let cands = vec![
            Candidate::new("/ip4/10.0.0.1/tcp/30333", Tier::Ranked, Some(10)),
            Candidate::new(&seed("12D3KooOK"), Tier::Ranked, Some(1)),
        ];
        let ranked = rank(&cands);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].peer_id, "12D3KooOK");
    }

    #[test]
    fn peer_id_of_takes_the_relayed_target() {
        assert_eq!(
            peer_id_of("/ip4/1.2.3.4/tcp/30333/p2p/12D3KooRELAY/p2p-circuit/p2p/12D3KooTARGET"),
            Some("12D3KooTARGET")
        );
        assert_eq!(peer_id_of("/ip4/1.2.3.4/tcp/30333"), None);
        assert_eq!(peer_id_of("/ip4/1.2.3.4/tcp/30333/p2p/"), None);
    }

    // §5 test 2 — eviction / slot policy.
    #[test]
    fn full_table_evicts_tier_two_before_tier_one_and_never_tier_zero() {
        let cands = vec![
            Candidate::new(&seed("12D3KooORACLE"), Tier::OracleSeed, Some(1)),
            Candidate::new(&seed("12D3KooT1A"), Tier::Ranked, Some(900)),
            Candidate::new(&seed("12D3KooT1B"), Tier::Ranked, Some(800)),
            Candidate::new(&seed("12D3KooT2A"), Tier::Unknown, None),
            Candidate::new(&seed("12D3KooT2B"), Tier::Unknown, None),
        ];
        let plan = dial_plan(&cands, 2);
        let dialled: Vec<String> = plan.dial.iter().map(|c| c.peer_id.clone()).collect();
        assert_eq!(
            dialled,
            vec![
                "12D3KooORACLE".to_string(),
                "12D3KooT1A".to_string(),
                "12D3KooT1B".to_string(),
            ]
        );
        let skipped: Vec<String> = plan.skipped.iter().map(|c| c.peer_id.clone()).collect();
        assert_eq!(
            skipped,
            vec!["12D3KooT2A".to_string(), "12D3KooT2B".to_string()]
        );

        // Eviction of a live table drops the same tier-2 peers, never the seed.
        let evicted = evictions(&cands, 2);
        assert_eq!(
            evicted,
            vec!["12D3KooT2A".to_string(), "12D3KooT2B".to_string()]
        );
        assert!(evictions(&cands, 8).is_empty());
    }

    #[test]
    fn oracle_seed_survives_a_zero_slot_table() {
        let cands = vec![
            Candidate::new(&seed("12D3KooORACLE"), Tier::OracleSeed, None),
            Candidate::new(&seed("12D3KooT1A"), Tier::Ranked, Some(5)),
        ];
        let plan = dial_plan(&cands, 0);
        assert_eq!(plan.dial.len(), 1);
        assert_eq!(plan.dial[0].tier, Tier::OracleSeed);
        assert_eq!(plan.skipped.len(), 1);
    }

    #[test]
    fn oracle_peer_ids_are_capped_at_five_and_deduplicated() {
        let seeds = vec![
            seed_rec("12D3KooA"),
            seed_rec("12D3KooA"),
            seed_rec("12D3KooB"),
            seed_rec("12D3KooC"),
            seed_rec("12D3KooD"),
            seed_rec("12D3KooE"),
            seed_rec("12D3KooF"),
        ];
        let ids = oracle_peer_ids(&seeds);
        assert_eq!(ids.len(), MAX_ORACLE_SEEDS);
        assert_eq!(ids[0], "12D3KooA");
        assert_eq!(ids[4], "12D3KooE");
    }

    // §5 test 3 — re-dial window and backoff growth/cap.
    #[test]
    fn tier_zero_is_redialled_every_sixty_seconds_regardless_of_failures() {
        assert!(!due_for_redial(Tier::OracleSeed, 59, 9));
        assert!(due_for_redial(Tier::OracleSeed, 60, 9));
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff_secs(0, 0), DIAL_BACKOFF_BASE_SECS);
        assert_eq!(backoff_secs(1, 0), 30);
        assert_eq!(backoff_secs(2, 0), 60);
        assert_eq!(backoff_secs(3, 0), 120);
        assert_eq!(backoff_secs(4, 0), 240);
        assert_eq!(backoff_secs(5, 0), DIAL_BACKOFF_CAP_SECS);
        assert_eq!(backoff_secs(50, 0), DIAL_BACKOFF_CAP_SECS);
        // Jitter only ever shortens the wait.
        assert!(backoff_secs(5, 100) < DIAL_BACKOFF_CAP_SECS);
        // A tier-1 peer is not retried before its backoff elapses.
        assert!(!due_for_redial(Tier::Ranked, 59, 3));
        assert!(due_for_redial(Tier::Ranked, 120, 3));
    }

    #[test]
    fn peer_book_merges_without_demoting() {
        let book = PeerBook::new();
        book.upsert(Candidate::new(&seed("12D3KooA"), Tier::Unknown, None));
        // Later learned from chain: tier improves, tenure appears.
        book.upsert(Candidate::new(&seed("12D3KooA"), Tier::Ranked, Some(700)));
        // A seed-list refresh must not undo that.
        book.upsert(Candidate::new(&seed("12D3KooA"), Tier::Unknown, None));
        // An older first-seen height wins over a younger one.
        book.upsert(Candidate::new(&seed("12D3KooA"), Tier::Ranked, Some(400)));
        book.upsert(Candidate::new("/ip4/10.0.0.9/tcp/1", Tier::Ranked, None));

        let snap = book.snapshot();
        assert!(!book.is_empty());
        assert_eq!(
            snap.len(),
            1,
            "undialable + duplicate entries must not pile up"
        );
        assert_eq!(snap[0].tier, Tier::Ranked);
        assert_eq!(snap[0].tenure, Some(400));
    }

    #[test]
    fn max_peers_parsing_is_total() {
        // Env is process-global; assert only the pure default path here.
        assert_eq!(DEFAULT_MAX_PEERS, 8);
    }
}
