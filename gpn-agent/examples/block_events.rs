//! block_events — decode and print the events of one block by name.
//!
//! `cargo run --release --example block_events -- ws://127.0.0.1:9944 347621`
//! or pass a 0x block hash instead of a number.
//! The node returns extrinsics/events as opaque bytes; this prints
//! `Pallet::Variant {fields}` so a dispatch outcome can be quoted verbatim.

use gpn_agent::chain::signer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let arg = std::env::args().nth(2).expect("block number or hash");
    let http = endpoint
        .replacen("ws://", "http://", 1)
        .replacen("wss://", "https://", 1);
    let client = signer::connect(&endpoint).await?;

    let hash: subxt::config::substrate::H256 = if arg.starts_with("0x") {
        arg.parse()?
    } else {
        let body = serde_json::json!({"id":1,"jsonrpc":"2.0","method":"chain_getBlockHash",
            "params":[arg.parse::<u64>()?]});
        let resp: serde_json::Value = reqwest::Client::new()
            .post(&http)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        resp["result"].as_str().expect("block hash").parse()?
    };
    println!("block {hash:?}");

    // NOTE: `client.blocks().at()` cannot decode this runtime's block body (the node
    // returns `extrinsics` as arrays of byte values, not hex strings) — use raw RPC
    // for the body and subxt only for events.
    let body = serde_json::json!({"id":1,"jsonrpc":"2.0","method":"chain_getBlock","params":[format!("{hash:?}")]});
    let resp: serde_json::Value = reqwest::Client::new()
        .post(&http)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    let exts = resp["result"]["block"]["extrinsics"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    println!("extrinsics: {}", exts.len());
    for (i, e) in exts.iter().enumerate() {
        let n = match e {
            serde_json::Value::Array(a) => a.len(),
            serde_json::Value::String(s) => (s.len() - 2) / 2,
            _ => 0,
        };
        println!("  ext[{i}] {n} bytes");
    }

    let evs = client.events().at(hash).await?;
    println!("events:");
    for ev in evs.iter() {
        match ev {
            Ok(e) => println!(
                "  {}::{} {:?}",
                e.pallet_name(),
                e.variant_name(),
                e.field_values()
            ),
            Err(e) => println!("  <decode error: {e}>"),
        }
    }
    Ok(())
}
