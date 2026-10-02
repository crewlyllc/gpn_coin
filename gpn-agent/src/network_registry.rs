//! Canonical network registry (shared with the node).
//!
//! See `shared_workspace/artifacts/project7/design-network-identity-fork-tolerance.md`.
//! `network.json` is the shipped reference for a GPN network:
//! `{ network, chain_id, genesis_hash, spec_version, bootnodes[], latest_version }`.
//! Embedded at build time; overridable at runtime via `GPN_NETWORK_REGISTRY`
//! or `~/.gpn/network.json`.

use serde::Deserialize;

/// The registry shipped inside the binary.
pub const EMBEDDED_REGISTRY: &str = include_str!("../../network.json");

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

impl Registry {
    /// True when `genesis_hash` is still the unset placeholder.
    pub fn genesis_is_placeholder(&self) -> bool {
        self.genesis_hash
            .trim_start_matches("0x")
            .chars()
            .all(|c| c == '0')
    }
}

/// Load the active registry: override file if configured/present, else embedded.
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
                Err(e) => eprintln!("network registry override '{}' invalid: {}", path, e),
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
    pub spec_ok: bool,
    pub summary: String,
}

/// Classify a running chain against the registry.
pub fn classify(genesis_hex: &str, spec_version: u32, running_version: &str) -> NetworkStatus {
    let r = load_registry();
    let placeholder = r.genesis_is_placeholder();
    let genesis_match = r.genesis_hash.eq_ignore_ascii_case(genesis_hex);
    let spec_ok = spec_version >= r.spec_version;
    let summary = if !placeholder && !genesis_match {
        format!(
            "⚠️ DIFFERENT NETWORK — genesis {} does not match canonical '{}' ({}).",
            genesis_hex, r.network, r.chain_id
        )
    } else if !spec_ok {
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
        spec_ok,
        summary,
    }
}
