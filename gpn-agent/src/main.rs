//! # GPU Node Agent
//!
//! The gpu-node-agent runs alongside each GPU provider's node.
//! It monitors GPU hardware, manages mining/compute/staking modes,
//! generates attestation proofs, and implements auto-switching logic.
//!
//! ## Modes
//! - **Mining**: Runs Ethash PoW miner on GPU
//! - **Compute**: Serves AI inference/training jobs via Docker
//! - **Stake Only**: GPU idle, only staking rewards
//! - **Auto**: Dynamically switches based on profitability
//!
//! ## Architecture
//! ```text
//! gpu-node-agent
//! ├── GPU Monitor (nvidia-smi / rocm-smi)
//! ├── Mining Engine (Ethash ethminer process)
//! ├── Compute Executor (Docker / Ray.io)
//! ├── Attestation Generator (challenge-response)
//! └── Profitability Calculator (auto-switch decision)
//! ```

use gpn_agent::{
    chain, compute::ComputeConfig, config, gpu, mining::MiningConfig, modes, profitability, AgentState, Mode,
};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Main agent struct
pub struct GPUMiningNodeAgent {
    config: config::AgentConfig,
    state: Arc<RwLock<AgentState>>,
    chain_client: chain::ChainClient,
    gpu_monitor: gpu::GPUMonitor,
    mode_manager: modes::ModeManager,
    profitability_calc: profitability::ProfitabilityCalculator,
}

impl GPUMiningNodeAgent {
    /// Create a new agent with the given configuration
    pub async fn new(config_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let config = config::AgentConfig::from_file(config_path)?;

        let chain_client = chain::ChainClient::new(&config.chain_endpoint)
            .await?
            .with_signer(&config.signer_uri);
        let gpu_monitor = gpu::GPUMonitor::new()?;
        let account = chain_client.get_account_info(&config.account_address).await?;

        let state = Arc::new(RwLock::new(AgentState {
            current_mode: Mode::Mining,
            gpu_info: gpu_monitor.get_gpu_info()?,
            account,
            last_profitability: None,
            is_running: true,
        }));

        let mode_manager = modes::ModeManager::new(
            state.clone(),
            MiningConfig {
                rpc_url: config.mining_rpc_url.clone(),
                device_index: config.mining_device_index,
                batch: 1 << 20,
                poll_ms: 2000,
            },
            ComputeConfig {
                device_index: config.compute_device_index,
                matrix_size: config.compute_matrix_size,
                batch: config.compute_batch,
            },
        )?;

        let profitability_calc = profitability::ProfitabilityCalculator::new(
            config.clone(),
            chain_client.clone(),
            gpu_monitor.clone(),
        );

        Ok(Self {
            config,
            state,
            chain_client,
            gpu_monitor,
            mode_manager,
            profitability_calc,
        })
    }

    /// Main agent loop
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("GPU Node Agent starting in {:?} mode", self.config.mode);

        // Enter the configured mode on boot so mining actually launches.
        let boot_mode = match self.config.mode.as_str() {
            "compute" => Mode::Compute,
            "stake_only" => Mode::StakeOnly,
            "mine" => Mode::Mining,
            // "auto" and anything else start mining, then let the
            // profitability oracle switch as needed.
            _ => Mode::Mining,
        };
        self.mode_manager.boot(boot_mode).await?;
        {
            let mut state = self.state.write().await;
            state.current_mode = boot_mode;
        }
        log::info!("Booted into {:?} mode (mining={}, computing={})", boot_mode, self.mode_manager.is_mining(), self.mode_manager.is_computing());

        // Periodic operations
        let mut attestation_interval = tokio::time::interval(
            std::time::Duration::from_secs(self.config.attestation_interval_secs)
        );
        let mut profitability_interval = tokio::time::interval(
            std::time::Duration::from_secs(self.config.profitability_check_interval_secs)
        );
        let mut health_check_interval = tokio::time::interval(
            std::time::Duration::from_secs(60) // Every minute
        );

