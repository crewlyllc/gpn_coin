//! entry_state_probe — dump the live entry-gate state so a rejected unsigned
//! entry can be attributed to a specific check instead of "bad signature".
//!
//! Run: `cargo run --example entry_state_probe -- ws://127.0.0.1:9944`

use futures::StreamExt;
use subxt::dynamic::Value;
use subxt::OnlineClient;

use gpn_agent::chain::signer::{self, GpnConfig};

async fn value_at(
    at: &subxt::storage::Storage<GpnConfig, subxt::OnlineClient<GpnConfig>>,
    pallet: &str,
    entry: &str,
) -> Option<Vec<u8>> {
    let q = subxt::dynamic::storage(pallet, entry, ());
    match at.fetch(&q).await {
        Ok(Some(v)) => Some(v.encoded().to_vec()),
        Ok(None) => None,
        Err(e) => {
            println!("  {pallet}.{entry}: fetch error {e}");
            None
        }
    }
}

async fn count_iter(
    at: &subxt::storage::Storage<GpnConfig, subxt::OnlineClient<GpnConfig>>,
    pallet: &str,
    entry: &str,
) {
    let q = subxt::dynamic::storage(pallet, entry, ());
    {
        match at.iter(q).await {
            Ok(mut it) => {
                let mut n = 0usize;
                while let Some(item) = it.next().await {
                    match item {
                        Ok(kv) => {
                            n += 1;
                            if n <= 12 {
                                println!("    [{n}] key={:02x?}", kv.key_bytes);
                            }
                        }
                        Err(e) => {
                            println!("    iter error {e}");
                            break;
                        }
                    }
                }
                println!("  {pallet}.{entry}: {n} entries");
            }
            Err(e) => println!("  {pallet}.{entry}: iter start error {e}"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let client = signer::connect(&endpoint).await?;
    let head: subxt::utils::H256 = async {
        let body = serde_json::json!({"id":1,"jsonrpc":"2.0","method":"chain_getBlockHash","params":[]});
        let http = endpoint.replacen("ws://", "http://", 1).replacen("wss://", "https://", 1);
        let v: serde_json::Value = reqwest::Client::new().post(&http).json(&body).send().await.ok()?.json().await.ok()?;
        let s = v["result"].as_str()?.to_string();
        Some(subxt::utils::H256::from_slice(&hex::decode(s.trim_start_matches("0x")).ok()?))
    }.await.ok_or("no head hash")?;
    println!("head block {head:?}");
    let at = client.storage().at(head);

    for (p, e) in [
        ("GPUAttestation", "FreeEntryLimit"),
        ("GPUAttestation", "FreeEntryCount"),
        ("GPUPoW", "Difficulty"),
        ("GPUAttestation", "AttestationInterval"),
    ] {
        let v = value_at(&at, p, e).await;
        println!(
            "{p}.{e} = {}",
            v.as_deref().map(hex::encode).unwrap_or_else(|| "<none>".into())
        );
    }
    for (p, e) in [
        ("GPUAttestation", "FreeEntryAccounts"),
        ("GPUAttestation", "FreeEntryGpus"),
        ("GPUAttestation", "GpuOwner"),
        ("ComputeMarketplace", "RegisteredProviders"),
    ] {
        count_iter(&at, p, e).await;
    }

    // A raw read of an arbitrary account's RegisteredProviders entry, if given.
    if let Some(acct) = std::env::args().nth(2) {
        let q = subxt::dynamic::storage(
            "ComputeMarketplace",
            "RegisteredProviders",
            vec![Value::from_bytes(acct.as_bytes())],
        );
        println!("(skip raw key read for {acct}: needs 32-byte pubkey)");
        let _ = q;
    }
    Ok(())
}
