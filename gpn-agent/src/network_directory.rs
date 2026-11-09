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
    /// Why the row is healthy: `"push"` (signed heartbeat, works behind NAT)
    /// or `"poll"` (the AP reached `status_url`). Empty when unhealthy.
    pub source: String,
    /// SPEC 47 tier: `"backbone"` | `"leaf"`.
    pub reachability: String,
    /// The one dialable machine per LAN/egress.
    pub gateway: bool,
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
    // Without a chain read the registration fact is unknown, not false.
    build_with_chain(
        generated_at,
        self_peer_id,
        live_peers,
        providers,
        seeds,
        None,
    )
}

/// Same as [`build`], but also publishes the **chain** registration fact.
///
/// The directory used to have one word for two independent questions:
/// "registered" meant "the access point has this provider in its registry",
/// which is *not* the same as "this account is in
/// `ComputeMarketplace::RegisteredProviders`". A node that registered on chain
/// and then never reached this access point was invisible, and a node the AP
/// had seen was published as "registered" whether or not the chain agreed
/// (2026-11-08, W50). Now every row carries both facts:
///
///   * `chain_registered: true|false|null` — chain registry, `null` when the
///     read failed (never a negative answer out of a dead socket, #141/#144);
///   * `seen_by_ap: true|false` — this access point holds a heartbeat record.
///
/// `onchain` is `None` when no chain read was attempted/allowed, so an older
/// caller keeps exactly the old document plus `null`s.
pub fn build_with_chain(
    generated_at: &str,
    self_peer_id: Option<&str>,
    live_peers: Result<Vec<String>, String>,
    providers: &[DirProvider],
    seeds: &[KnownSeed],
    onchain: Option<Result<Vec<String>, String>>,
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
    //
    // A row with **no address** is not a provider registration: it is the
    // placeholder for a configured endpoint that never answered (see
    // `unreachable_placeholder` in the access point). Counting it made `/nodes`
    // report 5 providers where 3 exist (2026-11-07, #147), so placeholders are
    // left out of the directory entirely — the AP's own log and `/providers`
    // are where "this endpoint is down" belongs.
    let identified: Vec<&DirProvider> = providers
        .iter()
        .filter(|p| !p.address.trim().is_empty())
        .collect();
    // ---- chain registry: an independent fact, never assumed ---------------
    let mut onchain_set: Option<std::collections::HashSet<String>> = None;
    let mut onchain_error: Option<String> = None;
    match onchain {
        Some(Ok(addrs)) => {
            let set: std::collections::HashSet<String> = addrs
                .iter()
                .map(|a| crate::abuse::canonical_account(a))
                .filter(|a| !a.is_empty())
                .collect();
            onchain_set = Some(set);
        }
        Some(Err(e)) => onchain_error = Some(e),
        None => {}
    }

    let mut provider_rows: Vec<Value> = Vec::new();
    let mut provider_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for p in &identified {
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
            "source": p.source,
            "reachability": p.reachability,
            "gateway": p.gateway,
            // W50: two facts, two fields. `seen_by_ap` is true by construction
            // (this row exists because the AP has a record), and
            // `chain_registered` is what the chain says — or null.
            "seen_by_ap": true,
            "chain_registered": onchain_set
                .as_ref()
                .map(|s| s.contains(&crate::abuse::canonical_account(&p.address))),
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

    // ---- chain-only rows: registered on chain, never seen by this AP --------
    //
    // This is the row the old directory could not express at all. It carries no
    // identity claim beyond the account (no peer id, no models, no address it
    // was observed at), so it cannot inflate `serving_ai` — that counter keeps
    // meaning "leaseable registrations the AP knows".
    let mut chain_only: Vec<Value> = Vec::new();
    if let Some(set) = onchain_set.as_ref() {
        let seen: std::collections::HashSet<String> = identified
            .iter()
            .map(|p| crate::abuse::canonical_account(&p.address))
            .collect();
        let mut missing: Vec<&String> = set.iter().filter(|a| !seen.contains(*a)).collect();
        missing.sort();
        for addr in missing {
            chain_only.push(json!({
                "address": addr,
                "peer_id": null,
                "healthy": false,
                "vram_mb": null,
                "models": Vec::<String>::new(),
                "multiaddrs": Vec::<String>::new(),
                "last_seen_secs": null,
                "status_url": "",
                "source": "chain",
                "reachability": "",
                "gateway": false,
                "seen_by_ap": false,
                "chain_registered": true,
            }));
        }
    }
    let chain_only_count = chain_only.len();
    provider_rows.extend(chain_only);

    // ---- classify ----------------------------------------------------------
    // `serving_ai` answers the registry question the page actually asks: how
    // many providers can the access point lease? It is NOT a heartbeat count.
    // Deriving it from `healthy` (a status-URL reachability fact) undercounted
    // the fleet and made a NAT'd provider look like a non-provider — audit
    // 2026-11-02 #3: `/nodes` said "1 serving AI" while the AP registry held 4.
    // Reachability is published separately as `healthy_now`.
    let serving_ai = identified.len();
    let healthy_now = identified.iter().filter(|p| p.healthy).count();
    // SPEC 47: separate backbone from leaf. A leaf is a full participant, it is
    // just not dialable — conflating the two is how a fleet of leaves looked
    // like zero providers. `gateways` counts the named site dial targets.
    let backbone_now = identified
        .iter()
        .filter(|p| p.reachability == "backbone")
        .count();
    let leaf_now = identified
        .iter()
        .filter(|p| p.reachability != "backbone")
        .count();
    let gateways = identified.iter().filter(|p| p.gateway).count();

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
            "healthy_now": healthy_now,
            "backbone_now": backbone_now,
            "leaf_now": leaf_now,
            "gateways": gateways,
            "offline": known_offline.len(),
            // W50: the two facts the page used to merge into one word.
            "ap_seen_now": identified.len(),
            "chain_registered_now": onchain_set.as_ref().map(|s| s.len()),
            "chain_only_now": chain_only_count,
        },
        "stale": stale,
    });
    if let Some(e) = onchain_error {
        doc["chain_registered_error"] = json!(e);
    }
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
            source: "poll".into(),
            reachability: "backbone".into(),
            gateway: false,
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
        // `serving_ai` counts leaseable registrations (2 accounts on one
        // machine), NOT distinct machines and NOT heartbeat reachability.
        assert_eq!(doc["counts"]["serving_ai"], 2);
        assert_eq!(doc["counts"]["healthy_now"], 2);
        // Only the stale seed is offline.
        assert_eq!(doc["counts"]["offline"], 1);
        assert_eq!(doc["seeds"][1]["peer_id"], STALE);
        assert_eq!(doc["seeds"][1]["live"], false);
        assert_eq!(doc["stale"], false);
    }

    #[test]
    fn an_unidentified_endpoint_is_not_counted_as_a_provider() {
        // 2026-11-07 (#147): the AP's registry carried a placeholder for every
        // configured `/status` URL that did not answer — `address: ""`, five
        // capabilities, `healthy: false`. `/nodes` counted them, so it said 5
        // providers where 3 exist. A row with no identity is not a provider.
        let placeholder = DirProvider {
            address: String::new(),
            peer_id: None,
            healthy: false,
            models: vec![],
            last_seen_secs: None,
            status_url: "http://gpn-testnet.duckdns.org:8093/status".into(),
            ..Default::default()
        };
        let live = Ok(vec![PEER_B.into()]);
        let doc = build(
            "t",
            None,
            live,
            &[
                provider(PEER_B, true, "http://198.51.100.7:8093"),
                placeholder,
            ],
            &[],
        );
        assert_eq!(doc["providers"].as_array().unwrap().len(), 1);
        assert_eq!(doc["counts"]["serving_ai"], 1);
        assert_eq!(doc["counts"]["healthy_now"], 1);
        assert_eq!(doc["counts"]["leaf_now"], 0, "a placeholder is not a leaf");
    }

    #[test]
    fn a_registered_provider_whose_status_url_is_unreachable_is_still_serving() {
        // The exact audit finding: one healthy heartbeat, one NAT'd/unreachable
        // one. Both are leaseable, so the headline must be 2 with healthy_now 1.
        let live = Ok(vec![BOX1.into(), PEER_B.into()]);
        let doc = build(
            "t",
            None,
            live,
            &[
                provider(PEER_B, true, "http://198.51.100.7:8093"),
                provider(BOX1, false, "http://23.241.109.255:8093"),
            ],
            &[],
        );
        assert_eq!(doc["counts"]["serving_ai"], 2);
        assert_eq!(doc["counts"]["healthy_now"], 1);
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
    fn chain_registration_and_ap_visibility_are_two_facts() {
        // W50: the page said "registered" for both. One provider is both
        // chain-registered and AP-seen; a second account is registered on chain
        // and has never reached this access point, so it must still be listed —
        // with `seen_by_ap: false` — instead of vanishing.
        let ap_addr = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj";
        let unseen = "5HgbpiTYE27CtBQF7gRWLz6Qt3HBxyi4tR2M75FC1oydH48z";
        let mut ap = provider(PEER_B, true, "http://198.51.100.7:8093");
        ap.address = ap_addr.into();
        let doc = build_with_chain(
            "t",
            None,
            Ok(vec![PEER_B.into()]),
            &[ap],
            &[],
            Some(Ok(vec![ap_addr.to_string(), unseen.to_string()])),
        );
        let rows = doc["providers"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "the chain-only account must be published");
        let seen_row = rows.iter().find(|r| r["address"] == ap_addr).unwrap();
        assert_eq!(seen_row["chain_registered"], true);
        assert_eq!(seen_row["seen_by_ap"], true);
        let unseen_row = rows.iter().find(|r| r["address"] == unseen).unwrap();
        assert_eq!(unseen_row["chain_registered"], true);
        assert_eq!(unseen_row["seen_by_ap"], false);
        assert_eq!(unseen_row["peer_id"], Value::Null);
        // Counters stay separate and `serving_ai` keeps its old meaning.
        assert_eq!(doc["counts"]["serving_ai"], 1);
        assert_eq!(doc["counts"]["ap_seen_now"], 1);
        assert_eq!(doc["counts"]["chain_registered_now"], 2);
        assert_eq!(doc["counts"]["chain_only_now"], 1);
    }

    #[test]
    fn unreadable_chain_registry_is_null_never_false() {
        // #141/#144: a failed read is "unknown", not "unregistered".
        let mut ap = provider(PEER_B, true, "http://198.51.100.7:8093");
        ap.address = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".into();
        let doc = build_with_chain(
            "t",
            None,
            Ok(vec![PEER_B.into()]),
            &[ap],
            &[],
            Some(Err("rpc timeout".into())),
        );
        assert_eq!(doc["providers"][0]["chain_registered"], Value::Null);
        assert_eq!(doc["providers"][0]["seen_by_ap"], true);
        assert_eq!(doc["counts"]["chain_registered_now"], Value::Null);
        assert_eq!(doc["counts"]["chain_only_now"], 0);
        assert!(doc["chain_registered_error"]
            .as_str()
            .unwrap()
            .contains("rpc timeout"));
    }

    #[test]
    fn build_without_a_chain_read_publishes_unknown_not_a_verdict() {
        // `build` (the old entry point) must not invent a registration fact.
        let doc = build(
            "t",
            None,
            Ok(vec![]),
            &[provider(PEER_B, true, "pull://x")],
            &[],
        );
        assert_eq!(doc["providers"][0]["chain_registered"], Value::Null);
        assert_eq!(doc["providers"][0]["seen_by_ap"], true);
        assert_eq!(doc["counts"]["chain_registered_now"], Value::Null);
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

    #[test]
    fn counts_split_backbone_from_leaf_without_breaking_the_old_fields() {
        let mut leaf = provider(PEER_C, true, "pull://leaf");
        leaf.reachability = "leaf".into();
        leaf.multiaddrs.clear();
        let mut gateway = provider(BOX1, true, "http://23.241.109.255:8093");
        gateway.gateway = true;
        let doc = build(
            "t",
            None,
            Ok(vec![PEER_B.into(), BOX1.into()]),
            &[provider(PEER_B, true, "pull://backbone"), leaf, gateway],
            &[],
        );
        // Old fields keep their meaning: all three are registered and healthy.
        assert_eq!(doc["counts"]["serving_ai"], 3);
        assert_eq!(doc["counts"]["healthy_now"], 3);
        // New split: two backbone rows, one leaf, one named gateway.
        assert_eq!(doc["counts"]["backbone_now"], 2);
        assert_eq!(doc["counts"]["leaf_now"], 1);
        assert_eq!(doc["counts"]["gateways"], 1);
        // And the tier is visible per row, so an operator can see who is who.
        let rows = doc["providers"].as_array().unwrap();
        let leaf_row = rows.iter().find(|r| r["peer_id"] == PEER_C).unwrap();
        assert_eq!(leaf_row["reachability"], "leaf");
        assert!(leaf_row["multiaddrs"].as_array().unwrap().is_empty());
        let gw_row = rows.iter().find(|r| r["peer_id"] == BOX1).unwrap();
        assert_eq!(gw_row["gateway"], true);
    }
}