        loop {
            tokio::select! {
                _ = attestation_interval.tick() => {
                    if let Err(e) = self.perform_attestation().await {
                        log::error!("Attestation failed: {}", e);
                    }
                }
                _ = profitability_interval.tick() => {
                    if self.config.mode == "auto" {
                        if let Err(e) = self.check_profitability().await {
                            log::error!("Profitability check failed: {}", e);
                        }
                    }
                }
                _ = health_check_interval.tick() => {
                    self.perform_health_check().await;
                }
            }

            // Check if agent should stop
            if !self.state.read().await.is_running {
                break;
            }
        }

        Ok(())
    }

    /// Perform GPU attestation challenge-response
    async fn perform_attestation(&self) -> Result<(), Box<dyn std::error::Error>> {
        log::debug!("Fetching attestation challenge...");

        // Get current challenge from chain
        let challenge = self.chain_client.get_attestation_challenge().await?;

        // Run challenge on GPU
        log::info!("Computing attestation proof on GPU...");
        let result = self.gpu_monitor.compute_challenge(&challenge).await?;

        // Build GPU info hash
        let gpu_info = self.state.read().await.gpu_info.clone();
        let gpu_info_hash = gpu::hash_gpu_info(&gpu_info);
        let gpu_count = 1u8; // TODO: read actual GPU count from telemetry

        // Measure real compute throughput (GFLOPS) on the GPU — this is the
        // on-chain attestable metric proving real AI-compute capability.
        let gflops = gpu::measure_gflops(self.config.mining_device_index).unwrap_or(0);
        log::info!("Measured GPU compute: {} GFLOPS (FP32)", gflops);

        // Submit attestation to chain
        self.chain_client
            .submit_attestation(result, gpu_info_hash, gpu_count, gflops)
            .await?;

        log::info!("GPU attestation submitted successfully");
        Ok(())
    }

    /// Check profitability and switch modes if needed (auto mode)
    async fn check_profitability(&self) -> Result<(), Box<dyn std::error::Error>> {
        let analysis = self.profitability_calc.analyze().await?;

        // Store analysis
        {
            let mut state = self.state.write().await;
            state.last_profitability = Some(analysis.clone());
        }

        log::info!(
            "Profitability: Mining=${:.4}/hr, Compute=${:.4}/hr, Staking=${:.4}/hr",
            analysis.mining_revenue_per_hour,
            analysis.compute_revenue_per_hour,
            analysis.staking_revenue_per_hour,
        );

        // Decide if switch is needed
        let current_mode = self.state.read().await.current_mode;
        let recommended = analysis.recommended_mode();

        if recommended != current_mode {
            log::info!(
                "🔄 Switching mode: {:?} → {:?}",
                current_mode,
                recommended
            );

            // Check hysteresis threshold
            if self.profitability_calc.should_switch(&analysis, current_mode) {
                self.mode_manager.switch_to(recommended).await?;

                let mut state = self.state.write().await;
                state.current_mode = recommended;
                log::info!("✅ Mode switch complete: now {:?}", recommended);
            }
        }

        Ok(())
    }

    /// Health check — report GPU status and mode
    async fn perform_health_check(&self) {
        let state = self.state.read().await;
        log::info!(
            "📊 [Health] Mode={:?} GPU={}°C VRAM={}MB/{:?} Hashes={:.1}MH/s Compute={:.1}GFLOPS",
            state.current_mode,
            state.gpu_info.temperature_celsius,
            state.gpu_info.vram_used_mb,
            state.gpu_info.vram_total_bytes / (1024 * 1024),
            state.gpu_info.current_hashrate_mhs,
            self.mode_manager.compute_gflops(),
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/default.toml".to_string());

    let agent = GPUMiningNodeAgent::new(&config_path).await?;
    agent.run().await?;

    Ok(())
}
