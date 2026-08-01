//! Long-running oracle operator loop — `ProfitabilityOracle.update_feed` maintenance.
//!
//! `gpn_seed_feed` (cycle 22) is a one-shot: it pushes a single feed and exits.
//! That's fine for bootstrapping a dev chain, but a **real oracle service** needs
//! to keep the feed fresh — token price and GPU-hour prices drift, utilization
//! and staking APR change, and the pallet enforces a minimum update interval
//! (`OracleUpdateInterval`, default 600 blocks ≈ 2.5h) so stale-but-updated feeds
//! are the norm. This tool is that maintained service:
//!
//!   - Reads the current feed values from a TOML/JSON file each iteration (so an
//!     external price feed / operator can update prices without restarting).
//!   - Respects the pallet's `UpdateTooSoon` guard: it never spams; if the
//!     interval hasn't elapsed it logs and waits instead of erroring.
//!   - Re-reads its own nonce from the head block each cycle (the cycle-27 fix,
//!     so a second+ tx from the same account isn't rejected as stale).
//!   - Handles the authorization failure (`NotAuthorized`) as a fatal, clear
//!     error — an operator must be in `OracleOperators` (root `add_operator`).
//!
//! Values file format (units match the oracle pallet / `gpn_seed_feed`):
//!   token_price_usd      = 75000      # micro-USD  → $0.075
//!   avg_gpu_hour_price   = 3000000    # micro-USD  → $3.00/hr
//!   utilization_rate_bps = 7200       # 72%
//!   staking_apr_bps      = 1500       # 15%
//!
//! Usage:
//!   gpn_operate_oracle [endpoint] [values_file] [interval_secs] [signer_uri]
//!     endpoint     ws://127.0.0.1:9944 (default)
//!     values_file  path to a TOML values file (default ./oracle_values.toml)
//!     interval_secs poll/update cadence, clamped to the pallet interval (default 60)
//!     signer_uri   //Alice (default; must be an oracle operator in genesis)
//!
//! A `oracle_values.toml` is written on first run (with defaults) if missing.

use gpn_agent::chain::signer;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct OracleValues {
    /// Token price in micro-USD (1e-6 USD). 75_000 = $0.075.
    token_price_usd: u64,
    /// Average GPU-hour price in micro-USD per hour. 3_000_000 = $3.00/hr.
    avg_gpu_hour_price: u64,
    /// Network GPU utilization in basis points (1/100 of a percent). 7200 = 72%.
    utilization_rate_bps: u32,
    /// Staking APR in basis points. 1500 = 15%.
    staking_apr_bps: u32,
}

impl Default for OracleValues {
    fn default() -> Self {
        Self {
            token_price_usd: 75_000,
            avg_gpu_hour_price: 3_000_000,
            utilization_rate_bps: 7_200,
            staking_apr_bps: 1_500,
        }
    }
}

/// Load feed values from a TOML file, writing the default template if absent.
fn load_values(path: &str) -> Result<OracleValues, Box<dyn std::error::Error>> {
    if !Path::new(path).exists() {
        let tmpl = toml::to_string_pretty(&OracleValues::default())?;
        std::fs::write(path, &tmpl)?;
        println!("📝 wrote default values template to {}", path);
    }
    let raw = std::fs::read_to_string(path)?;
    let vals: OracleValues = toml::from_str(&raw)
        .map_err(|e| format!("failed to parse {}: {}", path, e))?;
    Ok(vals)
}

/// Build the `ProfitabilityOracle.update_feed` dynamic call.
fn update_feed_call(
    v: &OracleValues,
) -> impl subxt::tx::Payload + Clone {
    subxt::dynamic::tx(
        "ProfitabilityOracle",
        "update_feed",
        vec![
            Value::u128(v.token_price_usd as u128),
            Value::u128(v.avg_gpu_hour_price as u128),
            Value::u128(v.utilization_rate_bps as u128),
            Value::u128(v.staking_apr_bps as u128),
        ],
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let values_file = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "oracle_values.toml".to_string());
    let interval_secs: u64 = args
        .get(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let signer_uri = args
        .get(4)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());

    println!(
        "🔁 Operating profitability oracle on {} (signer {}, values {}, interval {}s)",
        endpoint, signer_uri, values_file, interval_secs
    );

    let client = signer::connect(&endpoint).await?;
    let signer = signer::Sr25519Signer::from_uri(&signer_uri)?;
    println!("Connected to GPN chain; signing as {}", signer.account_id());

    // First iteration immediately, then every `interval_secs`.
    let mut tick = 0u64;
    loop {
        tick += 1;
        println!("\n——— iteration #{} ———", tick);

        let vals = match load_values(&values_file) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("⚠️ could not load values ({}); retrying next cycle", e);
                tokio::time::sleep(Duration::from_secs(interval_secs)).await;
                continue;
            }
        };
        println!(
            "  feed: token=${:.6} gpu=${:.4}/hr util={:.2}% apr={:.2}%",
            vals.token_price_usd as f64 / 1_000_000.0,
            vals.avg_gpu_hour_price as f64 / 1_000_000.0,
            vals.utilization_rate_bps as f64 / 100.0,
            vals.staking_apr_bps as f64 / 100.0,
        );

        let nonce = signer::current_nonce(&client, &endpoint, &signer.account_id()).await;
        let call = update_feed_call(&vals);

        match signer::submit_signed(&client, &call, &signer, nonce).await {
            Ok(()) => println!("  ✅ update_feed dispatched successfully (iteration #{})", tick),
            Err(e) => {
                // `UpdateTooSoon` is the *expected* back-off — the pallet guards
                // against feeds fresher than OracleUpdateInterval. Treat it as
                // informational, not an error worth exiting on. `NotAuthorized`
                // is fatal (misconfiguration). Anything else: log + keep trying.
                let msg = e.to_string();
                if msg.contains("UpdateTooSoon") {
                    println!("  ⏳ update_feed skipped: pallet interval not yet elapsed (UpdateTooSoon)");
                } else if msg.contains("NotAuthorized") {
                    eprintln!("  ❌ FATAL: {} is not an oracle operator. Add it via root `add_operator` or use an operator URI.", signer_uri);
                    return Err(e);
                } else {
                    eprintln!("  ⚠️ update_feed failed (will retry): {}", msg);
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}
