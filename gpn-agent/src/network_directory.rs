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
    /// #W78: how many peers this provider's own node reports (`/status` →
    /// `peers`). `None` = not reported (unknown); `Some(0)` = the box says it
    /// has no peers. The page must not infer "peered" from the AP's own peer
    /// list — a node peered with another member of the mesh is peered.
    pub peers: Option<u64>,
    /// The access point's **own** dial-back of this row's primary IPv4 socket
    /// (`dialcheck::verify`). `None` = never attempted (no IPv4 socket, or no
    /// verifier). This — never `reachability` — decides who may claim
    /// backbone: a self-reported tier is not evidence (2026-11-16).
    pub dialback: Option<Dialback>,
}

/// One dial-back verdict, as measured by the access point (not by the node).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialback {
    /// The socket that was dialled, exactly as it will be published.
    pub socket: String,
    /// `verified` | `wrong-peer` | `unreachable` | `skipped`.
    pub outcome: String,
    /// Who measured it (verifier peer id, or `ap`).
    pub verifier: String,
}

impl Dialback {
    pub fn verified(&self) -> bool {
        self.outcome == "verified"
    }
    pub fn wrong_peer(&self) -> bool {
        self.outcome == "wrong-peer"
    }
    /// `skipped:<reason>` / `unreachable` — everything that is not a pass.
    pub fn skipped(&self) -> bool {
        self.outcome.starts_with("skipped")
    }
}

/// What the directory decided to publish for one registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    /// `backbone` (publishes a dialable public address) or `leaf`.
    pub role: &'static str,
    /// The site's dial target for this public IP (one per IP at most).
    pub gateway: bool,
    /// Addresses to publish, IPv4 first. Empty for a leaf.
    pub multiaddrs: Vec<String>,
    /// The only address this box has is IPv6: a marker, not a dialability
    /// claim. Such a row never holds backbone.
    pub v6_only: bool,
    /// One line for the operator log.
    pub why: String,
}

/// `ip4` | `ip6` | `dns` plus the host, from a cleaned multiaddr.
fn family_of(ma: &str) -> Option<(&'static str, String)> {
    let parts: Vec<&str> = ma.split('/').filter(|p| !p.is_empty()).collect();
    let fam = match parts.first().copied()? {
        "ip4" => "ip4",
        "ip6" => "ip6",
        "dns" | "dns4" | "dns6" => "dns",
        _ => return None,
    };
    Some((fam, parts.get(1)?.to_string()))
}

/// The IPv4 (or `/dns`) multiaddr a row would publish first — the socket the
/// access point's dial-back verifies. `None` when the row has no IPv4 path.
pub fn primary_ipv4(mas: &[String]) -> Option<String> {
    dialable_list(mas).into_iter().find(|ma| {
        !matches!(family_of(ma).map(|(f, _)| f), None | Some("ip6"))
    })
}

/// The `/tcp/<port>` of a multiaddr, if it names one.
pub fn tcp_port_of(ma: &str) -> Option<u16> {
    let parts: Vec<&str> = ma.split('/').filter(|p| !p.is_empty()).collect();
    let i = parts.iter().position(|p| *p == "tcp")?;
    parts.get(i + 1)?.parse().ok()
}

