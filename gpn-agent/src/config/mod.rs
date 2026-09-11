/// Agent configuration
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentConfig {
    // Node identity
    pub node_name: String,
    pub chain_endpoint: String,
    pub account_address: String,
    pub account_key_file: String,
    /// Substrate dev URI (e.g. `//Alice`) used to sign on-chain extrinsics
    /// (attestation, staking, compute-marketplace). Leave empty for read-only.
    pub signer_uri: String,
    pub mode: String, // "auto", "mine", "compute", "stake_only"

    // GPU settings
    pub gpu_model: String,
    pub min_vram_gb: u32,
    pub attestation_interval_secs: u64,

    // Profitability settings
    pub hysteresis_percent: f64,
    pub min_epoch_hours: f64,
    pub profitability_check_interval_secs: u64,
    /// Profitability feed source: "chain" (live RPC) or "scripted"
    /// (deterministic demand-spike timeline — used for demos/tests until the
    /// real chain feed is wired).
    pub profitability_feed: String,
    /// Max age (in blocks) of the on-chain `ProfitabilityOracle::CurrentFeed`
    /// before the agent treats it as **stale** and falls back to a conservative
    /// Mining default (compute + staking revenue forced to 0, so mining wins
    /// the auto-switch) instead of acting on outdated prices. None disables.
    /// Default 1440 blocks (~6h at 15s blocks), ~2.4x the pallet's
    /// `OracleUpdateInterval` (600 blocks), so a feed that stops being
    /// refreshed for a few hours no longer drives the network.
    #[serde(default = "default_feed_ttl")]
    pub feed_staleness_ttl_blocks: Option<u64>,
    /// Ceiling (USD/hour) on a plausible AI-compute revenue from the
    /// profitability feed. If `calc_compute_revenue(gpu_price, utilization)`
    /// exceeds this, the feed is treated as **implausible** (oracle-governance
    /// §6 gap #2: unit misconversion, garbage value, or a single bad/outlier
    /// operator) and the agent falls back to a conservative Mining default
    /// (compute revenue forced to 0, so mining wins the auto-switch) instead
    /// of letting one bad feed force a mass switch to Compute. Default 50
    /// USD/hr — very generous (an RTX 4060-class node realistically earns
    /// single-digit USD/hr at most), so it never blocks a legitimate switch.
    #[serde(default = "default_compute_revenue_max")]
    pub compute_revenue_max_usd_per_hour: f64,
    /// Feed aggregation for the auto-switch: `"latest"` (use `CurrentFeed`,
    /// the last operator to write — backward-compatible default),
    /// `"median"` (aggregate all operators' latest feeds via a per-field
    /// median, so one outlier operator can't move the effective feed;
    /// oracle-governance §6 gap #3), or `"bonded_median"` (aggregate bonded
    /// Phase-C operators' latest feeds via a **bond-weighted** median, so a
    /// large-bond operator dominates and a tiny-bond outlier can't move it;
    /// oracle-governance §6 gap #5 / spec §8 D3).
    #[serde(default = "default_feed_aggregation")]
    pub feed_aggregation: String,

    // Staking
    pub staked_amount: f64,
    /// Minimum GPN stake to deposit before attesting (pallet
    /// `MinAttestationStake` = 1,000 GPN). 0 disables the pre-attest deposit.
    pub min_attestation_stake: u128,

    // Compute
    pub max_concurrent_jobs: u32,
    pub docker_endpoint: String,
    /// OpenCL GPU device index to run AI compute jobs on
    pub compute_device_index: usize,
    /// GEMM size N for the AI-compute workload (NxNxN matmul)
    pub compute_matrix_size: usize,
    /// Matmuls per reporting batch for the compute engine
    pub compute_batch: usize,

    // Mining
    pub mining_algorithm: String,
    pub mining_pool_url: String,
    pub solo_mining: bool,
    /// HTTP JSON-RPC endpoint of the node for external mining (gpn_getTemplate/gpn_submitWork)
    pub mining_rpc_url: String,
    /// OpenCL GPU device index to mine on
    pub mining_device_index: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            node_name: "gpu-node-1".to_string(),
            chain_endpoint: "ws://localhost:9944".to_string(),
            account_address: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".to_string(),
            account_key_file: "account_key.json".to_string(),
            signer_uri: "//Alice".to_string(),
            mode: "auto".to_string(),
            gpu_model: "NVIDIA GeForce RTX 4090".to_string(),
            min_vram_gb: 8,
            attestation_interval_secs: 3600, // Every hour
            hysteresis_percent: 5.0,
            min_epoch_hours: 1.0,
            profitability_check_interval_secs: 3600, // Every hour
            profitability_feed: "chain".to_string(),
            feed_staleness_ttl_blocks: Some(1440),
            compute_revenue_max_usd_per_hour: default_compute_revenue_max(),
            feed_aggregation: default_feed_aggregation(),
            staked_amount: 10_000.0,
            min_attestation_stake: 1_000_000_000_000_000, // 1,000 GPN (12 decimals)
            max_concurrent_jobs: 2,
            docker_endpoint: default_docker_endpoint(),
            compute_device_index: 0,
            compute_matrix_size: 2048,
            compute_batch: 4,
            mining_algorithm: "ethash".to_string(),
            mining_pool_url: "".to_string(),
            solo_mining: true,
            mining_rpc_url: "http://127.0.0.1:9944".to_string(),
            mining_device_index: 0,
        }
    }
}

