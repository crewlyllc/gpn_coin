//! unsigned_entry_probe — reproduce the "Transaction has a bad signature"
//! rejection of the unsigned provider-entry extrinsic without mining.
//!
//! Prints the resolved pallet/call indices from the live metadata, the encoded
//! unsigned extrinsic hex, and then submits it with a structurally valid but
//! unverified `EntryPow` (the pool rejects on the signature before verifying
//! the proof, so this is enough to isolate the encoding bug).
//!
//! Run: `cargo run --example unsigned_entry_probe -- ws://127.0.0.1:9944`

#[allow(unused_imports)]
use gpn_agent::chain::signer::GpnConfig;
use subxt::tx::Payload;
use subxt::OnlineClient;

use gpn_agent::chain::signer;
use gpn_agent::onboard::{register_provider_entry_unsigned_call, EntryParams, EntryPow};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());

    let client = signer::connect(&endpoint).await?;
    let meta = client.metadata();

    // Resolved indices straight from the chain metadata.
    for name in [
        "register_provider",
        "register_provider_entry",
        "register_provider_entry_unsigned",
    ] {
        match meta.pallet_by_name("ComputeMarketplace") {
            Some(p) => {
                let calls: Vec<(u8, String)> = p
                    .call_variants()
                    .unwrap_or(&[])
                    .iter()
                    .map(|v| (v.index, v.name.clone()))
                    .collect();
                let found = calls.iter().find(|(_, n)| n == name);
                println!(
                    "pallet ComputeMarketplace index={} call {name:35} -> {:?}",
                    p.index(),
                    found
                );
            }
            None => println!("ComputeMarketplace pallet NOT in metadata"),
        }
    }
    let cm = meta.pallet_by_name("ComputeMarketplace").unwrap();
    println!(
        "all calls: {:?}",
        cm.call_variants()
            .unwrap_or(&[])
            .iter()
            .map(|v| format!("{}({})", v.index, v.name))
            .collect::<Vec<_>>()
    );

    let pow = EntryPow {
        nonce: 1,
        mix_hash: [2u8; 32],
        solution_hash: [3u8; 32],
        algorithm_version: 1,
    };
    let params = EntryParams {
        challenge_response: [4u8; 32],
        gpu_info_hash: [5u8; 32],
        gpu_count: 1,
        compute_gflops: 1,
        requirements: Default::default(),
    };

    let acct = [7u8; 32];
    let call = register_provider_entry_unsigned_call(&acct, &pow, &params);
    let encoded = call.encode_call_data(&meta)?;
    println!("\ncall-data hex   = 0x{}", hex::encode(&encoded));
    println!(
        "  bytes: pallet={} call={} (expect 11 / 20 from source)",
        encoded[0], encoded[1]
    );

    let unsigned = client.tx().create_unsigned(&call)?;
    let bytes = unsigned.encoded();
    println!("\nunsigned hex    = 0x{}", hex::encode(bytes));
    println!("  version byte  = 0x{:02x}", bytes[0]);
    println!("  then pallet/call = 0x{:02x} 0x{:02x}", bytes[1], bytes[2]);

    let hexstr = format!("0x{}", hex::encode(bytes));
    let http = endpoint
        .replacen("ws://", "http://", 1)
        .replacen("wss://", "https://", 1);
    let body = serde_json::json!({
        "id": 1, "jsonrpc": "2.0",
        "method": "author_submitExtrinsic",
        "params": [hexstr],
    });
    let resp: serde_json::Value = reqwest::Client::new()
        .post(&http)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    println!("\nsubmit result   = {resp}");
    Ok(())
}
