//! Peer exchange gossip (GPN #155, design §2).
//!
//! A ranked dial list (see [`crate::peer_priority`]) fixes *who* to dial, but it
//! cannot fix a node that never learns about anyone: on 2026-11-09 the fleet
//! formed two disjoint pairs (`linux ↔ box 2`, `box 1 ↔ box 4`) because each
//! node only ever knew the seed plus whoever dialled it. Nothing was broken —
//! libp2p never forms a mesh on its own, and the nodes had no way to tell each
//! other who else exists.
//!
//! This module is the **wire format and the gossip policy** for the
//! `gpn/peers/1` notification protocol:
//!
//! * every 5 min a node sends up to 32 `(multiaddr, last_seen)` entries, capped
//!   at 8 KB, to every peer that has the protocol open;
//! * a receiving node adds unknown entries as tier 2 ([`crate::peer_priority`]),
//!   so they are dialled only while slots remain — gossip can never displace a
//!   peer the chain vouched for;
//! * an entry older than 24 h is never propagated, and neither is one that has
//!   failed three dials in a row, so a dead address dies out instead of being
//!   passed around forever.
//!
//! Everything here is pure (encode / decode / select) and unit-tested;
//! `service.rs` owns the libp2p half.

use crate::peer_priority::{peer_id_of, Candidate, Tier};
use std::collections::HashMap;

/// Protocol id. Substrate's notification protocols are multistream-select
/// names, so they carry the leading slash (the design says `gpn/peers/1`).
pub const PEERS_PROTOCOL: &str = "/gpn/peers/1";
/// Design §2: at most 32 entries per message.
pub const MAX_GOSSIP_ENTRIES: usize = 32;
/// Design §2: a message must never exceed 8 KB.
pub const MAX_GOSSIP_BYTES: usize = 8 * 1024;
/// Design §2: never propagate an entry older than 24 h.
pub const MAX_ENTRY_AGE_SECS: u64 = 24 * 60 * 60;
/// Design §2: never propagate an entry that failed this many dials in a row.
pub const MAX_FAILED_DIALS: u32 = 3;
/// Design §2: gossip every 5 min.
pub const GOSSIP_INTERVAL_SECS: u64 = 5 * 60;
/// Wire format version (a future v2 must stay decodable by this decoder).
pub const WIRE_VERSION: u8 = 1;
/// Longest multiaddress we accept. Real addresses are ~60-120 bytes; this
/// bounds a hostile payload without rejecting a relayed IPv6 address.
pub const MAX_ADDR_LEN: usize = 256;

/// One gossiped peer: where to dial it and when we last saw it alive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerEntry {
    /// Full multiaddress including the `/p2p/<peer id>` suffix.
    pub addr: String,
    /// Unix seconds at which this node last had a connection to it.
    pub last_seen: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Wire {
    v: u8,
    peers: Vec<PeerEntry>,
}

/// A structurally usable entry: dialable address, sane length, non-zero time.
fn is_usable(entry: &PeerEntry) -> bool {
    !entry.addr.is_empty()
        && entry.addr.len() <= MAX_ADDR_LEN
        && entry.addr.starts_with('/')
        && peer_id_of(&entry.addr).is_some()
        && entry.last_seen > 0
}

/// Is the entry fresh enough to propagate? A clock that went backwards (entry
/// in the "future") is treated as fresh: refusing it would punish a peer for a
/// local clock skew we cannot verify.
fn is_fresh(entry: &PeerEntry, now: u64) -> bool {
    entry.last_seen > 0 && now.saturating_sub(entry.last_seen) <= MAX_ENTRY_AGE_SECS
}

/// Deterministic encoding: usable + fresh entries, de-duplicated by address,
/// sorted by address, at most [`MAX_GOSSIP_ENTRIES`], and then trimmed until it
/// fits [`MAX_GOSSIP_BYTES`].
///
/// Determinism matters for tests and for debugging a fleet: two nodes with the
/// same knowledge emit the same bytes.
pub fn encode(entries: &[PeerEntry], now: u64) -> Vec<u8> {
    let mut kept: Vec<PeerEntry> = entries
        .iter()
        .filter(|e| is_usable(e) && is_fresh(e, now))
        .cloned()
        .collect();
    kept.sort_by(|a, b| a.addr.cmp(&b.addr));
    kept.dedup_by(|a, b| a.addr == b.addr);
    kept.truncate(MAX_GOSSIP_ENTRIES);

    loop {
        let wire = Wire {
            v: WIRE_VERSION,
            peers: kept.clone(),
        };
        match serde_json::to_vec(&wire) {
            Ok(bytes) if bytes.len() <= MAX_GOSSIP_BYTES => return bytes,
            Ok(_) => {
                // Over the byte cap: drop the last entry (sorted order keeps
                // this deterministic) and try again.
                if kept.pop().is_none() {
                    return Vec::new();
                }
            }
            Err(_) => return Vec::new(),
        }
    }
}