/// Decide, for every deduped registration, what may be published and what tier
/// it holds. Pure: the dial-back verdicts are inputs.
///
/// The rules (2026-11-16, owner priority):
///
/// 1. **One backbone per public IP.** Exactly one node behind an egress may
///    claim backbone — the one a third party actually reaches. Ranked by the
///    access point's own dial-back, then by canonical dial port, then by peer
///    id (deterministic, so the answer does not flap between polls). A row
///    whose dial-back returned another peer id can never win.
/// 2. **IPv4 first.** A row that has an IPv4 (or `/dns`) address publishes it
///    as the primary address; IPv6 is an *additional* path. A row whose only
///    address is IPv6 is a leaf carrying `v6_only: true` — an explicit marker,
///    never an implied dialability claim.
/// 3. A leaf publishes **no** public address (SPEC 47 step 4).
/// 4. The gateway is the backbone whose socket is the canonical dial port.
pub fn plan_publication(rows: &[&DirProvider], canonical_port: u16) -> Vec<Publication> {
    struct Cand {
        ip: String,
        primary: String,
        extras: Vec<String>,
        rank: (u8, u8, u16, String),
        verifier: Option<String>,
        verdict: String,
    }
    let mut out: Vec<Publication> = vec![Publication::default(); rows.len()];
    let mut groups: std::collections::BTreeMap<String, Vec<(usize, Cand)>> =
        std::collections::BTreeMap::new();

    for (i, p) in rows.iter().enumerate() {
        let addrs = dialable_list(&p.multiaddrs);
        let mut v4: Vec<String> = Vec::new();
        let mut v6: Vec<String> = Vec::new();
        for ma in addrs {
            match family_of(&ma).map(|(f, _)| f) {
                Some("ip6") => v6.push(ma),
                Some(_) => v4.push(ma),
                None => {}
            }
        }
        if v4.is_empty() {
            // No IPv4 path at all: a leaf. If there is an IPv6 path we say so
            // instead of pretending the box is dialable by ordinary peers.
            out[i] = Publication {
                role: "leaf",
                gateway: false,
                multiaddrs: Vec::new(),
                v6_only: !v6.is_empty(),
                why: if v6.is_empty() {
                    "no public address published by the node".to_string()
                } else {
                    "IPv6-only: no IPv4 path was published or verified".to_string()
                },
            };
            continue;
        }
        let primary = v4.remove(0);
        let ip = family_of(&primary).map(|(_, h)| h).unwrap_or_default();
        let port = tcp_port_of(&primary).unwrap_or(0);
        // Rank: verified < unknown/skipped < unreachable. A wrong-peer verdict
        // is positive disproof and disqualifies the row entirely.
        let (tier, verifier, why) = match &p.dialback {
            Some(d) if d.verified() => (0u8, Some(d.verifier.clone()), "verified".to_string()),
            Some(d) if d.wrong_peer() => (9u8, Some(d.verifier.clone()), "wrong-peer".to_string()),
            // Any non-verdict that is not "I could not tell" is a failure:
            // an unreachable socket from a different egress is evidence that
            // the public address does not work, and a node may not hold a
            // tier on evidence that contradicts it.
            Some(d) if d.outcome == "unreachable" => (
                9u8,
                Some(d.verifier.clone()),
                "unreachable".to_string(),
            ),
            Some(d) => (
                2u8,
                Some(d.verifier.clone()),
                if d.skipped() {
                    d.outcome.clone()
                } else {
                    format!("skipped:{}", d.outcome)
                },
            ),
            None => (1u8, None, "skipped:no-verifier".to_string()),
        };
        let port_rank = if port == canonical_port { 0u8 } else { 1u8 };
        let peer = p.peer_id.clone().unwrap_or_default();
        groups.entry(ip.clone()).or_default().push((
            i,
            Cand {
                ip,
                primary,
                extras: v4.into_iter().chain(v6).collect(),
                rank: (tier, port_rank, port, peer),
                verifier,
                verdict: why.clone(),
            },
        ));
        out[i].why = why; // overwritten by the winner/loser decision below
    }

    for (_ip, mut cands) in groups {
        cands.sort_by(|a, b| a.1.rank.cmp(&b.1.rank));
        // The first row that is not positively disproved wins the IP. If every
        // claimant failed verification, nobody holds backbone: an egress with
        // no working forward must publish no backbone at all rather than
        // promote the least-bad claim.
        let winner = cands.iter().position(|(_, c)| c.rank.0 < 9);
        for (n, (i, c)) in cands.iter().enumerate() {
            if Some(n) == winner {
                let port = tcp_port_of(&c.primary).unwrap_or(0);
                let mut addrs = vec![c.primary.clone()];
                addrs.extend(c.extras.iter().cloned());
                let verified = c.rank.0 == 0;
                out[*i] = Publication {
                    role: "backbone",
                    gateway: port == canonical_port,
                    multiaddrs: addrs,
                    v6_only: false,
                    why: if verified {
                        format!("verified by {}", c.verifier.clone().unwrap_or_default())
                    } else if c.rank.0 == 2 {
                        format!(
                            "skipped:{} (elected by canonical port)",
                            c.verdict.trim_start_matches("skipped:")
                        )
                    } else {
                        "skipped:no-verifier (elected by canonical port)".to_string()
                    },
                };
            } else {
                out[*i] = Publication {
                    role: "leaf",
                    gateway: false,
                    multiaddrs: Vec::new(),
                    v6_only: false,
                    why: if c.rank.0 == 9 {
                        if c.verdict == "wrong-peer" {
                            format!("failed dial-back on {}: another peer answers", c.ip)
                        } else {
                            format!("failed dial-back on {}: {}", c.ip, c.verdict)
                        }
                    } else {
                        format!("another node owns the forward on {}", c.ip)
                    },
                };
            }
        }
    }
    out
}

