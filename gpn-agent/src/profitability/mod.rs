/// Profitability calculator and auto-switch decision engine
/// Determines the most profitable mode for the GPU at any time.
pub mod scripted;

use crate::{chain::ChainClient, config::AgentConfig, gpu::GPUMonitor, Mode};
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

/// Expected USD/hour from serving AI compute.
///
/// `gpu_price` is the marketplace's average GPU-hour price **already in
/// USD** (the oracle stores micro-USD; `OnChainFeed` converts to USD). It
/// must NOT be multiplied by `token_price` again — doing so produced USD^2
/// and deflated compute revenue ~13x at token=$0.075, which is why the live
/// chain feed never recommended Compute (only the scripted feed, which
/// bypasses this calc, ever triggered job claiming). Fixed cycle 31.
pub fn calc_compute_revenue(gpu_price: f64, utilization: f64) -> f64 {
    let overhead_factor = 0.85; // 15% overhead for setup/teardown
    gpu_price * utilization * overhead_factor
}

/// Oracle-governance §6 gap #2: is a computed compute revenue (USD/hour)
/// **plausible** enough to act on? Guards the auto-switch against a single
/// bad/outlier profitability feed (unit misconversion — the cycle-31 USD²
/// class — a garbage value, or NaN/Inf) that would otherwise inflate compute
/// revenue and force a mass switch to Compute.
///
/// A revenue is reasonable iff it is finite, positive, and within
/// `max_usd_per_hour`. (Since `calc_compute_revenue` is monotonic in both
/// inputs, one ceiling on the *output* bounds every implausible input
/// combination: a negative/huge `utilization` or `gpu_price` surfaces as a
/// negative/huge revenue, both rejected here.)
pub fn compute_revenue_reasonable(revenue: f64, max_usd_per_hour: f64) -> bool {
    revenue.is_finite() && revenue > 0.0 && revenue <= max_usd_per_hour
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
            let step = self
                .scripted_step
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

        // Live chain feed (single fetch — the old code read the feed 4x, one
        // network round-trip per field; we now fetch once and reuse it).
        let difficulty = self.chain_client.get_difficulty().await?;
        let mut feed = self.chain_client.get_feed().await;
        let head_block = self.chain_client.head_block_number().await;

        // Oracle-governance §6 gap #3: when `feed_aggregation = "median"`,
        // aggregate ALL operators' latest feeds via a per-field median so one
        // outlier operator cannot move the effective feed. Falls back to the
        // single `CurrentFeed` (latest writer) if no operator has posted yet.
        if self.config.feed_aggregation == "median" {
            let ops = self.chain_client.get_operator_feeds().await;
            let feeds: Vec<crate::chain::OnChainFeed> =
                ops.iter().map(|(_, f)| f.clone()).collect();
            match crate::chain::aggregate_median_feed(&feeds) {
                Some(m) => {
                    log::info!(
                        "feed_aggregation=median: aggregated {} operator feed(s) -> token=${:.4} gpu=${:.2}/hr util={:.2} apr={:.1}%",
                        feeds.len(), m.token_price_usd, m.avg_gpu_hour_price_usd,
                        m.utilization, m.staking_apr_percent
                    );
                    feed = Some(m);
                }
                None => {
                    log::warn!(
                        "feed_aggregation=median but no operator feeds on chain; falling back to CurrentFeed (latest writer)."
                    );
                }
            }
        }

        // Oracle-governance §6 gap #5 / Phase-C aggregation (spec §5/§8 D3):
        // when `feed_aggregation = "bonded_median"`, aggregate every bonded
        // operator's latest feed via a **bond-weighted** median (bond from
        // `StakedOperators`, feed from `OperatorFeeds`). A large-bond operator
        // dominates the effective feed; a tiny-bond outlier cannot move it.
        // This is the on-chain economic-weight counterpart of the plain
        // `median` (client-side, weight-agnostic). Falls back to `CurrentFeed`
        // if no bonded operator has posted yet.
        if self.config.feed_aggregation == "bonded_median" {
            let ops = self.chain_client.get_operator_feeds().await;
            let bonds = self.chain_client.get_staked_bonds().await;
            // Align operator feeds with their bonds by account key; only keep
            // operators who appear in BOTH maps (bonded AND have posted).
            let bond_of: std::collections::HashMap<String, u128> = bonds.into_iter().collect();
            let mut feeds: Vec<crate::chain::OnChainFeed> = Vec::new();
            let mut bond_weights: Vec<u128> = Vec::new();
            for (op, f) in ops.iter() {
                if let Some(bond) = bond_of.get(op) {
                    feeds.push(f.clone());
                    bond_weights.push(*bond);
                }
            }
            match crate::chain::aggregate_bonded_median_feed(&feeds, &bond_weights) {
                Some(m) => {
                    log::info!(
                        "feed_aggregation=bonded_median: aggregated {} bonded operator feed(s) -> token=${:.4} gpu=${:.2}/hr util={:.2} apr={:.1}%",
                        feeds.len(), m.token_price_usd, m.avg_gpu_hour_price_usd,
                        m.utilization, m.staking_apr_percent
                    );
                    feed = Some(m);
                }
                None => {
                    log::warn!(
                        "feed_aggregation=bonded_median but no bonded operator feeds on chain; falling back to CurrentFeed (latest writer)."
                    );
                }
            }
        }

        // Staleness guard (oracle-governance §6 gap #1): if the feed was
        // written more than `feed_staleness_ttl_blocks` ago, DON'T trust its
        // prices — fall back to a conservative Mining default (compute and
        // staking revenue forced to 0, so mining always wins the switch). A
        // stale feed could otherwise drive the whole network off mining onto
        // outdated compute/staking prices.
        let ttl = self.config.feed_staleness_ttl_blocks;
        let stale = feed
            .as_ref()
            .zip(head_block)
            .map(|(f, h)| crate::chain::is_feed_stale(f.updated_at_block, h, ttl))
            .unwrap_or(false);
        if stale {
            log::warn!(
                "Profitability feed STALE (updated at block {}, head {}, TTL {:?} blocks). Forcing conservative Mining default.",
                feed.as_ref().map(|f| f.updated_at_block).unwrap_or(0),
                head_block.unwrap_or(0),
                ttl
            );
        }

        // Resolve feed fields (fall back to the existing per-field getters —
        // which themselves fall back to placeholder defaults when the feed is
        // empty — only when the whole feed is unavailable).
        let (token_price, gpu_price, utilization, staking_apr) = match &feed {
            Some(f) => (
                f.token_price_usd,
                f.avg_gpu_hour_price_usd,
                f.utilization,
                f.staking_apr_percent,
            ),
            None => (
                self.chain_client.get_token_price_usd().await?,
                self.chain_client
                    .get_gpu_hour_price(&self.config.gpu_model)
                    .await?,
                self.chain_client
                    .get_utilization_rate(&self.config.gpu_model)
                    .await?,
                self.chain_client.get_staking_apr().await?,
            ),
        };

        // Fetch local GPU data
        let gpu_info = self.gpu_monitor.get_gpu_info()?;

        // Calculate revenues. A stale feed forces mining: compute + staking
        // revenue = 0, so `recommended_mode()` picks Mining.
        let mining =
            self.calc_mining_revenue(gpu_info.current_hashrate_mhs, difficulty, token_price);
        let (compute, staking) = if stale {
            (0.0, 0.0)
        } else {
            let compute_raw = calc_compute_revenue(gpu_price, utilization);
            // Oracle-governance §6 gap #2: feed reasonableness bounds. A
            // compute revenue outside the plausible USD/hr ceiling means the
            // feed is bad (units/garbage/outlier) — fall back to Mining so a
            // single bad feed can't force a mass switch to Compute.
            let max = self.config.compute_revenue_max_usd_per_hour;
            let compute = if compute_revenue_reasonable(compute_raw, max) {
                compute_raw
            } else {
                log::warn!(
                    "Compute revenue ${:.4}/hr implausible (max ${:.2}/hr). Rejecting feed -> forcing conservative Mining default.",
                    compute_raw, max
                );
                0.0
            };
            (
                compute,
                self.calc_staking_revenue(self.config.staked_amount, staking_apr, token_price),
            )
        };

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

    /// Expected USD/hour from serving AI compute.
    ///
    /// `gpu_price` is the marketplace's average GPU-hour price **already in
    /// USD** (the oracle stores micro-USD; `OnChainFeed` converts to USD). It
    /// must NOT be multiplied by `token_price` again — doing so produced USD^2
    /// and deflated compute revenue ~13x at token=$0.075, which is why the live
    /// chain feed never recommended Compute (only the scripted feed, which
    /// bypasses this calc, ever triggered job claiming). Fixed cycle 31.

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
    fn compute_revenue_is_in_usd_not_double_converted() {
        // Regression (cycle 31): gpu_price is already USD from the oracle feed;
        // multiplying by token_price produced USD^2 and deflated compute revenue
        // ~13x (at token=$0.075, $2.5/hr -> $0.069/hr), so the live chain feed
        // never recommended Compute. Correct value = price x util x overhead.
        let revenue = calc_compute_revenue(2.5, 0.65);
        let expected = 2.5 * 0.65 * 0.85; // 1.38125 USD/hr
        assert!(
            (revenue - expected).abs() < 1e-9,
            "expected {expected} USD/hr, got {revenue}"
        );
        // Sanity: it must be ~USD-scale, not deflated to ~$0.07.
        assert!(
            revenue > 1.0,
            "compute revenue should be ~$1.38/hr, got {revenue}"
        );
    }

    #[test]
    fn compute_revenue_reasonable_accepts_plausible_and_rejects_bad() {
        // Plausible (within the 50 USD/hr ceiling).
        assert!(compute_revenue_reasonable(1.38, 50.0));
        assert!(compute_revenue_reasonable(50.0, 50.0)); // boundary == max is OK
                                                         // Implausible: zero / negative (garbage utilization or price).
        assert!(!compute_revenue_reasonable(0.0, 50.0));
        assert!(!compute_revenue_reasonable(-1.0, 50.0));
        // Implausible: absurdly high (unit misconversion / outlier feed).
        assert!(!compute_revenue_reasonable(51.0, 50.0));
        assert!(!compute_revenue_reasonable(10_000.0, 50.0));
        // Implausible: non-finite (NaN / ±Inf from a corrupt feed).
        assert!(!compute_revenue_reasonable(f64::NAN, 50.0));
        assert!(!compute_revenue_reasonable(f64::INFINITY, 50.0));
        assert!(!compute_revenue_reasonable(f64::NEG_INFINITY, 50.0));
        // A tighter ceiling rejects what a looser one allows.
        assert!(compute_revenue_reasonable(1.38, 2.0));
        assert!(!compute_revenue_reasonable(1.38, 1.0));
    }

    #[test]
    fn implausible_feed_revenue_forces_mining() {
        // §6 gap #2 decision contract: if the feed's compute revenue is
        // rejected (0), the auto-switch must recommend Mining even against a
        // tiny mining revenue — a bad feed can't force Compute.
        let a = analysis(0.05, 0.0, 0.0); // implausible compute -> 0
        assert_eq!(a.recommended_mode(), Mode::Mining);
        // And a plausible compute still beats mining (switch is allowed).
        let ok = analysis(0.05, 2.0, 0.0);
        assert_eq!(ok.recommended_mode(), Mode::Compute);
    }

    #[test]
    fn stale_feed_zeroed_compute_staking_forces_mining() {
        // Oracle-governance §6 gap #1: when the feed is stale, analyze() forces
        // compute + staking revenue to 0 so mining always wins. This test locks
        // in that decision contract (recommended_mode -> Mining).
        let a = analysis(0.5, 0.0, 0.0); // stale: compute/staking zeroed
        assert_eq!(a.recommended_mode(), Mode::Mining);
        // Mining with zeroed compute/staking beats even a low mining revenue.
        let a2 = analysis(0.01, 0.0, 0.0);
        assert_eq!(a2.recommended_mode(), Mode::Mining);
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
