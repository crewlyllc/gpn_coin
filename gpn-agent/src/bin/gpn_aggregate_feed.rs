//! Read every oracle operator's latest profitability feed and report the
//! per-operator values plus the **median** aggregate — oracle-governance §6
//! gap #3 (client-side median across operators) — and, when any Phase-C
//! operators have bonded, the **bond-weighted median** (spec §5/§8 D3).
//!
//! The pallet now stores each operator's own latest feed in the
//! `ProfitabilityOracle::OperatorFeeds` map (populated on every `update_feed`),
//! alongside the single `CurrentFeed` (latest writer). This tool enumerates
//! that map, prints each operator's feed, the per-field median, and (if any
//! operators have bonded via `StakedOperators`) the bond-weighted median. With
//! `feed_aggregation = "median"` in the agent config, the auto-switch uses the
//! plain median; with `feed_aggregation = "bonded_median"` it uses the
//! bond-weighted median — so one outlier operator cannot move the effective
//! feed, and a large-bond operator dominates.
//!
//! Read-only diagnostic — never writes the chain.
//!
//! Usage:
//!   gpn_aggregate_feed [endpoint]
//!     endpoint  default ws://127.0.0.1:9944
//!
//! Exit code 0 = read one or more operator feeds; 1 = none found / chain down.

use gpn_agent::chain::{aggregate_bonded_median_feed, aggregate_median_feed, ChainClient};

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
        }
        None => {
            println!("[verdict] could not aggregate median (no finite median).");
        }
    }

    // Phase-C: bond-weighted median (spec §5/§8 D3). Align each operator's
    // feed with its `StakedOperators` bond; only bonded operators who have
    // posted count.
    let bonds = client.get_staked_bonds().await;
    let bond_of: std::collections::HashMap<String, u128> =
        bonds.into_iter().collect();
    let mut bfeeds: Vec<gpn_agent::chain::OnChainFeed> = Vec::new();
    let mut bweights: Vec<u128> = Vec::new();
    for (op, f) in ops.iter() {
        if let Some(bond) = bond_of.get(op) {
            bfeeds.push(f.clone());
            bweights.push(*bond);
        }
    }
    if bfeeds.is_empty() {
        println!(
            "[bonded-median] NONE — no Phase-C operator has BOTH bonded (StakedOperators) AND posted a feed."
        );
    } else {
        match aggregate_bonded_median_feed(&bfeeds, &bweights) {
            Some(m) => {
                println!(
                    "[bonded-median] token=${:.6}  gpu=${:.4}/hr  util={:.2}%  apr={:.2}%  @block {} ({} bonded ops)",
                    m.token_price_usd,
                    m.avg_gpu_hour_price_usd,
                    m.utilization * 100.0,
                    m.staking_apr_percent,
                    m.updated_at_block,
                    bfeeds.len(),
                );
                println!(
                    "[verdict] bonded-median ready — agent with feed_aggregation=\"bonded_median\" will use this."
                );
            }
            None => {
                println!(
                    "[verdict] could not aggregate bonded-median (no finite weighted median)."
                );
            }
        }
    }

    if !feeds.is_empty() {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
