//! Seed-list policy — turn on-chain liveness into a published `seeds.json`
//! (design `design-network-membership-bootstrap.md` §3.4).
//!
//! **Seed is earned, not asserted.** Nobody hand-curates the dynamic list: a
//! node is a *candidate* only when the chain already proves it is reachable
//! (distinct bonded peer witnesses) and reliably online (rolling uptime). The
//! inputs are on-chain, so anyone can audit — or re-publish — the list, and a
//! captured publisher cannot inject a node that peers would accept (the libp2p
//! handshake is the real check, `peer_id` is in the multiaddr).
//!
//! This module is deliberately pure: the chain read lives in
//! `ChainClient::list_liveness_nodes`, the JSON write in
//! `gpn_seedlist`. Everything policy-shaped is here, unit-tested.

use serde_json::json;

/// Distinct bonded witnesses required before a node may be published.
/// Matches the pallet's `MinWitnesses` default.
pub const MIN_WITNESSES: u32 = 3;
/// Minimum rolling uptime (basis points; 9_500 = 95%).
pub const MIN_UPTIME_BPS: u32 = 9_500;
/// Published list size (top-N by uptime, then confirmed-online history).
pub const MAX_SEEDS: usize = 16;
/// Advertised cache lifetime for the published JSON, in seconds.
pub const DEFAULT_TTL: u64 = 300;

/// One full node as the chain describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLiveness {
    /// ss58 address of the registered (bonded) account.
    pub account: String,
    /// Declared public libp2p endpoint (multiaddr) — may be empty/placeholder.
    pub endpoint: String,
    /// Liveness record still active (not slashed / unregistered).
    pub active: bool,
    /// Rolling uptime in basis points (already divided by the lookback).
    pub uptime_bps: u32,
    /// Raw uptime bitmask, used only as a deterministic tie-break.
    pub uptime_bits: u32,
    /// Window id of the last window this node was confirmed online.
    pub last_confirmed_window: u64,
    /// Distinct witnesses that saw this node reachable in the current window.
    pub reachable: u32,
}

/// A candidate that survived the promotion rule, ready to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedEntry {
    pub account: String,
    pub multiaddr: String,
    pub uptime_bps: u32,
    pub last_confirmed_window: u64,
}

/// Accept only a dialable libp2p multiaddr.
///
/// The on-chain endpoint is whatever the operator registered: real backbone
/// nodes register `/ip4/…/tcp/…/p2p/…`, but a node that has not published one
/// still has a *placeholder* (or an http URL from older tooling). Anything
/// without a `/p2p/<peer-id>` suffix is undialable by `discovery`, so it must
/// never reach `seeds.json` — publishing it would waste every client a dial
/// timeout and teach nothing.
pub fn multiaddr_of(endpoint: &str) -> Option<String> {
    let e = endpoint.trim();
    if !e.starts_with('/') || e.contains(char::is_whitespace) {
        return None;
    }
    // `/p2p/<id>` must be the LAST component and non-empty.
    let (_, peer) = e.rsplit_once("/p2p/")?;
    if peer.is_empty() || peer.contains('/') {
        return None;
    }
    Some(e.to_string())
}

/// Protocols a published p2p multiaddr may contain.
pub const P2P_PROTOCOLS: &[&str] = &[
    "ip4", "ip6", "dns", "dns4", "dns6", "tcp", "udp", "quic", "quic-v1", "p2p", "tls", "sni",
    "ws", "wss",
];

/// Accept only a plausible libp2p peer id (base58, ed25519 `12D3Koo…` or the
/// legacy multihash form `Qm…`).
pub fn clean_peer_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if !(40..=64).contains(&s.len()) || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    if s.starts_with("12D3Koo") || s.starts_with("Qm") {
        Some(s.to_string())
    } else {
        None
    }
}

/// The **access point's** structural multiaddr gate, as a library function.
///
/// One definition, two users: the provider heartbeat emits addresses and the
/// scheduler/access point consumes them. Keeping the rules here lets a
/// round-trip test prove the emitter produces something the consumer accepts —
/// the bug that made every provider look peer-less was exactly a mismatch
/// between these two (a doubled `/p2p/<id>/p2p/<id>` suffix that the gate
/// rightly rejected).
///
/// Rules: `/proto/value/…` pairs only, whitelisted protocols, a numeric
/// TCP/UDP port in range, and exactly one `/p2p/<peer-id>` as the final
/// component.
pub fn clean_published_multiaddr(raw: &str) -> Option<String> {
    let s = raw.trim();
    if !s.starts_with('/') || s.len() > 256 {
        return None;
    }
    if s.chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\' || c == '"')
    {
        return None;
    }
    let parts: Vec<&str> = s.split('/').skip(1).collect();
    if parts.iter().any(|p| p.is_empty()) || parts.len() < 4 {
        return None;
    }
    let mut i = 0;
    let mut has_p2p = false;
    while i + 1 < parts.len() {
        let proto = parts[i];
        let value = parts[i + 1];
        if !P2P_PROTOCOLS.contains(&proto) {
            return None;
        }
        match proto {
            "tcp" | "udp" => {
                let port: u32 = value.parse().ok()?;
                if !(1..=65535).contains(&port) {
                    return None;
                }
            }
            "p2p" => {
                // Must be the last component, and a real peer id.
                if i + 2 != parts.len() || clean_peer_id(value).is_none() {
                    return None;
                }
                has_p2p = true;
            }
            _ => {}
        }
        i += 2;
    }
    if i != parts.len() || !has_p2p {
        return None;
    }
    Some(s.to_string())
}

