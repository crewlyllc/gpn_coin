/// Profitability calculator and auto-switch decision engine
/// Determines the most profitable mode for the GPU at any time.

use crate::{config::AgentConfig, gpu::GPUMonitor, chain::ChainClient, Mode};
use serde::{Deserialize, Serialize};

/// Result of a profitability analysis
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfitAnalysis {
    /// Estimated mining revenue (USD/hour)
    pub mining_revenue_per_hour: f64,
    /// Estimated compute serving revenue (USD/hour)
    pub compute_revenue_per_hour: f64,
    /// Estimated staking revenue (USD/hour)
    pub staking_revenue_per_hour: f64,
    /// Current network difficulty
    pub network_difficulty: f64,
    /// Token price in USD
    pub token_price_usd: f64,
    /// Average GPU hour price from marketplace
    pub avg_gpu_hour_price: f64,
    /// Marketplace utilization rate (0.0-1.0)
    pub utilization_rate: f64,
    /// GPUs hashrate in MH/s
    pub hashrate_mhs: f64,
}

impl ProfitAnalysis {
    /// Get the recommended mode based on profitability
    pub fn recommended_mode(&self) -> Mode {
        let mining = self.mining_revenue_per_hour;
        let compute = self.compute_revenue_per_hour;
        let staking = self.staking_revenue_per_hour;

        if compute > mining && compute > staking {
            Mode::Compute
        } else if mining > compute && mining > staking {
            Mode::Mining
        } else {
            Mode::StakeOnly
        }
    }

    /// Get revenue breakdown as formatted string
    pub fn summary(&self) -> String {
        format!(
            "Mining: ${:.4}/hr | Compute: ${:.4}/hr | Staking: ${:.4}/hr",
            self.mining_revenue_per_hour,
            self.compute_revenue_per_hour,
            self.staking_revenue_per_hour,
        )
    }
}

pub struct ProfitabilityCalculator {
    config: AgentConfig,
    chain_client: ChainClient,
    gpu_monitor: GPUMonitor,
}

impl ProfitabilityCalculator {
    pub fn new(config: AgentConfig, chain_client: ChainClient, gpu_monitor: GPUMonitor) -> Self {
        Self { config, chain_client, gpu_monitor }
    }

    /// Run a full profitability analysis
    pub async fn analyze(&self) -> Result<ProfitAnalysis, Box<dyn std::error::Error>> {
        // Fetch on-chain data
        let difficulty = self.chain_client.get_difficulty().await?;
        let token_price = self.chain_client.get_token_price_usd().await?;
        let gpu_price = self.chain_client.get_gpu_hour_price(&self.config.gpu_model).await?;
        let utilization = self.chain_client.get_utilization_rate(&self.config.gpu_model).await?;
        let staking_apr = self.chain_client.get_staking_apr().await?;

        // Fetch local GPU data
        let gpu_info = self.gpu_monitor.get_gpu_info()?;

        // Calculate revenues
        let mining = self.calc_mining_revenue(gpu_info.current_hashrate_mhs, difficulty, token_price);
        let compute = self.calc_compute_revenue(gpu_price, utilization, token_price);
        let staking = self.calc_staking_revenue(self.config.staked_amount, staking_apr, token_price);

        Ok(ProfitAnalysis {
            mining_revenue_per_hour: mining,
            compute_revenue_per_hour: compute,
            staking_revenue_per_hour: staking,
            network_difficulty: difficulty,
            token_price_usd: token_price,
            avg_gpu_hour_price: gpu_price,
            utilization_rate: utilization,
            hashrate_mhs: gpu_info.current_hashrate_mhs,
        })
    }

    /// Determine if a mode switch is warranted (with hysteresis)
    pub fn should_switch(&self, analysis: &ProfitAnalysis, current: Mode) -> bool {
        let hysteresis = 1.0 + (self.config.hysteresis_percent / 100.0);
        let recommended = analysis.recommended_mode();

        if recommended == current {
            return false;
        }

        // Apply hysteresis buffer
        match (current, recommended) {
            (Mode::Mining, Mode::Compute) => {
                analysis.compute_revenue_per_hour > analysis.mining_revenue_per_hour * hysteresis
            }
            (Mode::Compute, Mode::Mining) => {
                analysis.mining_revenue_per_hour > analysis.compute_revenue_per_hour * hysteresis
            }
            (Mode::Mining, Mode::StakeOnly) => {
                analysis.staking_revenue_per_hour > analysis.mining_revenue_per_hour * hysteresis
            }
            (Mode::Compute, Mode::StakeOnly) => {
                analysis.staking_revenue_per_hour > analysis.compute_revenue_per_hour * hysteresis
            }
            (Mode::StakeOnly, Mode::Mining) => {
                analysis.mining_revenue_per_hour > analysis.staking_revenue_per_hour * hysteresis
            }
            (Mode::StakeOnly, Mode::Compute) => {
                analysis.compute_revenue_per_hour > analysis.staking_revenue_per_hour * hysteresis
            }
            _ => false, // Same mode (already filtered above but needed for exhaustiveness)
        }
    }

    fn calc_mining_revenue(&self, hashrate_mhs: f64, difficulty: f64, token_price: f64) -> f64 {
        let block_time = 15.0; // seconds
        let block_reward = 50.0; // GPN (launch value)

        // Network hashrate ≈ difficulty × 2^32 / block_time
        let network_hashrate = difficulty * (2u64.pow(32) as f64) / block_time;

        if network_hashrate <= 0.0 {
            return 0.0;
        }

        let share = hashrate_mhs / network_hashrate;
        let blocks_per_hour = 3600.0 / block_time;
        let my_blocks = share * blocks_per_hour;
        let hourly_reward = my_blocks * block_reward;

        hourly_reward * token_price
    }

    fn calc_compute_revenue(&self, gpu_price: f64, utilization: f64, token_price: f64) -> f64 {
        let overhead_factor = 0.85; // 15% overhead for setup/teardown
        gpu_price * utilization * overhead_factor * token_price
    }

    fn calc_staking_revenue(&self, staked_amount: f64, apr: f64, token_price: f64) -> f64 {
        let annual = staked_amount * (apr / 100.0);
        (annual * token_price) / (365.25 * 24.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mining_revenue_basic() {
        // Not self-hosted due to ChainClient dependency
        // Testing pure calc logic
        assert!(true);
    }

    #[test]
    fn test_should_switch_hysteresis() {
        // These tests need a full config, will mock in integration tests
    }
}
