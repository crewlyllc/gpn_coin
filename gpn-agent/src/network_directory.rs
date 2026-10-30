//! The live network directory behind `GET /network` on the access point.
//!
//! The public `/nodes` page used to render `/seeds.json`, which is a
//! hand-maintained list of *dial targets*: compiled-in bootnodes plus whatever
//! the AP's provider registry happened to hold. That conflated three different
//! questions and answered all of them wrong (2026-10-30):
//!
//!   * the same machine appeared several times (a compiled-in seed, a
//!     `bootstrap_seeds` entry and an AP provider row, all one peer id);
//!   * a compiled-in seed that is no longer a peer (`WRWv4yV…:30334`) was
//!     presented as if it were live;
//!   * a peer that is connected but publishes no dialable multiaddr (box #1)
//!     was invisible, so "3 nodes" was shown while the chain saw 4 computers.
//!
//! This module answers the question the page actually asks — **who is on the
//! network right now** — and is deliberately pure: the chain read
//! (`system_peers` / `system_localPeerId`) and the provider registry live in
//! the caller, so every rule here is unit-tested without a node.

use serde_json::{json, Value};

/// One AP-registered provider, flattened for publication.
#[derive(Debug, Clone, Default)]
pub struct DirProvider {
    pub address: String,
    pub peer_id: Option<String>,
    pub healthy: bool,
    pub vram_mb: u64,
    pub models: Vec<String>,
    pub multiaddrs: Vec<String>,
    pub last_seen_secs: Option<u64>,
    pub status_url: String,
}

/// A dial target the network ships or advertises, with where it came from.
#[derive(Debug, Clone, Default)]
pub struct KnownSeed {
    pub multiaddr: String,
    /// `compiled-in` | `bootstrap-seed` | `provider`.
    pub source: String,
}

/// The `/network` document.
///
/// `live_peers` is a `Result` on purpose: an RPC failure must be visible as
/// `"stale": true`, not silently rendered as "no peers online".
pub fn build(
    generated_at: &str,
    self_peer_id: Option<&str>,
    live_peers: Result<Vec<String>, String>,
    providers: &[DirProvider],
    seeds: &[KnownSeed],
) -> Value {
    let stale = live_peers.is_err();
    let mut live_order: Vec<String> = Vec::new();
    let mut live_set = std::collections::HashSet::new();
    let mut rpc_error: Option<String> = None;
    match live_peers {
        Ok(ids) => {
            for id in ids {
                if let Some(pid) = clean_peer_id(&id) {
                    if live_set.insert(pid.clone()) {
                        live_order.push(pid);
                    }
                }
            }
        }
        Err(e) => rpc_error = Some(e),
    }
    // The local node is not in its own `system_peers`; count it once. Skipped
    // when the RPC read failed — a node that cannot read its own peer list is
    // not evidence that it is connected.
    if !stale {
        if let Some(pid) = self_peer_id.and_then(clean_peer_id) {
            if live_set.insert(pid.clone()) {
                live_order.push(pid);
            }
        }
    }

    // ---- seeds: dialable entries only, deduped by peer id ------------------
    let mut seed_rows: Vec<Value> = Vec::new();
    let mut seen_seed_peers = std::collections::HashSet::new();
    for s in seeds {
        let Some(ma) = dialable(&s.multiaddr) else {
            continue;
        };
        let Some(pid) = peer_id_of(&ma) else {
            continue;
        };
        if !seen_seed_peers.insert(pid.clone()) {
            continue; // one row per machine, first (most canonical) source wins
        }
        seed_rows.push(json!({
            "multiaddr": ma,
            "peer_id": pid,
            "live": live_set.contains(&pid),
            "source": s.source,
        }));
    }

    // ---- providers: deduped by peer id, healthy one wins -------------------
    let mut provider_rows: Vec<Value> = Vec::new();
    let mut provider_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for p in providers {
        let pid = p.peer_id.as_deref().and_then(clean_peer_id);
        let key = pid
            .clone()
            .unwrap_or_else(|| format!("url:{}", p.status_url.trim_end_matches('/')));
        let row = json!({
            "address": p.address,
            "peer_id": pid,
            "healthy": p.healthy,
            "vram_mb": p.vram_mb,
            "models": p.models,
            "multiaddrs": dialable_list(&p.multiaddrs),
            "last_seen_secs": p.last_seen_secs,
            "status_url": p.status_url,
        });
        match provider_index.get(&key) {
            // Same machine registered twice (e.g. two accounts, pull + push):
            // keep one row, preferring the healthy record.
            Some(&i) => {
                let existing_healthy = provider_rows[i]["healthy"].as_bool().unwrap_or(false);
                if p.healthy && !existing_healthy {
                    provider_rows[i] = row;
                }
            }
            None => {
                provider_index.insert(key, provider_rows.len());
                provider_rows.push(row);
            }
        }
    }

    // ---- classify ----------------------------------------------------------
    let serving_ai = provider_rows
        .iter()
        .filter(|p| p["healthy"].as_bool().unwrap_or(false))
        .count();

    let mut known_offline: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in &seed_rows {
        if let Some(pid) = row["peer_id"].as_str() {
            if !live_set.contains(pid) {
                known_offline.insert(pid.to_string());
            }
        }
    }
    for row in &provider_rows {
        if let Some(pid) = row["peer_id"].as_str() {
            if !live_set.contains(pid) {
                known_offline.insert(pid.to_string());
            }
        }
    }

    let mut doc = json!({
        "generated_at": generated_at,
        "peers": if stale { 0 } else { live_order.len() },
        "peer_ids": if stale { Vec::<String>::new() } else { live_order },
        "providers": provider_rows,
        "seeds": seed_rows,
        "counts": {
            "peers_live": if stale { 0 } else { live_set.len() },
            "serving_ai": serving_ai,
            "offline": known_offline.len(),
        },
        "stale": stale,
    });
    if let Some(e) = rpc_error {
        doc["error"] = json!(format!("chain RPC read failed: {e}"));
    }
    doc
}

