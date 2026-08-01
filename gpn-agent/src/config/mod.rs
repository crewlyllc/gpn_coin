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
    pub mode: String,  // "auto", "mine", "compute", "stake_only"

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
            staked_amount: 10_000.0,
            min_attestation_stake: 1_000_000_000_000_000_000_000, // 1,000 GPN
            max_concurrent_jobs: 2,
            docker_endpoint: "unix:///var/run/docker.sock".to_string(),
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

impl AgentConfig {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let path = Path::new(path);
        if !path.exists() {
            log::warn!("Config file not found at {}, using defaults", path.display());
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

        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}
