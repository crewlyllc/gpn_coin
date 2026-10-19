//! Canonical network registry (see
//! `shared_workspace/artifacts/project7/design-network-identity-fork-tolerance.md`).
//!
//! `network.json` is the shipped, canonical reference for a GPN network:
//! `{ network, chain_id, genesis_hash, spec_version, bootnodes[], latest_version }`.
//! It is embedded at build time so a node always carries the identity of the
//! network it was built for. An operator can override it at runtime by pointing
//! `GPN_NETWORK_REGISTRY` at a file (or dropping `~/.gpn/network.json`) if the
//! canonical registry is updated without a rebuild.
//!
//! A network is *defined* by its genesis hash; different params ⇒ different
//! network even if named the same. We therefore classify the running chain by
//! comparing its genesis hash against the registry rather than trusting names.

use serde::Deserialize;

/// The registry shipped inside the binary.
pub const EMBEDDED_REGISTRY: &str = include_str!("../../../network.json");

/// One hardcoded bootstrap seed ("Source A" of the membership design).
///
/// `ip4` is authoritative (Jeremy's requirement: *IP addresses*); `dns4` is an
/// optional alias in the same entry so an IP change survives a DNS update
/// without a release. `peer_id` is mandatory — it is the real identity check
/// (a dial to a rogue host fails the libp2p handshake), so an entry without a
/// valid peer id is skipped at load time rather than dialed blindly.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BootstrapSeed {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub ip4: String,
    #[serde(default = "default_seed_port")]
    pub port: u16,
    #[serde(default)]
    pub peer_id: String,
    #[serde(default)]
    pub dns4: String,
    #[serde(default)]
    pub operator: String,
}

fn default_seed_port() -> u16 {
    30333
}

#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    pub network: String,
    pub chain_id: String,
    pub genesis_hash: String,
    pub spec_version: u32,
    #[serde(default)]
    pub bootnodes: Vec<String>,
    /// Source A: up to five hardcoded seeds, compiled into the binary.
    #[serde(default)]
    pub bootstrap_seeds: Vec<BootstrapSeed>,
    /// Source B: bootstrap-list URLs (independent publishers).
    #[serde(default)]
    pub bootstrap_urls: Vec<String>,
    pub latest_version: String,
}

/// Load the active registry: an override file if configured/present, else the
/// embedded copy. Never fails — a corrupt override falls back to embedded.
///
/// An override may replace network identity fields, but it may **not** blank
/// the compiled-in bootstrap floor: `bootstrap_seeds` / `bootstrap_urls` from
/// the embedded registry are used whenever the override does not provide its
/// own. Source A exists to bootstrap a node with no DNS, no HTTP and no chain;
/// an operator file silently removing it would defeat that (design §3.0).
pub fn load_registry() -> Registry {
    let embedded: Registry =
        serde_json::from_str(EMBEDDED_REGISTRY).expect("embedded network.json must parse");
    let candidate = std::env::var("GPN_NETWORK_REGISTRY").ok().or_else(|| {
        let home = std::env::var("HOME").ok()?;
        let p = format!("{home}/.gpn/network.json");
        if std::path::Path::new(&p).exists() {
            Some(p)
        } else {
            None
        }
    });
    if let Some(path) = candidate {
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<Registry>(&text) {
                Ok(r) => return apply_bootstrap_floor(r, &embedded),
                Err(e) => log::warn!("network registry override '{}' invalid: {}", path, e),
            }
        }
    }
    embedded
}

/// Fill an override's empty bootstrap lists from the compiled-in registry.
/// Pure, so the floor invariant is testable without touching process state.
pub fn apply_bootstrap_floor(mut registry: Registry, embedded: &Registry) -> Registry {
    if registry.bootstrap_seeds.is_empty() {
        registry.bootstrap_seeds = embedded.bootstrap_seeds.clone();
    }
    if registry.bootstrap_urls.is_empty() {
        registry.bootstrap_urls = embedded.bootstrap_urls.clone();
    }
    registry
}

/// How the running chain relates to the canonical registry.
#[derive(Debug, Clone)]
pub struct NetworkStatus {
    pub name: String,
    pub chain_id: String,
    pub genesis_match: bool,
    /// One-line human status: MATCH / DIFFERENT NETWORK / OUTDATED.
    pub summary: String,
}

/// Classify the running chain (`genesis_hex`, `spec_version`, `running_version`)
/// against the registry.
pub fn classify(genesis_hex: &str, spec_version: u32, running_version: &str) -> NetworkStatus {
    let r = load_registry();
    let placeholder = r
        .genesis_hash
        .trim_start_matches("0x")
        .chars()
        .all(|c| c == '0');
    let genesis_match = r.genesis_hash.eq_ignore_ascii_case(genesis_hex);
    let summary = if !placeholder && !genesis_match {
        format!(
            "⚠️ DIFFERENT NETWORK — genesis {} does not match canonical '{}' ({}). You are NOT on {}.",
            genesis_hex, r.network, r.chain_id, r.network
        )
    } else if spec_version < r.spec_version {
        format!(
            "⚠️ OUTDATED — running spec {} < canonical {} — update to stay on {}.",
            spec_version, r.spec_version, r.network
        )
    } else if r.latest_version != running_version {
        format!(
            "note: running {} (registry latest {}) on canonical {}",
            running_version, r.latest_version, r.network
        )
    } else {
        format!("✅ MATCH — canonical {} ({})", r.network, r.chain_id)
    };
    NetworkStatus {
        name: r.network,
        chain_id: r.chain_id,
        genesis_match,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> Registry {
        serde_json::from_str(EMBEDDED_REGISTRY).expect("embedded network.json must parse")
    }

    #[test]
    fn an_override_cannot_blank_the_compiled_bootstrap_floor() {
        // The live shape that motivated this: an operator drop-in that sets
        // identity fields and leaves `bootnodes`/bootstrap lists empty. It must
        // not be able to remove the no-DNS/no-HTTP floor.
        let override_json = r#"{
            "network": "GPN Public Testnet",
            "chain_id": "gpn_testnet",
            "genesis_hash": "0xb8e4636a65d0c812abb437c38dd7e937bd7c3355c54c54d63060f26a33519a5c",
            "spec_version": 6,
            "bootnodes": [],
            "latest_version": "0.1.0"
        }"#;
        let over: Registry = serde_json::from_str(override_json).unwrap();
        assert!(over.bootstrap_seeds.is_empty());
        let merged = apply_bootstrap_floor(over, &embedded());
        assert_eq!(merged.bootstrap_seeds.len(), embedded().bootstrap_seeds.len());
        assert!(!merged.bootstrap_urls.is_empty());
    }

    #[test]
    fn an_override_that_supplies_its_own_seeds_keeps_them() {
        let mut over = embedded();
        over.bootstrap_seeds = vec![BootstrapSeed {
            id: "mine".into(),
            ip4: "203.0.113.9".into(),
            port: 31333,
            peer_id: "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo".into(),
            dns4: String::new(),
            operator: "self".into(),
        }];
        over.bootstrap_urls = vec!["https://example.org/seeds.json".into()];
        let merged = apply_bootstrap_floor(over, &embedded());
        assert_eq!(merged.bootstrap_seeds.len(), 1);
        assert_eq!(merged.bootstrap_seeds[0].id, "mine");
        assert_eq!(merged.bootstrap_urls, vec!["https://example.org/seeds.json"]);
    }
}
