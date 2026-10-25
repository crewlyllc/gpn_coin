//! Read the on-chain profitability feed and report whether it is **plausible**
//! enough to act on — oracle-governance §6 gap #2 (feed reasonableness bounds
//! on `calc_compute_revenue`).
//!
//! A single bad/outlier feed (unit misconversion, a garbage GPU-hour price, or
//! NaN/Inf) would otherwise inflate compute revenue and force a mass switch to
//! Compute. This tool prints the raw feed, the resulting compute revenue, and
//! the reasonableness verdict against the configurable ceiling
//! (`compute_revenue_max_usd_per_hour`, default 50 USD/hr). It is a read-only
//! diagnostic / operator sanity check — it never writes the chain.
//!
//! Usage:
//!   gpn_check_feed [endpoint] [max_usd_per_hour]
//!     endpoint         default ws://127.0.0.1:9944
//!     max_usd_per_hour default 50.0  (must match the agent config you run)
//!
//! Exit codes (item #108 — the four states are deliberately distinguishable,
//! because "the chain carries no feed" and "I could not read the chain" are
//! different facts and only the first one is evidence about the chain):
//!   0 = feed present and reasonable
//!   1 = feed present but IMPLAUSIBLE — force conservative Mining
//!   2 = no feed on chain (the read succeeded; the key is empty)
//!   3 = could not read the chain — nothing can be concluded, do not trust a
//!       fallback verdict either way (the reason is printed)
//! Any non-zero code still means "do not act on the feed", so callers that
//! only test `!= 0` keep working.
//!
//! Example (implausible): seed a $10,000/hr GPU price, then
//!   gpn_check_feed            -> "compute_revenue_reasonable = false" (exit 1)
//! Example (plausible): seed $2.50/hr @ 65%, then
//!   gpn_check_feed            -> "compute_revenue_reasonable = true"  (exit 0)

use gpn_agent::chain::ChainClient;
use gpn_agent::cli;
use gpn_agent::profitability::{
    calc_compute_revenue, classify_feed_read, compute_revenue_reasonable, FeedRead,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let max_usd_per_hour = cli::opt_num("max_usd_per_hour", args.get(2).map(|s| s.as_str()), 50.0)?;

    let client = ChainClient::new(&endpoint).await?;

    let feed = match classify_feed_read(client.get_feed_read().await) {
        FeedRead::Present(f) => f,
        FeedRead::Absent => {
            // A real answer about the chain: the read worked and there is
            // nothing stored. This is the only case where "absent" is true.
            println!("[feed] ABSENT — the chain at {endpoint} was read successfully and carries no ProfitabilityOracle::CurrentFeed.");
            println!("[verdict] no feed to assess (agent will use per-field fallbacks).");
            std::process::exit(2);
        }
        FeedRead::Unreadable(why) => {
            // NOT evidence about the chain — say so, and do not print a
            // verdict that a cron or an operator could mistake for one.
            eprintln!("[feed] UNREADABLE — could not read the feed from {endpoint}: {why}");
            eprintln!(
                "[verdict] no conclusion about the chain was reached; this is a reader/connectivity \
                 failure, not an absent or implausible feed."
            );
            std::process::exit(3);
        }
    };

    let raw = calc_compute_revenue(feed.avg_gpu_hour_price_usd, feed.utilization);
    let reasonable = compute_revenue_reasonable(raw, max_usd_per_hour);

    println!("[feed] token_price_usd    = ${:.6}", feed.token_price_usd);
    println!(
        "[feed] avg_gpu_hour_price  = ${:.4}/hr",
        feed.avg_gpu_hour_price_usd
    );
    println!(
        "[feed] utilization_rate   = {:.2}%",
        feed.utilization * 100.0
    );
    println!(
        "[feed] staking_apr        = {:.2}%",
        feed.staking_apr_percent
    );
    println!("[feed] updated_at_block    = {}", feed.updated_at_block);
    println!(
        "[calc] compute_revenue   = ${:.4}/hr  (gpu x util x 0.85)",
        raw
    );
    println!(
        "[verdict] compute_revenue_reasonable(max=${:.2}/hr) = {}",
        max_usd_per_hour, reasonable
    );
    if reasonable {
        println!("[verdict] feed is plausible — auto-switch may act on it.");
        std::process::exit(0);
    } else {
        println!("[verdict] feed IMPLAUSIBLE — agent will force conservative Mining (compute=0).");
        std::process::exit(1);
    }
}
