//! Seed the on-chain profitability feed — `ProfitabilityOracle.update_feed`.
//!
//! This is the missing link for a **fully live-market-driven auto-switch**: the
//! `ChainClient` already reads on-chain difficulty (real, cycle 21) and the
//! profitability feed (graceful fallback when empty). This tool submits a
//! *signed* `update_feed` extrinsic as an authorized oracle operator, so the
//! token price / GPU-hour price / utilization / staking-APR come from the chain
//! instead of placeholder defaults.
//!
//! In the dev / local chain specs the genesis oracle operators are the well-known
//! dev accounts (`//Alice`, `//Bob`, ...). Alice is an operator in both specs, so
//! signing as Alice lets us seed a `gpn-node --dev` out of the box.
//!
//! The signer + GpnConfig plumbing lives in `gpn_agent::chain::signer` (shared
//! with the agent's `ChainClient` so it can submit signed extrinsics too).
//!
//! Usage (units documented in the oracle pallet):
//!   gpn_seed_feed [endpoint] [token_micro_usd] [gpu_hour_micro_usd] [util_bps] [apr_bps] [signer]
//!
//! Defaults (match the ChainClient fallbacks so a seeded feed == prior behavior):
//!   endpoint       = ws://127.0.0.1:9944
//!   token_micro    = 50_000          ($0.05)
//!   gpu_hour_micro = 2_500_000       ($2.50/hr)
//!   util_bps       = 6_500           (65%)
//!   apr_bps        = 1_200           (12%)
//!   signer         = //Alice
//!
//! Example live run against `gpn-node --dev`:
//!   cargo run -p gpn-agent --bin gpn_seed_feed --release

use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

fn parse_u64(s: &str, default: u64) -> u64 {
    s.parse().unwrap_or(default)
}

fn parse_u32(s: &str, default: u32) -> u32 {
    s.parse().unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let token_micro = parse_u64(args.get(2).map(|s| s.as_str()).unwrap_or("50000"), 50_000);
    let gpu_micro = parse_u64(
        args.get(3).map(|s| s.as_str()).unwrap_or("2500000"),
        2_500_000,
    );
    let util_bps = parse_u32(args.get(4).map(|s| s.as_str()).unwrap_or("6500"), 6_500);
    let apr_bps = parse_u32(args.get(5).map(|s| s.as_str()).unwrap_or("1200"), 1_200);
    let signer_uri = args
        .get(6)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());

    println!(
        "Seeding profitability feed on {} (signer {})",
        endpoint, signer_uri
    );
    println!(
        "  token_price_usd = {:.6} USD   ({})",
        token_micro as f64 / 1_000_000.0,
        token_micro
    );
    println!(
        "  avg_gpu_hour_price = {:.4} USD/hr   ({})",
        gpu_micro as f64 / 1_000_000.0,
        gpu_micro
    );
    println!(
        "  utilization_rate = {:.2}%   ({} bps)",
        util_bps as f64 / 100.0,
        util_bps
    );
    println!(
        "  staking_apr = {:.2}%   ({} bps)",
        apr_bps as f64 / 100.0,
        apr_bps
    );

    // Connect with the runtime-correct GpnConfig + shared signer plumbing.
    let client = signer::connect(&endpoint).await?;
    println!("Connected to GPN chain at {}", endpoint);

    let signer = signer::Sr25519Signer::from_uri(&signer_uri)?;
    println!("Signing as: {}", signer.account_id());

    let nonce = signer::current_nonce(&client, &endpoint, &signer.account_id()).await;
    println!("Account nonce (head): {}", nonce);

    // Construct the dynamic call: ProfitabilityOracle.update_feed(...)
    let call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "update_feed",
        vec![
            Value::u128(token_micro as u128),
            Value::u128(gpu_micro as u128),
            Value::u128(util_bps as u128),
            Value::u128(apr_bps as u128),
        ],
    );

    println!("Submitting update_feed extrinsic...");
    signer::submit_signed(&client, &call, &signer, nonce).await?;
    println!("✅ update_feed finalized and succeeded.");

    // Verify: re-read the on-chain feed back.
    let feed_addr = subxt::dynamic::storage("ProfitabilityOracle", "CurrentFeed", vec![]);
    let stored = client
        .storage()
        .at_latest()
        .await?
        .fetch(&feed_addr)
        .await?;
    match stored {
        Some(thunk) => {
            let val = thunk.to_value()?.remove_context();
            println!("On-chain CurrentFeed now set:");
            println!("  {:?}", val);
        }
        None => {
            println!("⚠️ CurrentFeed still empty after seed — check operator/interval.");
        }
    }

    Ok(())
}
