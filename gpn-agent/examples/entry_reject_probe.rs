//! entry_reject_probe — ask the live pool *why* it refuses an unsigned entry,
//! for a fingerprint that already consumed a free entry.
//!
//! Row 251's acceptance turns on which reason comes back for a second account
//! presenting a previously-used fingerprint: `Custom(6)` (rate limited) proves
//! the cooldown branch ran; `Custom(1)` (proof) proves the pallet fell back to
//! PoW verification; `Custom(4)` proves the ownership check caught it first.
//!
//! Run: `cargo run --release --example entry_reject_probe -- ws://127.0.0.1:9944 <fingerprint_hex>`

use gpn_agent::chain::signer::{self, GpnConfig};
use gpn_agent::onboard::{register_provider_entry_unsigned_call, EntryParams, EntryPow};
use subxt::tx::Payload;
use subxt::OnlineClient;

type Storage = subxt::storage::Storage<GpnConfig, OnlineClient<GpnConfig>>;

async fn at_best(
    client: &OnlineClient<GpnConfig>,
    http: &str,
) -> Result<Storage, Box<dyn std::error::Error>> {
    let body =
        serde_json::json!({"id":1,"jsonrpc":"2.0","method":"chain_getBlockHash","params":[]});
    let v: serde_json::Value = reqwest::Client::new()
        .post(http)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    let hash: subxt::utils::H256 = v["result"].as_str().unwrap_or_default().parse()?;
    Ok(client.storage().at(hash))
}

async fn get(
    at: &Storage,
    pallet: &str,
    entry: &str,
    params: Vec<subxt::dynamic::Value>,
) -> Option<Vec<u8>> {
    let q = subxt::dynamic::storage(pallet, entry, params);
    match at.fetch(&q).await {
        Ok(Some(v)) => Some(v.encoded().to_vec()),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let fp_hex = std::env::args().nth(2).expect("fingerprint hex");
    let gpu_info_hash: [u8; 32] = hex::decode(fp_hex.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| "fingerprint must be 32 bytes")?;
    let http = endpoint
        .replacen("ws://", "http://", 1)
        .replacen("wss://", "https://", 1);

    let client: OnlineClient<GpnConfig> = signer::connect(&endpoint).await?;

    let mut seed = Vec::new();
    seed.extend(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            .to_le_bytes(),
    );
    seed.extend(b"reject-probe");
    let account = sp_core::hashing::blake2_256(&seed);
    println!("fresh account    = 0x{}", hex::encode(account));
    println!("fingerprint      = 0x{}", hex::encode(gpu_info_hash));

    let mine = std::env::args().any(|a| a == "--mine");
    let bogus = if mine {
        // A *valid* proof against the live target: this separates "the pool
        // insists on a fresh proof" from "the pool did not see the cooldown".
        use gpn_agent::mining::search_solution;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let at = at_best(&client, &http).await?;
        let dbytes = get(&at, "GPUPoW", "Difficulty", vec![])
            .await
            .expect("GPUPoW.Difficulty");
        let target = sp_core::U256::from_little_endian(&dbytes);
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for t in 0..16u64 {
            let stop = stop.clone();
            handles.push(std::thread::spawn(move || {
                let mut nonce = t << 56;
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return None;
                    }
                    if let Some(seal) = search_solution(&target, nonce, 1 << 16, &stop) {
                        stop.store(true, Ordering::SeqCst);
                        return Some(seal);
                    }
                    nonce = nonce.wrapping_add(1 << 16);
                }
            }));
        }
        let mut seal = None;
        for h in handles {
            if let Ok(Some(s)) = h.join() {
                seal = Some(s);
            }
        }
        let seal = seal.expect("mined");
        println!("mined a valid proof: nonce={:#x}", seal.nonce);
        EntryPow::from_mined(&seal)
    } else {
        EntryPow {
            nonce: 0,
            mix_hash: [0u8; 32],
            solution_hash: [0u8; 32],
            algorithm_version: 0,
        }
    };
    let params = EntryParams {
        challenge_response: [9u8; 32],
        gpu_info_hash,
        gpu_count: 1,
        compute_gflops: 300,
        requirements: Default::default(),
    };
    let call = register_provider_entry_unsigned_call(&account, &bogus, &params);
    let unsigned = client.tx().create_unsigned(&call)?;
    let body = serde_json::json!({
        "id": 1, "jsonrpc": "2.0", "method": "author_submitExtrinsic",
        "params": [format!("0x{}", hex::encode(unsigned.encoded()))]
    });
    let v: serde_json::Value = reqwest::Client::new()
        .post(&http)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    println!("submit result    = {v}");
    let data = v["error"]["data"].as_str().unwrap_or("<none>");
    println!(
        "reason           = {data:?}  (1 proof / 2 fingerprint / 3 account used / 4 gpu used / 5 per-block cap / 6 rate limited)"
    );
    Ok(())
}
