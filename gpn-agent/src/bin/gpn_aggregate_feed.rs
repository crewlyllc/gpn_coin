//! Read every oracle operator's latest profitability feed and report the
//! per-operator values plus the **median** aggregate — oracle-governance §6
//! gap #3 (client-side median across operators).
//!
//! The pallet now stores each operator's own latest feed in the
//! `ProfitabilityOracle::OperatorFeeds` map (populated on every `update_feed`),
//! alongside the single `CurrentFeed` (latest writer). This tool enumerates
//! that map, prints each operator's feed, and the per-field median. With
//! `feed_aggregation = "median"` in the agent config, the auto-switch uses this
//! same median so one outlier operator cannot move the effective feed.
//!
//! Read-only diagnostic — never writes the chain.
//!
//! Usage:
//!   gpn_aggregate_feed [endpoint]
//!     endpoint  default ws://127.0.0.1:9944
//!
//! Exit code 0 = read one or more operator feeds; 1 = none found / chain down.

use gpn_agent::chain::{aggregate_median_feed, ChainClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());

    let client = ChainClient::new(&endpoint).await?;
    let ops = client.get_operator_feeds().await;

    if ops.is_empty() {
        println!(
            "[operator-feeds] NONE — no ProfitabilityOracle::OperatorFeeds on chain "
        );
        println!("                 (no operator has posted a feed yet, or chain unreachable).");
        std::process::exit(1);
    }

    println!("[operator-feeds] {} operator(s) have posted a feed:", ops.len());
    for (i, (op, f)) in ops.iter().enumerate() {
        println!(
            "  #{:<2} {}  token=${:.6}  gpu=${:.4}/hr  util={:.2}%  apr={:.2}%  @block {}",
            i + 1,
            op,
            f.token_price_usd,
            f.avg_gpu_hour_price_usd,
            f.utilization * 100.0,
            f.staking_apr_percent,
            f.updated_at_block,
        );
    }

    let feeds: Vec<gpn_agent::chain::OnChainFeed> =
        ops.iter().map(|(_, f)| f.clone()).collect();
    match aggregate_median_feed(&feeds) {
        Some(m) => {
            println!(
                "[median]  token=${:.6}  gpu=${:.4}/hr  util={:.2}%  apr={:.2}%  @block {} ({} ops)",
                m.token_price_usd,
                m.avg_gpu_hour_price_usd,
                m.utilization * 100.0,
                m.staking_apr_percent,
                m.updated_at_block,
                ops.len(),
            );
            println!(
                "[verdict] median aggregate ready — agent with feed_aggregation=\"median\" will use this."
            );
            std::process::exit(0);
        }
        None => {
            println!("[verdict] could not aggregate (no finite median).");
            std::process::exit(1);
        }
    }
}