/// Default feed staleness TTL (1440 blocks ≈ 6h at 15s/block). Used by the
/// `#[serde(default = ...)]` so a config file that omits the field still gets
/// the safe default (Some(1440)), rather than serde's None.
fn default_feed_ttl() -> Option<u64> {
    Some(1440)
}

/// Platform-appropriate Docker endpoint.
///
/// Docker is optional for the prototype (see `compute/mod.rs`), but the default
/// is set correctly per-OS so a config that omits `docker_endpoint` still works
/// on Windows and Linux alike:
///   - Windows: `npipe:////./pipe/docker_engine` (named pipe)
///   - Linux/macOS: `unix:///var/run/docker.sock`
#[cfg(target_os = "windows")]
pub fn default_docker_endpoint() -> String {
    "npipe:////./pipe/docker_engine".to_string()
}

#[cfg(not(target_os = "windows"))]
pub fn default_docker_endpoint() -> String {
    "unix:///var/run/docker.sock".to_string()
}

/// Default compute-revenue reasonableness ceiling (50 USD/hr). See
/// [`AgentConfig::compute_revenue_max_usd_per_hour`].
fn default_compute_revenue_max() -> f64 {
    50.0
}

/// Default feed aggregation mode: `"latest"` (backward compatible).
fn default_feed_aggregation() -> String {
    "latest".to_string()
}

impl AgentConfig {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let path = Path::new(path);
        if !path.exists() {
            log::warn!(
                "Config file not found at {}, using defaults",
                path.display()
            );
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)?;
        let config: AgentConfig = toml::from_str(&contents)?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.node_name.is_empty() {
            errors.push("node_name is required".to_string());
        }
        if self.chain_endpoint.is_empty() {
            errors.push("chain_endpoint is required".to_string());
        }
        if self.hysteresis_percent < 0.0 || self.hysteresis_percent > 50.0 {
            errors.push("hysteresis_percent must be between 0 and 50".to_string());
        }
        if self.gpu_model.is_empty() {
            errors.push("gpu_model is required".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_feed_ttl_is_safe_1440() {
        assert_eq!(AgentConfig::default().feed_staleness_ttl_blocks, Some(1440));
    }

    #[test]
    fn default_toml_parses_with_staleness_ttl() {
        // The checked-in default.toml must deserialize with the new field and
        // carry the safe 1440-block default.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/config/default.toml");
        let cfg = AgentConfig::from_file(path).expect("default.toml must parse");
        assert_eq!(cfg.feed_staleness_ttl_blocks, Some(1440));
    }

    #[test]
    fn default_compute_revenue_max_is_safe_50() {
        assert_eq!(
            AgentConfig::default().compute_revenue_max_usd_per_hour,
            50.0
        );
    }

    #[test]
    fn omitted_compute_revenue_max_defaults_to_50() {
        // A config that predates the field (or omits it) must still get the
        // safe 50 USD/hr ceiling via #[serde(default = "default_compute_revenue_max")].
        let toml = r#"
            node_name = "n"
            chain_endpoint = "ws://x:9944"
            gpu_model = "RTX 4060"
        "#;
        let cfg: AgentConfig = toml::from_str(toml).expect("must parse");
        assert_eq!(cfg.compute_revenue_max_usd_per_hour, 50.0);
    }

    #[test]
    fn omitted_feed_aggregation_defaults_to_latest() {
        // A config that predates the field must default to "latest"
        // (backward compatible — no behavioural change for existing nodes).
        let toml = r#"
            node_name = "n"
            chain_endpoint = "ws://x:9944"
            gpu_model = "RTX 4060"
        "#;
        let cfg: AgentConfig = toml::from_str(toml).expect("must parse");
        assert_eq!(cfg.feed_aggregation, "latest");
        // Explicit "median" also parses.
        let toml2 = r#"
            node_name = "n"
            chain_endpoint = "ws://x:9944"
            gpu_model = "RTX 4060"
            feed_aggregation = "median"
        "#;
        let cfg2: AgentConfig = toml::from_str(toml2).expect("must parse");
        assert_eq!(cfg2.feed_aggregation, "median");
    }

    #[test]
    fn omitted_ttl_field_defaults_to_some_1440() {
        // A config that predates the field (or omits it) must still get the
        // safe default via #[serde(default = "default_feed_ttl")], NOT None.
        let toml = r#"
            node_name = "n"
            chain_endpoint = "ws://x:9944"
            gpu_model = "RTX 4060"
        "#;
        let cfg: AgentConfig = toml::from_str(toml).expect("must parse");
        assert_eq!(cfg.feed_staleness_ttl_blocks, Some(1440));
    }
}
