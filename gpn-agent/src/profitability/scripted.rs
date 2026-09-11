//! Scripted profitability feed — a deterministic time series of market
//! snapshots used to drive and demo the auto-switch decision logic **without**
//! a live chain / GPU.
//!
//! This is the "real/mock profitability feed" the HEARTBEAT calls for: instead
//! of hardcoded placeholders that always keep `auto` in Mining, a scripted
//! series lets compute prices rise and fall so the agent actually flips
//! Mining -> Compute -> Mining. The same `ProfitAnalysis` values it produces
//! are what the live `analyze()` would produce from real chain data, so the
//! decision code path is identical.

use crate::profitability::{decide_switch, ProfitAnalysis};
use crate::Mode;

/// A single point in the simulated market timeline.
#[derive(Clone, Debug)]
pub struct MarketSnapshot {
    /// Label for this step (e.g. "t+10min: compute demand spike").
    pub label: String,
    /// Mining revenue in USD/hr (driven by difficulty + token price).
    pub mining_usd_hr: f64,
    /// Compute revenue in USD/hr (driven by GPU-hour price + utilization).
    pub compute_usd_hr: f64,
    /// Staking revenue in USD/hr.
    pub staking_usd_hr: f64,
}

impl MarketSnapshot {
    pub fn new(label: &str, mining: f64, compute: f64, staking: f64) -> Self {
        Self {
            label: label.to_string(),
            mining_usd_hr: mining,
            compute_usd_hr: compute,
            staking_usd_hr: staking,
        }
    }

    /// Convert into a `ProfitAnalysis` (fills the diagnostic fields the agent
    /// would normally fetch from chain).
    pub fn to_analysis(
        &self,
        difficulty: f64,
        token_price: f64,
        gpu_hour_price: f64,
        util: f64,
        mhs: f64,
    ) -> ProfitAnalysis {
        ProfitAnalysis {
            mining_revenue_per_hour: self.mining_usd_hr,
            compute_revenue_per_hour: self.compute_usd_hr,
            staking_revenue_per_hour: self.staking_usd_hr,
            network_difficulty: difficulty,
            token_price_usd: token_price,
            avg_gpu_hour_price: gpu_hour_price,
            utilization_rate: util,
            hashrate_mhs: mhs,
        }
    }
}

/// A scripted market scenario: a list of snapshots over time.
#[derive(Clone, Debug)]
pub struct ScriptedFeed {
    pub steps: Vec<MarketSnapshot>,
}

impl ScriptedFeed {
    /// The canonical demo scenario: starts mining-dominant, then a compute
    /// demand spike flips the GPU to Compute, then demand fades and it flips
    /// back to Mining (with hysteresis preventing flapping at the edges).
    pub fn demand_spike_scenario() -> Self {
        Self {
            steps: vec![
                MarketSnapshot::new("baseline (mining dominant)", 1.00, 0.50, 0.20),
                MarketSnapshot::new("compute demand rising", 0.95, 0.80, 0.20),
                MarketSnapshot::new("compute demand spike (+60%)", 0.90, 1.60, 0.20),
                MarketSnapshot::new("compute hot & sustained", 0.88, 1.75, 0.20),
                MarketSnapshot::new("compute demand fading", 0.92, 1.10, 0.20),
                MarketSnapshot::new("compute back to baseline", 0.98, 0.55, 0.20),
                MarketSnapshot::new("mining rebound (compute dead)", 1.10, 0.40, 0.20),
            ],
        }
    }

    /// Run the scenario through the pure decision logic and return the list of
    /// `(mode, snapshot, decided_switch)` transitions. This is the headless
    /// proof that the auto-switch reacts to profitability.
    pub fn simulate(
        &self,
        start_mode: Mode,
        hysteresis_percent: f64,
    ) -> Vec<(Mode, &MarketSnapshot, Option<Mode>)> {
        let mut current = start_mode;
        let mut out = Vec::new();
        for step in &self.steps {
            let analysis = step.to_analysis(100_000_000.0, 0.05, 2.5, 0.65, 20.0);
            let decided = decide_switch(&analysis, current, hysteresis_percent);
            if let Some(next) = decided {
                current = next;
            }
            out.push((current, step, decided));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demand_spike_flips_to_compute_and_back() {
        let feed = ScriptedFeed::demand_spike_scenario();
        let trace = feed.simulate(Mode::Mining, 5.0);

        // Mode timeline must show: Mining ... Compute ... Mining
        let modes: Vec<Mode> = trace.iter().map(|(m, _, _)| *m).collect();
        assert_eq!(modes[0], Mode::Mining, "starts mining");

        // At some point we are in Compute (the spike flips us).
        assert!(
            modes.contains(&Mode::Compute),
            "compute spike must flip mode: {modes:?}"
        );

        // And we return to Mining by the end.
        assert_eq!(
            *modes.last().unwrap(),
            Mode::Mining,
            "must flip back to mining: {modes:?}"
        );

        // Verify the exact transitions recorded match expectation.
        let switches: Vec<Mode> = trace.iter().filter_map(|(_, _, d)| *d).collect();
        assert_eq!(
            switches,
            vec![Mode::Compute, Mode::Mining],
            "exactly two switches: to Compute then back to Mining"
        );
    }

    #[test]
    fn small_oscillation_within_hysteresis_never_flips() {
        // Compute and mining oscillate by ±5% around a mining-dominant base.
        // With 10% hysteresis neither direction ever clears the buffer, so the
        // GPU must stay in Mining the whole time (no flapping).
        let feed = ScriptedFeed {
            steps: vec![
                MarketSnapshot::new("base", 1.00, 0.95, 0.10),
                MarketSnapshot::new("compute +4%", 0.98, 1.02, 0.10),
                MarketSnapshot::new("compute -4%", 1.02, 0.98, 0.10),
                MarketSnapshot::new("compute +3%", 0.99, 1.01, 0.10),
                MarketSnapshot::new("compute -3%", 1.01, 0.99, 0.10),
            ],
        };
        let trace = feed.simulate(Mode::Mining, 10.0);
        let modes: Vec<Mode> = trace.iter().map(|(m, _, _)| *m).collect();
        assert!(
            modes.iter().all(|m| *m == Mode::Mining),
            "no flapping under wide hysteresis: {modes:?}"
        );
    }
}
