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
//! The signer is implemented directly on `sp_core::sr25519::Pair` (v28) against
//! subxt's `Signer` trait, so we avoid the `substrate-compat` feature (which
//! drags in a conflicting sp-core v31).
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

use sp_core::{crypto::Pair as _, sr25519};
use subxt::{
    config::{substrate::{BlakeTwo256, H256, MultiSignature, SubstrateHeader}, Config, DefaultExtrinsicParams},
    dynamic::Value,
    tx::Signer,
    utils::AccountId32,
    OnlineClient,
};

/// Custom config matching the GPN runtime: `Address = AccountId32` (bare),
/// NOT `MultiAddress`. The GPN runtime declares `pub type Address = AccountId`,
/// so the default `SubstrateConfig` (which wraps in `MultiAddress`) adds an
/// extra SCALE variant byte that breaks extrinsic decoding on the node.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum GpnConfig {}

impl Config for GpnConfig {
    type Hash = H256;
    type AccountId = AccountId32;
    type Address = AccountId32;
    type Signature = MultiSignature;
    type Hasher = BlakeTwo256;
    type Header = SubstrateHeader<u32, BlakeTwo256>;
    type ExtrinsicParams = DefaultExtrinsicParams<Self>;
    type AssetId = u32;
}

/// A `Signer` backed by an `sp_core::sr25519::Pair` (v28), mapped into the GPN
/// runtime's types (AccountId32 / MultiSignature).
struct Sr25519Signer {
    pair: sr25519::Pair,
}

impl Signer<GpnConfig> for Sr25519Signer {
    fn account_id(&self) -> AccountId32 {
        AccountId32(self.pair.public().0)
    }

    fn address(&self) -> AccountId32 {
        AccountId32(self.pair.public().0)
    }

    fn sign(&self, signer_payload: &[u8]) -> MultiSignature {
        let sig = self.pair.sign(signer_payload);
        MultiSignature::Sr25519(sig.0)
    }
}

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
    let gpu_micro = parse_u64(args.get(3).map(|s| s.as_str()).unwrap_or("2500000"), 2_500_000);
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

    // Connect.
    let client = OnlineClient::<GpnConfig>::from_url(&endpoint).await?;
    println!("Connected to GPN chain at {}", endpoint);

    // Build the signer from an sp_core sr25519 pair (Substrate URI, e.g. //Alice).
    let pair = sr25519::Pair::from_string(&signer_uri, None)
        .map_err(|e| format!("invalid signer URI '{}': {}", signer_uri, e))?;
    let signer = Sr25519Signer { pair };
    println!("Signing as: {}", signer.account_id());

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
    // Use an immortal transaction (robust on fast-block dev chains where a
    // mortal 32-block window can expire before the tx is included).
    let params = subxt::config::DefaultExtrinsicParamsBuilder::<GpnConfig>::new().build();
    let events = client
        .tx()
        .sign_and_submit_then_watch(&call, &signer, params)
        .await?
        .wait_for_finalized_success()
        .await?;

    println!("✅ update_feed finalized and succeeded.");
    for event in events.iter() {
        let ev = event?;
        let field_count = ev.field_values().map(|c| c.len()).unwrap_or(0);
        println!("  event: {}::{}({} fields)", ev.pallet_name(), ev.variant_name(), field_count);
    }

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