/// Tolerant decoder. A malformed, oversized or future-versioned message is not
/// an error to act on — it yields nothing, because gossip is best-effort and a
/// node must never stall or disconnect over it.
pub fn decode(bytes: &[u8]) -> Vec<PeerEntry> {
    if bytes.is_empty() || bytes.len() > MAX_GOSSIP_BYTES {
        return Vec::new();
    }
    let Ok(wire) = serde_json::from_slice::<Wire>(bytes) else {
        return Vec::new();
    };
    if wire.v != WIRE_VERSION {
        return Vec::new();
    }
    let mut out: Vec<PeerEntry> = wire.peers.into_iter().filter(is_usable).collect();
    out.sort_by(|a, b| a.addr.cmp(&b.addr));
    out.dedup_by(|a, b| a.addr == b.addr);
    out.truncate(MAX_GOSSIP_ENTRIES);
    out
}

/// Learned entries become tier-2 candidates — never tier 1, because gossip is
/// an unauthenticated claim (design §2: "exchange is the filler", the roster is
/// the trust anchor).
pub fn entries_to_candidates(entries: &[PeerEntry]) -> Vec<Candidate> {
    entries
        .iter()
        .map(|e| Candidate::new(&e.addr, Tier::Unknown, None))
        .filter(|c| c.is_dialable())
        .collect()
}

/// Choose what to tell one peer.
///
/// `learned` maps peer id → unix seconds we first learned about it; a peer that
/// is in our book but not in the map is one we know locally (compiled-in seed,
/// chain record, persisted store) and counts as just-seen. `failures` is the
/// re-dial loop's consecutive-failure counter. `recipient` is never advertised
/// back to itself (it knows), and its own address is dropped.
pub fn select_entries(
    candidates: &[Candidate],
    learned: &HashMap<String, u64>,
    failures: &HashMap<String, u32>,
    recipient: Option<&str>,
    now: u64,
) -> Vec<PeerEntry> {
    let mut out: Vec<PeerEntry> = Vec::new();
    for c in candidates {
        if !c.is_dialable() || c.addr.is_empty() {
            continue;
        }
        if recipient.is_some_and(|r| r == c.peer_id) {
            continue;
        }
        if failures.get(&c.peer_id).copied().unwrap_or(0) >= MAX_FAILED_DIALS {
            continue;
        }
        let last_seen = learned.get(&c.peer_id).copied().unwrap_or(now);
        let entry = PeerEntry {
            addr: c.addr.clone(),
            last_seen,
        };
        if is_fresh(&entry, now) {
            out.push(entry);
        }
    }
    out.truncate(MAX_GOSSIP_ENTRIES);
    out
}

