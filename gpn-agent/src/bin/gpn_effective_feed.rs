//! Read the **on-chain** Phase-C bond-weighted median profitability feed via
//! the `gpn_effectiveFeed` RPC — the authoritative aggregation computed by
//! `ProfitabilityOracle::effective_feed()` in the pallet.
//!
//! This is the single source of truth for the aggregate feed: unlike
//! `gpn_aggregate_feed` (which reimplements the bond-weighted median
//! client-side from `OperatorFeeds` + `StakedOperators`), this tool calls the
//! runtime API directly, so the reported median is exactly what the chain
//! itself would use for slashing-outlier detection and (with
//! `feed_aggregation = "bonded_median"`) the auto-switch.
//!
//! Read-only — never writes the chain.
//!
//! Usage:
//!   gpn_effective_feed [endpoint]
//!     endpoint  default ws://127.0.0.1:9944
//!
//! Exit code 0 = on-chain effective feed available; 1 = none (no bonded
//! operator has posted, or chain/RPC unreachable).

use gpn_agent::chain::ChainClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());

    let client = ChainClient::new(&endpoint).await?;

    match client.get_effective_feed().await {
        Some(f) => {
            println!("[effective-feed] ON-CHAIN bond-weighted median (ProfitabilityOracle::effective_feed):");
            println!("  token price      = ${:.6}", f.token_price_usd);
            println!("  gpu hour price   = ${:.4}/hr", f.avg_gpu_hour_price_usd);
            println!("  utilization      = {:.2}%", f.utilization * 100.0);
            println!("  staking APR      = {:.2}%", f.staking_apr_percent);
            println!("  updated at block = {}", f.updated_at_block);
            println!("[verdict] on-chain effective feed available — this is what the auto-switch");
            println!("          consumes with feed_aggregation=\"bonded_median\".");
            std::process::exit(0);
        }
        None => {
            println!("[effective-feed] NONE — no bonded (Phase-C) operator has posted a feed yet,");
            println!("                 or chain/RPC unreachable.");
            println!("[verdict] no on-chain effective feed; agent falls back to CurrentFeed / defaults.");
            std::process::exit(1);
        }
    }
}