impl Default for Publication {
    fn default() -> Self {
        Publication {
            role: "leaf",
            gateway: false,
            multiaddrs: Vec::new(),
            v6_only: false,
            why: String::new(),
        }
    }
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
    build_full(
        generated_at,
        self_peer_id,
        live_peers,
        providers,
        seeds,
        onchain,
        30333,
    )
}

/// The full document builder: every row's publication decision is made here,
/// from the dial-back verdicts the caller attached to each `DirProvider`.
///
/// `canonical_port` is the port the network's own dial targets use (the
/// `network.json` bootnode port). It only breaks ties between several
/// unverifiable machines behind one egress, where the one listening on the
/// canonical port is the one the forward was made for.
pub fn build_full(
    generated_at: &str,
    self_peer_id: Option<&str>,
    live_peers: Result<Vec<String>, String>,
    providers: &[DirProvider],
    seeds: &[KnownSeed],
    onchain: Option<Result<Vec<String>, String>>,
    canonical_port: u16,
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

    // ---- dedupe FIRST, then decide ---------------------------------------
    //
    // The counters must be computed over the same list the page renders; until
    // 2026-11-16 they were computed over the pre-dedupe list, so `/network`
    // said "5 backbone / 0 leaf" while the page showed 4 rows (and every row
    // said backbone, including two machines on one egress).
    let mut chosen: Vec<&DirProvider> = Vec::new();
    let mut provider_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for p in &identified {
        let pid = p.peer_id.as_deref().and_then(clean_peer_id);
        let key = pid
            .clone()
            .unwrap_or_else(|| format!("url:{}", p.status_url.trim_end_matches('/')));
        match provider_index.get(&key) {
            // Same machine registered twice (e.g. two accounts, pull + push):
            // keep one row, preferring the healthy record.
            Some(&i) => {
                if p.healthy && !chosen[i].healthy {
                    chosen[i] = p;
                }
            }
            None => {
                provider_index.insert(key, chosen.len());
                chosen.push(p);
            }
        }
    }

    // What each row may publish and which tier it holds. Verification is the
    // caller's dial-back (never the node's own `reachability` claim).
    let decisions = plan_publication(&chosen, canonical_port);

    let mut provider_rows: Vec<Value> = Vec::new();
    for (p, d) in chosen.iter().zip(decisions.iter()) {
        let pid = p.peer_id.as_deref().and_then(clean_peer_id);
        // One INFO line per decision, naming the evidence. The access point
        // polls this every few seconds, so it is deduped on the exact line —
        // an unchanged decision is not news.
        log_decision(&format!(
            "reachability: publish {} ({}); role={}",
            if d.multiaddrs.is_empty() {
                "-".to_string()
            } else {
                d.multiaddrs.join(",")
            },
            d.why,
            d.role
        ), pid.as_deref());
        let row = json!({
            "address": p.address,
            "peer_id": pid,
            "healthy": p.healthy,
            "vram_mb": p.vram_mb,
            "models": p.models,
            // What the directory decided, not what the node claimed.
            "multiaddrs": d.multiaddrs,
            "last_seen_secs": p.last_seen_secs,
            "status_url": p.status_url,
            "source": p.source,
            "reachability": d.role,
            "gateway": d.gateway,
            // A node whose only address is IPv6: an explicit marker, so the
            // page never implies it is reachable by an IPv4-only peer.
            "v6_only": d.v6_only,
            "decision": d.why,
            "self_declared": p.reachability,
            // #W78: a fact this box reported about itself. `null` = it did not
            // say (old build / unreadable), which the page treats as unknown
            // and falls back to peer-of-this-node.
            "peers": p.peers,
            // W50: two facts, two fields. `seen_by_ap` is true by construction
            // (this row exists because the AP has a record), and
            // `chain_registered` is what the chain says — or null.
            "seen_by_ap": true,
            "chain_registered": onchain_set
                .as_ref()
                .map(|s| s.contains(&crate::abuse::canonical_account(&p.address))),
        });
        provider_rows.push(row);
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
                // Registered on chain but never seen by this AP: nothing has
                // measured its peering, so it makes no claim.
                "peers": null,
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
    //
    // Every counter below is computed over `chosen` / `decisions` — the exact
    // rows the document renders. Counting anything else is how "5 backbone"
    // was published for 4 rows (2026-11-16).
    let serving_ai = chosen.len();
    let healthy_now = chosen.iter().filter(|p| p.healthy).count();
    let backbone_now = decisions.iter().filter(|d| d.role == "backbone").count();
    let leaf_now = decisions.iter().filter(|d| d.role == "leaf").count();
    let gateways = decisions.iter().filter(|d| d.gateway).count();
    let v6_only_now = decisions.iter().filter(|d| d.v6_only).count();

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
            "v6_only_now": v6_only_now,
            "offline": known_offline.len(),
            // W50: the two facts the page used to merge into one word.
            "ap_seen_now": chosen.len(),
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

/// Emit one INFO line per publication decision, deduped on the exact text so
/// a 5-second poll does not turn the journal into noise. The line is the
/// operator's audit trail: which address was published, on what evidence, and
/// in which tier.
fn log_decision(line: &str, peer: Option<&str>) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static LAST: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    let key = peer.unwrap_or("-").to_string();
    let mut guard = match LAST.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let map = guard.get_or_insert_with(HashMap::new);
    if map.get(&key).map(|v| v == line).unwrap_or(false) {
        return;
    }
    map.insert(key, line.to_string());
    eprintln!("[network] {line}");
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

    /// The websocket form GPN nodes actually listen on.
    fn maws(host: &str, port: u16, pid: &str) -> String {
        format!("/ip4/{host}/tcp/{port}/ws/p2p/{pid}")
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
            peers: None,
            dialback: None,
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
        // Two provider registrations, one machine → one row.
        assert_eq!(doc["providers"].as_array().unwrap().len(), 1);
        // 2026-11-16: every counter is computed over the *rendered* rows, so
        // one deduped row is one registration. This used to say 2 — twice the
        // rows the page showed, and the same class of bug that published
        // "5 backbone / 0 leaf" for 4 rows.
        assert_eq!(doc["counts"]["serving_ai"], 1);
        assert_eq!(doc["counts"]["ap_seen_now"], 1);
        assert_eq!(doc["counts"]["healthy_now"], 1);
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

    /// #W78: the row publishes the peer count the box reported about ITSELF, so
    /// the page can answer "is this node peered?" without asking whether it is
    /// peered with this access point. Unknown stays `null`.
    #[test]
    fn a_row_publishes_the_boxes_own_peer_count() {
        let mut peery = provider(PEER_B, true, "http://198.51.100.7:8093");
        peery.address = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".into();
        peery.peers = Some(2);
        // A chain-only account was never measured: no claim at all.
        let unseen = "5HgbpiTYE27CtBQF7gRWLz6Qt3HBxyi4tR2M75FC1oydH48z";
        let doc = build_with_chain(
            "t",
            None,
            // Peered with nobody this AP knows — the box still says it has 2.
            Ok(vec![]),
            &[peery],
            &[],
            Some(Ok(vec![
                "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".to_string(),
                unseen.to_string(),
            ])),
        );
        let rows = doc["providers"].as_array().unwrap();
        let seen_row = rows.iter().find(|r| r["source"] != "chain").unwrap();
        assert_eq!(seen_row["peers"], json!(2), "{seen_row}");
        let chain_row = rows.iter().find(|r| r["source"] == "chain").unwrap();
        assert_eq!(
            chain_row["peers"],
            Value::Null,
            "an unmeasured peering fact must be null, not 0: {chain_row}"
        );

        // A box that reports nothing (older build) is unknown, never "0 peers".
        let mut old = provider(PEER_C, true, "http://198.51.100.8:8093");
        old.address = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".into();
        old.peers = None;
        let doc = build_with_chain("t", None, Ok(vec![]), &[old], &[], Some(Ok(vec![])));
        assert_eq!(doc["providers"][0]["peers"], Value::Null);
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
        // A different egress, so it is a second site's backbone (two machines
        // on ONE public IP may not both be backbone — see the election tests).
        gateway.multiaddrs = vec![ma("23.241.109.255", 30333, BOX1)];
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
        // New split: two backbone rows (one per egress), one leaf. The
        // gateway is a *per-egress* dial target, so two sites with a backbone
        // on the canonical port are two gateways.
        assert_eq!(doc["counts"]["backbone_now"], 2);
        assert_eq!(doc["counts"]["leaf_now"], 1);
        assert_eq!(doc["counts"]["gateways"], 2);
        // And the tier is visible per row, so an operator can see who is who.
        let rows = doc["providers"].as_array().unwrap();
        let leaf_row = rows.iter().find(|r| r["peer_id"] == PEER_C).unwrap();
        assert_eq!(leaf_row["reachability"], "leaf");
        assert!(leaf_row["multiaddrs"].as_array().unwrap().is_empty());
        let gw_row = rows.iter().find(|r| r["peer_id"] == BOX1).unwrap();
        assert_eq!(gw_row["gateway"], true);
    }

    // ── 2026-11-16: the directory stops counting self-claims ──────────────

    fn dialback(socket: &str, outcome: &str) -> Dialback {
        Dialback {
            socket: socket.to_string(),
            outcome: outcome.to_string(),
            verifier: "12D3KooWVerifier".to_string(),
        }
    }

    /// A node with BOTH a working IPv4 forward and an IPv6 address publishes
    /// the IPv4 form as its primary address and keeps IPv6 as an extra path.
    /// Before this, a dual-stack box published whichever address the node's
    /// listen list happened to name first — on the 2026-11-16 fleet that was
    /// IPv6-only for both machines behind 172.117.107.91, so no peer without
    /// IPv6 could reach that LAN at all.
    #[test]
    fn ipv4_forward_is_published_first_and_ipv6_only_is_marked() {
        let mut dual = provider(PEER_B, true, "pull://dual");
        dual.address = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".into();
        // Deliberately list IPv6 first, as the boxes did.
        dual.multiaddrs = vec![
            "/ip6/2603:8002:e400:204f:c9b5:3fe4:ddb7:504/tcp/30333/ws/p2p/12D3KooWRfKQbKnynFsKVyi76FjFDWLQqQPTKZFrMFF6Zt7zoZ2o".into(),
            maws("172.117.107.91", 30333, PEER_B),
        ];
        dual.dialback = Some(dialback("/ip4/172.117.107.91/tcp/30333", "verified"));
        let v6only = DirProvider {
            address: "5HgbpiTYE27CtBQF7gRWLz6Qt3HBxyi4tR2M75FC1oydH48z".into(),
            peer_id: Some(PEER_C.into()),
            healthy: true,
            vram_mb: 8188,
            models: vec!["llama3.2:1b".into()],
            multiaddrs: vec![format!(
                "/ip6/2603:8002:e400:204f:6c31:c9a2:4af2:2085/tcp/30333/ws/p2p/{PEER_C}"
            )],
            last_seen_secs: Some(1),
            status_url: "pull://v6".into(),
            source: "push".into(),
            reachability: "backbone".into(),
            gateway: false,
            peers: Some(2),
            dialback: None,
        };
        let doc = build_full(
            "t",
            None,
            Ok(vec![]),
            &[dual, v6only],
            &[],
            None,
            30333,
        );
        let rows = doc["providers"].as_array().unwrap();
        let d = rows.iter().find(|r| r["peer_id"] == PEER_B).unwrap();
        assert_eq!(d["reachability"], "backbone", "{d}");
        let addrs = d["multiaddrs"].as_array().unwrap();
        assert_eq!(addrs.len(), 2, "IPv6 stays as an additional path: {d}");
        assert!(
            addrs[0].as_str().unwrap().starts_with("/ip4/172.117.107.91/"),
            "the IPv4 forward must be the primary address: {d}"
        );
        assert!(addrs[1].as_str().unwrap().starts_with("/ip6/"), "{d}");
        assert_eq!(d["v6_only"], false);

        // The IPv6-only box is explicitly marked, never implied dialable.
        let v = rows.iter().find(|r| r["peer_id"] == PEER_C).unwrap();
        assert_eq!(v["v6_only"], true, "{v}");
        assert_eq!(v["reachability"], "leaf", "{v}");
        assert!(v["multiaddrs"].as_array().unwrap().is_empty(), "{v}");
        assert_eq!(doc["counts"]["v6_only_now"], 1);
    }

    /// Two machines behind one public IP: exactly one is backbone, the other
    /// is a leaf whose row carries no public address. The forward holder is
    /// decided by dial-back, never by the node's own `reachability` string.
    #[test]
    fn one_backbone_per_public_ip_and_the_loser_publishes_nothing() {
        let mut owner = provider(PEER_B, true, "pull://owner");
        owner.address = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".into();
        owner.multiaddrs = vec![maws("172.117.107.91", 30333, PEER_B)];
        owner.dialback = Some(dialback("/ip4/172.117.107.91/tcp/30333", "verified"));
        // The other machine self-declares backbone and even advertises the same
        // public socket (the 2026-11-12 duplicate-bootnode bug).
        let mut loser = provider(PEER_C, true, "pull://loser");
        loser.address = "5HgbpiTYE27CtBQF7gRWLz6Qt3HBxyi4tR2M75FC1oydH48z".into();
        loser.multiaddrs = vec![maws("172.117.107.91", 30333, PEER_C)];
        loser.reachability = "backbone".into();
        loser.dialback = Some(dialback("/ip4/172.117.107.91/tcp/30333", "wrong-peer"));

        let doc = build_full("t", None, Ok(vec![]), &[loser, owner], &[], None, 30333);
        let rows = doc["providers"].as_array().unwrap();
        let o = rows.iter().find(|r| r["peer_id"] == PEER_B).unwrap();
        let l = rows.iter().find(|r| r["peer_id"] == PEER_C).unwrap();
        assert_eq!(o["reachability"], "backbone", "{o}");
        assert_eq!(o["gateway"], true, "the canonical-port backbone is the dial target: {o}");
        assert_eq!(l["reachability"], "leaf", "{l}");
        assert!(
            l["multiaddrs"].as_array().unwrap().is_empty(),
            "a leaf publishes no public address: {l}"
        );
        assert_eq!(l["self_declared"], "backbone", "the claim is kept for audit");
        assert_eq!(doc["counts"]["backbone_now"], 1);
        assert_eq!(doc["counts"]["leaf_now"], 1);
        assert_eq!(
            doc["counts"]["backbone_now"].as_u64().unwrap(),
            rows.iter()
                .filter(|r| r["reachability"] == "backbone")
                .count() as u64,
            "the counter must equal the number of backbone rows"
        );
    }

    /// A node that names itself backbone but fails the dial-back is demoted —
    /// including when the verdict is merely "unreachable" and another box on
    /// the same egress was verified.
    #[test]
    fn a_self_declared_backbone_that_fails_dialback_is_demoted() {
        let mut bragger = provider(PEER_B, true, "pull://bragger");
        bragger.multiaddrs = vec![maws("203.0.113.9", 30333, PEER_B)];
        bragger.reachability = "backbone".into();
        bragger.dialback = Some(dialback("/ip4/203.0.113.9/tcp/30333", "unreachable"));
        let doc = build_full("t", None, Ok(vec![]), &[bragger], &[], None, 30333);
        let r = &doc["providers"][0];
        assert_eq!(r["reachability"], "leaf", "{r}");
        assert!(r["multiaddrs"].as_array().unwrap().is_empty(), "{r}");
        assert_eq!(doc["counts"]["backbone_now"], 0);
    }

    /// Regression: the counters used to be computed from the pre-dedupe list
    /// (`identified`) while the page rendered the deduped rows — 5 backbone
    /// for 4 rows. Every counter must equal the number of matching rows.
    #[test]
    fn counters_equal_matching_deduped_rows() {
        // Two accounts on one machine (same peer id, both push), plus one
        // chain-only placeholder.
        let a = provider(PEER_B, true, "pull://a");
        let mut b = provider(PEER_B, true, "pull://a");
        b.address = "5HgbpiTYE27CtBQF7gRWLz6Qt3HBxyi4tR2M75FC1oydH48z".into();
        let mut a = a;
        a.address = "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".into();
        let doc = build_with_chain(
            "t",
            None,
            Ok(vec![PEER_B.into()]),
            &[a, b],
            &[],
            Some(Ok(vec![
                "5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj".to_string(),
                "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".to_string(),
            ])),
        );
        let rows = doc["providers"].as_array().unwrap();
        let seen: Vec<&Value> = rows.iter().filter(|r| r["source"] != "chain").collect();
        assert_eq!(seen.len(), 1, "one machine, one row: {rows:?}");
        for key in ["serving_ai", "ap_seen_now", "healthy_now", "backbone_now"] {
            assert_eq!(
                doc["counts"][key].as_u64().unwrap(),
                1,
                "{key} must count rows, not pre-dedupe entries: {rows:?}"
            );
        }
        assert_eq!(
            doc["counts"]["leaf_now"].as_u64().unwrap(),
            seen.iter().filter(|r| r["reachability"] == "leaf").count() as u64
        );
        // The chain placeholders are listed but never counted as providers.
        assert_eq!(doc["counts"]["chain_only_now"], 1);
    }
}
