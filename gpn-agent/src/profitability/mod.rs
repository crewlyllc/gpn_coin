/// Profitability calculator and auto-switch decision engine
/// Determines the most profitable mode for the GPU at any time.

pub mod scripted;

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

/// Pure auto-switch decision: should we move from `current` to the
/// recommended mode, given a hysteresis buffer?
///
/// This is a **free function** (not a method) so it can be unit-tested
/// without a live `ChainClient` / GPU, and so `ModeManager` / the agent loop
/// can drive it directly. The hysteresis prevents flapping when revenues are
/// close.
///
/// Returns `Some(new_mode)` if a switch is warranted, else `None`.
pub fn decide_switch(
    analysis: &ProfitAnalysis,
    current: Mode,
    hysteresis_percent: f64,
) -> Option<Mode> {
    let recommended = analysis.recommended_mode();
    if recommended == current {
        return None;
    }
    let h = 1.0 + (hysteresis_percent / 100.0);
    let (cur_r, rec_r) = (
        analysis.revenue_for(current),
        analysis.revenue_for(recommended),
    );
    // Only switch if the new mode beats the current one by the hysteresis
    // buffer. This is the core "no flapping" rule.
    if rec_r > cur_r * h {
        Some(recommended)
    } else {
        None
    }
}

impl ProfitAnalysis {
    /// USD/hour revenue for a given mode (used by the decision logic).
    pub fn revenue_for(&self, mode: Mode) -> f64 {
        match mode {
            Mode::Mining => self.mining_revenue_per_hour,
            Mode::Compute => self.compute_revenue_per_hour,
            Mode::StakeOnly => self.staking_revenue_per_hour,
        }
    }

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
    /// Scripted feed (only used when `config.profitability_feed == "scripted"`).
    scripted: Option<scripted::ScriptedFeed>,
    /// Index into the scripted timeline, advanced on each `analyze()`.
    scripted_step: std::sync::atomic::AtomicUsize,
}

impl ProfitabilityCalculator {
    pub fn new(config: AgentConfig, chain_client: ChainClient, gpu_monitor: GPUMonitor) -> Self {
        let scripted = if config.profitability_feed == "scripted" {
            Some(scripted::ScriptedFeed::demand_spike_scenario())
        } else {
            None
        };
        Self {
            config,
            chain_client,
            gpu_monitor,
            scripted,
            scripted_step: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Run a full profitability analysis
    pub async fn analyze(&self) -> Result<ProfitAnalysis, Box<dyn std::error::Error>> {
        // Scripted feed: deterministic timeline, ignores live chain data.
        if let Some(feed) = &self.scripted {
            let step = self.scripted_step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let snap = &feed.steps[step.min(feed.steps.len() - 1)];
            // Use the local GPU hashrate for the diagnostic field only.
            let gpu_info = self.gpu_monitor.get_gpu_info()?;
            return Ok(snap.to_analysis(
                100_000_000.0,
                0.05,
                2.5,
                0.65,
                gpu_info.current_hashrate_mhs,
            ));
        }

        // Live chain feed (current default — placeholder RPC until subxt client lands).
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

    /// Determine if a mode switch is warranted (with hysteresis).
    /// Delegates to the pure [`decide_switch`] free function.
    pub fn should_switch(&self, analysis: &ProfitAnalysis, current: Mode) -> bool {
        decide_switch(analysis, current, self.config.hysteresis_percent).is_some()
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

    fn analysis(mining: f64, compute: f64, staking: f64) -> ProfitAnalysis {
        ProfitAnalysis {
            mining_revenue_per_hour: mining,
            compute_revenue_per_hour: compute,
            staking_revenue_per_hour: staking,
            network_difficulty: 100_000_000.0,
            token_price_usd: 0.05,
            avg_gpu_hour_price: 2.5,
            utilization_rate: 0.65,
            hashrate_mhs: 20.0,
        }
    }

    #[test]
    fn recommended_mode_prefers_highest_revenue() {
        assert_eq!(analysis(10.0, 5.0, 1.0).recommended_mode(), Mode::Mining);
        assert_eq!(analysis(5.0, 10.0, 1.0).recommended_mode(), Mode::Compute);
        assert_eq!(analysis(1.0, 5.0, 10.0).recommended_mode(), Mode::StakeOnly);
    }

    #[test]
    fn decide_switch_no_op_when_same_or_within_hysteresis() {
        // Mining currently dominant; compute close but under 5% buffer.
        let a = analysis(10.0, 10.3, 1.0); // compute 3% higher < 5% hysteresis
        assert_eq!(decide_switch(&a, Mode::Mining, 5.0), None);

        // Already in the recommended mode -> no switch.
        let a2 = analysis(10.0, 1.0, 1.0);
        assert_eq!(decide_switch(&a2, Mode::Mining, 5.0), None);
    }

    #[test]
    fn decide_switch_flips_when_new_mode_exceeds_hysteresis() {
        // Compute exceeds mining by >5% -> switch to Compute.
        let a = analysis(10.0, 11.0, 1.0); // +10% > 5% hysteresis
        assert_eq!(decide_switch(&a, Mode::Mining, 5.0), Some(Mode::Compute));

        // Mining rebounds above compute by >5% -> switch back.
        let a2 = analysis(12.0, 10.0, 1.0);
        assert_eq!(decide_switch(&a2, Mode::Compute, 5.0), Some(Mode::Mining));
    }

    #[test]
    fn decide_switch_hysteresis_prevents_flapping() {
        // Two consecutive analyses where revenue oscillates just below the
        // buffer must NOT produce a switch both times.
        let a_hi = analysis(10.0, 10.4, 1.0);
        let a_lo = analysis(10.4, 10.0, 1.0);
        // Both are within 5% of each other -> no switch either direction.
        assert_eq!(decide_switch(&a_hi, Mode::Mining, 5.0), None);
        assert_eq!(decide_switch(&a_lo, Mode::Compute, 5.0), None);
    }

    #[test]
    fn decide_switch_handles_all_mode_pairs() {
        // Mining -> StakeOnly
        assert_eq!(
            decide_switch(&analysis(1.0, 0.5, 10.0), Mode::Mining, 5.0),
            Some(Mode::StakeOnly)
        );
        // Compute -> StakeOnly
        assert_eq!(
            decide_switch(&analysis(0.5, 1.0, 10.0), Mode::Compute, 5.0),
            Some(Mode::StakeOnly)
        );
        // StakeOnly -> Mining
        assert_eq!(
            decide_switch(&analysis(10.0, 1.0, 1.0), Mode::StakeOnly, 5.0),
            Some(Mode::Mining)
        );
        // StakeOnly -> Compute
        assert_eq!(
            decide_switch(&analysis(1.0, 10.0, 1.0), Mode::StakeOnly, 5.0),
            Some(Mode::Compute)
        );
    }
}