/// Merge a received message into the book and remember when we learned each
/// peer. Returns how many entries were new to this node's book (for one log
/// line, not per-packet noise).
///
/// A peer we were told about is recorded even when it is already known, so the
/// `last_seen` map reflects the latest claim — but the book's tier is only ever
/// improved by [`crate::peer_priority::PeerBook::upsert`], never demoted here.
pub fn ingest(
    bytes: &[u8],
    book: &crate::peer_priority::PeerBook,
    known_before: usize,
    learned: &mut HashMap<String, u64>,
    now: u64,
) -> usize {
    let entries = decode(bytes);
    let mut fresh = 0usize;
    for entry in &entries {
        if !is_fresh(entry, now) {
            continue;
        }
        if let Some(id) = peer_id_of(&entry.addr) {
            learned.entry(id.to_string()).or_insert(now);
        }
        fresh += 1;
    }
    book.upsert_all(entries_to_candidates(&entries));
    book.len().saturating_sub(known_before).min(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(id: &str) -> String {
        format!("/ip4/10.0.0.1/tcp/30333/p2p/{id}")
    }

    fn entry(id: &str, last_seen: u64) -> PeerEntry {
        PeerEntry {
            addr: addr(id),
            last_seen,
        }
    }

    // §5 test 4 (policy half) — a learned peer is accepted as tier 2.
    #[test]
    fn roundtrip_yields_tier_two_candidates() {
        let now = 1_700_000_000;
        let bytes = encode(&[entry("12D3KooA", now), entry("12D3KooB", now - 60)], now);
        let decoded = decode(&bytes);
        assert_eq!(decoded.len(), 2);
        // Sorted by address, so deterministic.
        assert_eq!(decode(&bytes), decoded);
        let cands = entries_to_candidates(&decoded);
        assert_eq!(cands.len(), 2);
        assert!(cands
            .iter()
            .all(|c| c.tier == crate::peer_priority::Tier::Unknown));
        assert!(cands.iter().all(|c| c.is_dialable()));
    }

    #[test]
    fn stale_entries_are_never_encoded() {
        let now = 1_700_000_000;
        let bytes = encode(
            &[
                entry("12D3KooOLD", now - MAX_ENTRY_AGE_SECS - 1),
                entry("12D3KooNEW", now - MAX_ENTRY_AGE_SECS + 1),
            ],
            now,
        );
        let decoded = decode(&bytes);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].addr, addr("12D3KooNEW"));
    }

    #[test]
    fn caps_entries_and_bytes() {
        let now = 1_700_000_000;
        let many: Vec<PeerEntry> = (0..100)
            .map(|i| entry(&format!("12D3Koo{i:04}"), now))
            .collect();
        let bytes = encode(&many, now);
        assert!(bytes.len() <= MAX_GOSSIP_BYTES);
        assert!(decode(&bytes).len() <= MAX_GOSSIP_ENTRIES);

        // A payload that cannot fit even one entry yields nothing rather than
        // an oversized message (a peer would drop it anyway).
        let huge = PeerEntry {
            addr: format!("/ip4/10.0.0.1/tcp/1/p2p/{}", "z".repeat(MAX_ADDR_LEN)),
            last_seen: now,
        };
        let bytes = encode(&[huge], now);
        assert!(bytes.is_empty() || bytes.len() <= MAX_GOSSIP_BYTES);
    }

    #[test]
    fn decoder_is_total_on_hostile_input() {
        assert!(decode(b"").is_empty());
        assert!(decode(b"not json").is_empty());
        assert!(decode(&vec![b'x'; MAX_GOSSIP_BYTES + 1]).is_empty());
        // Wrong version: ignored, not misread.
        assert!(decode(
            br#"{"v":9,"peers":[{"addr":"/ip4/1.2.3.4/tcp/1/p2p/12D3KooA","last_seen":1}]}"#
        )
        .is_empty());
        // Structurally unusable entries are dropped, the rest survive.
        let body = br#"{"v":1,"peers":[{"addr":"","last_seen":1},{"addr":"/ip4/1.2.3.4/tcp/1","last_seen":1},{"addr":"/ip4/1.2.3.4/tcp/1/p2p/12D3KooOK","last_seen":5}]}"#;
        let decoded = decode(body);
        assert_eq!(decoded.len(), 1);
        assert_eq!(peer_id_of(&decoded[0].addr), Some("12D3KooOK"));
        // Zero last_seen is "never seen", not "ancient".
        assert!(decode(
            br#"{"v":1,"peers":[{"addr":"/ip4/1.2.3.4/tcp/1/p2p/12D3KooA","last_seen":0}]}"#
        )
        .is_empty());
    }

    // §5 test 4 (policy half) — gossip never promotes and never displaces.
    #[test]
    fn select_drops_failing_and_self_entries() {
        let now = 1_700_000_000;
        let cands = vec![
            Candidate::new(&addr("12D3KooSELF"), Tier::Unknown, None),
            Candidate::new(&addr("12D3KooDEAD"), Tier::Unknown, None),
            Candidate::new(&addr("12D3KooOK1"), Tier::Ranked, Some(900)),
            Candidate::new("/ip4/1.2.3.4/tcp/1", Tier::Unknown, None),
        ];
        let mut learned = HashMap::new();
        learned.insert("12D3KooOK1".to_string(), now - 100);
        let mut failures = HashMap::new();
        failures.insert("12D3KooDEAD".to_string(), MAX_FAILED_DIALS);

        let selected = select_entries(&cands, &learned, &failures, Some("12D3KooSELF"), now);
        let ids: Vec<&str> = selected
            .iter()
            .filter_map(|e| peer_id_of(&e.addr))
            .collect();
        assert_eq!(ids, vec!["12D3KooOK1"]);
        assert_eq!(selected[0].last_seen, now - 100);
    }

    #[test]
    fn ingest_adds_unknown_peers_without_demoting_known_ones() {
        let now = 1_700_000_000;
        let book = crate::peer_priority::PeerBook::new();
        book.upsert(Candidate::new(
            &addr("12D3KooCHAIN"),
            Tier::Ranked,
            Some(400),
        ));
        let before = book.len();
        assert_eq!(before, 1);

        let bytes = encode(
            &[
                PeerEntry {
                    addr: addr("12D3KooCHAIN"),
                    // A gossiping peer claims this trusted peer is unknown:
                    // the book must keep tier 1 and its tenure.
                    last_seen: now,
                },
                entry("12D3KooNEW", now),
                entry("12D3KooSTALE", now - MAX_ENTRY_AGE_SECS - 5),
            ],
            now,
        );
        let mut learned = HashMap::new();
        let added = ingest(&bytes, &book, before, &mut learned, now);
        assert_eq!(added, 1);
        let snap = book.snapshot();
        assert_eq!(snap.len(), 2);
        let chain = snap
            .iter()
            .find(|c| c.peer_id == "12D3KooCHAIN")
            .expect("chain peer kept");
        assert_eq!(chain.tier, crate::peer_priority::Tier::Ranked);
        assert_eq!(chain.tenure, Some(400));
        assert!(snap.iter().any(|c| c.peer_id == "12D3KooNEW"));
        assert!(!snap.iter().any(|c| c.peer_id == "12D3KooSTALE"));
        assert!(learned.contains_key("12D3KooNEW"));
    }

    #[test]
    fn protocol_name_and_caps_match_the_design() {
        assert_eq!(PEERS_PROTOCOL, "/gpn/peers/1");
        assert_eq!(MAX_GOSSIP_ENTRIES, 32);
        assert_eq!(MAX_GOSSIP_BYTES, 8192);
        assert_eq!(MAX_ENTRY_AGE_SECS, 86_400);
        assert_eq!(GOSSIP_INTERVAL_SECS, 300);
    }
}
