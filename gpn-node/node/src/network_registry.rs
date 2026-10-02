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

#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    pub network: String,
    pub chain_id: String,
    pub genesis_hash: String,
    pub spec_version: u32,
    #[serde(default)]
    pub bootnodes: Vec<String>,
    pub latest_version: String,
}

/// Load the active registry: an override file if configured/present, else the
/// embedded copy. Never fails — a corrupt override falls back to embedded.
pub fn load_registry() -> Registry {
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
                Ok(r) => return r,
                Err(e) => log::warn!("network registry override '{}' invalid: {}", path, e),
            }
        }
    }
    serde_json::from_str(EMBEDDED_REGISTRY).expect("embedded network.json must parse")
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
    let placeholder = r.genesis_hash.trim_start_matches("0x").chars().all(|c| c == '0');
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