/// A multiaddr is dialable when it ends in exactly one `/p2p/<peer-id>`.
fn dialable(ma: &str) -> Option<String> {
    crate::seedlist::clean_published_multiaddr(ma)
}

fn dialable_list(mas: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for ma in mas {
        if let Some(clean) = dialable(ma) {
            if !out.contains(&clean) {
                out.push(clean);
            }
        }
    }
    out
}

/// The peer id at the end of a multiaddr, if it is a plausible one.
pub fn peer_id_of(ma: &str) -> Option<String> {
    let (_, raw) = ma.rsplit_once("/p2p/")?;
    clean_peer_id(raw)
}

/// Accept only a plausible libp2p peer id (mirrors `seedlist::clean_peer_id`).
pub fn clean_peer_id(raw: &str) -> Option<String> {
    crate::seedlist::clean_peer_id(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SELF: &str = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
    const BOX1: &str = "12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz";
    const PEER_B: &str = "12D3KooWRfKQbKnynFsKVyi76FjFDWLQqQPTKZFrMFF6Zt7zoZ2o";
    const PEER_C: &str = "12D3KooWRhz9gbuEZpXAxfGeudnN1JQYw8D5nqPXTbWJQZiSbDiJ";
    const STALE: &str = "12D3KooWRWv4yVdJGiVg7xRgdUJY9G57EeutB596gWVpAwGC9qQ2";

    fn ma(host: &str, port: u16, pid: &str) -> String {
        format!("/ip4/{host}/tcp/{port}/p2p/{pid}")
    }

    fn provider(peer: &str, healthy: bool, url: &str) -> DirProvider {
        DirProvider {
            address: "5Addr".into(),
            peer_id: Some(peer.to_string()),
            healthy,
            vram_mb: 8188,
            models: vec!["llama3.2:1b".into()],
            multiaddrs: vec![ma("142.129.54.88", 30333, peer)],
            last_seen_secs: Some(3),
            status_url: url.into(),
        }
    }

    fn seeds_typical() -> Vec<KnownSeed> {
        vec![
            KnownSeed {
                multiaddr: format!("/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/{SELF}"),
                source: "compiled-in".into(),
            },
            // Same machine again, different source: must dedupe.
            KnownSeed {
                multiaddr: ma("142.129.54.88", 30333, SELF),
                source: "bootstrap-seed".into(),
            },
            // Stale compiled-in seed: kept, flagged not live.
            KnownSeed {
                multiaddr: ma("142.129.54.88", 30334, STALE),
                source: "compiled-in".into(),
            },
            // Not a real multiaddr: dropped entirely.
            KnownSeed {
                multiaddr: "http://203.0.113.9:8093/status".into(),
                source: "provider".into(),
            },
        ]
    }

    #[test]
    fn dedupes_same_machine_published_as_seed_and_provider() {
        let live = Ok(vec![BOX1.into(), PEER_B.into(), PEER_C.into()]);
        let doc = build(
            "2026-10-30T20:00:00Z",
            Some(SELF),
            live,
            &[
                provider(SELF, true, "http://127.0.0.1:8093"),
                provider(SELF, true, "pull://5Fh7"),
            ],
            &seeds_typical(),
        );
        // 3 remote peers + this box.
        assert_eq!(doc["counts"]["peers_live"], 4);
        assert_eq!(doc["peers"], 4);
        // One seed row for this box (dns4 wins over ip4) and one stale seed.
        assert_eq!(doc["seeds"].as_array().unwrap().len(), 2);
        assert_eq!(doc["seeds"][0]["peer_id"], SELF);
        assert_eq!(doc["seeds"][0]["live"], true);
        assert_eq!(doc["seeds"][0]["source"], "compiled-in");
        // Two provider registrations, one machine.
        assert_eq!(doc["providers"].as_array().unwrap().len(), 1);
        assert_eq!(doc["counts"]["serving_ai"], 1);
        // Only the stale seed is offline.
        assert_eq!(doc["counts"]["offline"], 1);
        assert_eq!(doc["seeds"][1]["peer_id"], STALE);
        assert_eq!(doc["seeds"][1]["live"], false);
        assert_eq!(doc["stale"], false);
    }

    #[test]
    fn stale_compiled_in_seed_is_not_counted_as_live() {
        let live = Ok(vec![PEER_B.into()]);
        let doc = build("t", None, live, &[], &seeds_typical());
        let stale_row = doc["seeds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["peer_id"] == STALE)
            .expect("stale seed must still be listed");
        assert_eq!(stale_row["live"], false);
        assert_eq!(doc["counts"]["peers_live"], 1);
        assert_eq!(doc["counts"]["offline"], 2); // SELF + STALE are both offline
    }

    #[test]
    fn rpc_failure_marks_the_document_stale_instead_of_guessing() {
        let doc = build(
            "t",
            Some(SELF),
            Err("connection refused".into()),
            &[provider(SELF, true, "http://127.0.0.1:8093")],
            &seeds_typical(),
        );
        assert_eq!(doc["stale"], true);
        assert_eq!(doc["peers"], 0);
        assert_eq!(doc["peer_ids"].as_array().unwrap().len(), 0);
        assert_eq!(doc["counts"]["peers_live"], 0);
        assert!(doc["error"]
            .as_str()
            .unwrap()
            .contains("connection refused"));
        // Known entries are still listed, all offline.
        for s in doc["seeds"].as_array().unwrap() {
            assert_eq!(s["live"], false);
        }
    }

    #[test]
    fn undialable_provider_multiaddrs_are_not_published() {
        let mut p = provider(SELF, true, "http://203.0.113.7:8093");
        p.multiaddrs = vec!["/ip4/203.0.113.7/tcp/30333".into(), "junk".into()];
        let doc = build("t", None, Ok(vec![]), &[p], &[]);
        assert_eq!(
            doc["providers"][0]["multiaddrs"].as_array().unwrap().len(),
            0
        );
    }
}