/// The promotion rule from the design, as one predicate.
///
/// `current_window` comes from `block_number / LivenessWindow`. A node confirmed
/// online in the previous window still counts (`last_confirmed_window + 1 >=
/// current_window`): finalisation lags by one window, so demanding the *current*
/// window would publish nothing at all.
pub fn is_candidate(
    node: &NodeLiveness,
    current_window: u64,
    min_witnesses: u32,
    min_uptime_bps: u32,
) -> bool {
    if !node.active || node.reachable < min_witnesses || node.uptime_bps < min_uptime_bps {
        return false;
    }
    if multiaddr_of(&node.endpoint).is_none() {
        return false;
    }
    node.last_confirmed_window.saturating_add(1) >= current_window
}

/// Filter + rank + cap. Ranking is `(uptime_bps, then confirmed window, then
/// uptime_bits, then account)` — fully deterministic so two publishers holding
/// the same chain state emit byte-identical lists (no churn for clients).
pub fn rank(
    nodes: &[NodeLiveness],
    current_window: u64,
    min_witnesses: u32,
    min_uptime_bps: u32,
    max: usize,
) -> Vec<SeedEntry> {
    let mut out: Vec<SeedEntry> = nodes
        .iter()
        .filter(|n| is_candidate(n, current_window, min_witnesses, min_uptime_bps))
        .filter_map(|n| {
            multiaddr_of(&n.endpoint).map(|multiaddr| SeedEntry {
                account: n.account.clone(),
                multiaddr,
                uptime_bps: n.uptime_bps,
                last_confirmed_window: n.last_confirmed_window,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.uptime_bps
            .cmp(&a.uptime_bps)
            .then(b.last_confirmed_window.cmp(&a.last_confirmed_window))
            .then(a.multiaddr.cmp(&b.multiaddr))
            .then(a.account.cmp(&b.account))
    });
    out.truncate(max);
    out
}

/// Build the published document.
///
/// Shape matches what `discovery.rs` already parses: `multiaddrs` first-class,
/// extra metadata ignored by older clients. It is deliberately *only* the
/// dynamic set — the hardcoded ≤5 seeds (Source A) are shipped in the binary and
/// must keep working when this pipeline is broken, so they are never "managed"
/// here.
pub fn seeds_json(
    network: &str,
    chain_id: &str,
    genesis: &str,
    updated: u64,
    ttl: u64,
    publisher: &str,
    current_window: u64,
    entries: &[SeedEntry],
) -> serde_json::Value {
    json!({
        "network": network,
        "chain_id": chain_id,
        "genesis": genesis,
        "updated": updated,
        "ttl": ttl,
        "publisher": publisher,
        "source": "on-chain liveness (gpn_seedlist)",
        "window": current_window,
        "count": entries.len(),
        "multiaddrs": entries.iter().map(|e| e.multiaddr.clone()).collect::<Vec<_>>(),
        "seeds": entries
            .iter()
            .map(|e| json!({
                "account": e.account,
                "multiaddr": e.multiaddr,
                "uptime_bps": e.uptime_bps,
                "last_confirmed_window": e.last_confirmed_window,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(account: &str, endpoint: &str, uptime_bps: u32, reachable: u32) -> NodeLiveness {
        NodeLiveness {
            account: account.to_string(),
            endpoint: endpoint.to_string(),
            active: true,
            uptime_bps,
            uptime_bits: 0,
            last_confirmed_window: 10,
            reachable,
        }
    }

    const GOOD: &str = "/ip4/198.51.100.22/tcp/30333/p2p/12D3KooWAbc";

    #[test]
    fn multiaddr_requires_a_peer_id_suffix() {
        assert_eq!(multiaddr_of(GOOD).as_deref(), Some(GOOD));
        assert_eq!(
            multiaddr_of(" /dns4/seed.gpn.network/tcp/30333/p2p/12D3KooWX \n").as_deref(),
            Some("/dns4/seed.gpn.network/tcp/30333/p2p/12D3KooWX")
        );
        // Undialable shapes must be rejected.
        assert!(multiaddr_of("").is_none());
        assert!(multiaddr_of("http://1.2.3.4:8093/status").is_none());
        assert!(multiaddr_of("/ip4/1.2.3.4/tcp/30333").is_none());
        assert!(multiaddr_of("/ip4/1.2.3.4/tcp/30333/p2p/").is_none());
        assert!(multiaddr_of("/ip4/1.2.3.4/tcp/30333/p2p/id/extra").is_none());
        assert!(multiaddr_of("/ip4/1.2.3.4/tcp/30333/p2p/id with space").is_none());
    }

    #[test]
    fn promotion_needs_witnesses_uptime_freshness_and_a_dialable_endpoint() {
        let good = node("a", GOOD, 9_900, 3);
        assert!(is_candidate(&good, 11, MIN_WITNESSES, MIN_UPTIME_BPS));

        // Too few distinct witnesses.
        assert!(!is_candidate(&node("a", GOOD, 9_900, 2), 11, 3, 9_500));
        // Uptime below the floor.
        assert!(!is_candidate(&node("a", GOOD, 9_400, 9), 11, 3, 9_500));
        // Inactive (slashed / unregistered).
        let mut inactive = good.clone();
        inactive.active = false;
        assert!(!is_candidate(&inactive, 11, 3, 9_500));
        // Placeholder endpoint.
        assert!(!is_candidate(
            &node("a", "placeholder", 9_900, 9),
            11,
            3,
            9_500
        ));
        // Stale: last confirmed two windows back (finalisation lags one).
        let mut stale = good.clone();
        stale.last_confirmed_window = 9;
        assert!(!is_candidate(&stale, 11, 3, 9_500));
        // ...but one window back still counts.
        let mut lagging = good.clone();
        lagging.last_confirmed_window = 10;
        assert!(is_candidate(&lagging, 11, 3, 9_500));
    }

    #[test]
    fn ranking_is_deterministic_and_capped() {
        let nodes = vec![
            node("b", "/ip4/10.0.0.2/tcp/30333/p2p/12D3KooB", 9_600, 5),
            node("a", "/ip4/10.0.0.1/tcp/30333/p2p/12D3KooA", 9_900, 5),
            node("c", "/ip4/10.0.0.3/tcp/30333/p2p/12D3KooC", 9_600, 5),
            node("d", "http://10.0.0.4:8093/status", 10_000, 9), // not dialable
            node("e", "/ip4/10.0.0.5/tcp/30333/p2p/12D3KooE", 9_000, 9), // uptime floor
        ];
        let ranked = rank(&nodes, 11, MIN_WITNESSES, MIN_UPTIME_BPS, MAX_SEEDS);
        let addrs: Vec<&str> = ranked.iter().map(|e| e.multiaddr.as_str()).collect();
        assert_eq!(
            addrs,
            vec![
                "/ip4/10.0.0.1/tcp/30333/p2p/12D3KooA", // 9900
                // 9600 ties broken by multiaddr (deterministic)
                "/ip4/10.0.0.2/tcp/30333/p2p/12D3KooB",
                "/ip4/10.0.0.3/tcp/30333/p2p/12D3KooC",
            ]
        );
        // Stable across input permutation.
        let mut shuffled = nodes.clone();
        shuffled.reverse();
        let again = rank(&shuffled, 11, MIN_WITNESSES, MIN_UPTIME_BPS, MAX_SEEDS);
        assert_eq!(again, ranked);
        // Cap is honoured.
        assert_eq!(rank(&nodes, 11, MIN_WITNESSES, MIN_UPTIME_BPS, 1).len(), 1);
    }

    #[test]
    fn published_json_has_the_shape_clients_already_parse() {
        let entries = rank(
            &[node("a", GOOD, 9_900, 4)],
            11,
            MIN_WITNESSES,
            MIN_UPTIME_BPS,
            MAX_SEEDS,
        );
        let doc = seeds_json(
            "GPN Public Testnet",
            "gpn_testnet",
            "0xb8e4",
            1_700_000_000,
            DEFAULT_TTL,
            "gpn_seedlist",
            11,
            &entries,
        );
        // `multiaddrs` is the key discovery.rs looks for first.
        assert_eq!(doc["multiaddrs"][0], GOOD);
        assert_eq!(doc["count"], 1);
        assert_eq!(doc["ttl"], DEFAULT_TTL);
        assert_eq!(doc["genesis"], "0xb8e4");
        assert_eq!(doc["seeds"][0]["uptime_bps"], 9_900);
        // An empty list must still be a valid document (publishers are allowed
        // to publish "nobody qualifies" — clients fall back to Source A).
        let empty = seeds_json("n", "c", "g", 0, 60, "p", 0, &[]);
        assert_eq!(empty["multiaddrs"].as_array().unwrap().len(), 0);
        assert_eq!(empty["count"], 0);
    }
}
